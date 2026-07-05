use tracing::*;

use super::{Block, BlockHeader, Transaction, TransactionOutput};
use super::block::calculate_miner_fees_for_transactions;
use crate::crypto::PublicKey;
use crate::error::{BtcError, Result};
use crate::sha256::Hash;
use crate::util::{MerkleRoot, Saveable};
use crate::U256;
use bigdecimal::BigDecimal;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::io::{
    Error as IoError, ErrorKind as IoErrorKind, Read, Result as IoResult, Write,
};
use uuid::Uuid;

impl Saveable for Blockchain {
    fn load<I: Read>(reader: I) -> IoResult<Self> {
        ciborium::de::from_reader(reader).map_err(|_| {
            IoError::new(
                IoErrorKind::InvalidData,
                "Failed to deserialize Blockchain",
            )
        })
    }

    fn save<O: Write>(&self, writer: O) -> IoResult<()> {
        ciborium::ser::into_writer(self, writer).map_err(|_| {
            IoError::new(
                IoErrorKind::InvalidData,
                "Failed to serialize Blockchain",
            )
        })
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Blockchain {
    utxos: HashMap<Hash, (bool, TransactionOutput)>,
    target: U256,
    blocks: Vec<Block>,
    #[serde(default, skip_serializing)]
    mempool: Vec<(DateTime<Utc>, Transaction)>,
}

impl Blockchain {
    pub fn new() -> Self {
        Blockchain {
            utxos: HashMap::new(),
            blocks: vec![],
            target: crate::MIN_TARGET,
            mempool: vec![],
        }
    }

    pub fn mempool(&self) -> &[(DateTime<Utc>, Transaction)] {
        &self.mempool
    }


    pub fn calculate_block_reward(&self) -> u64 {
        let block_height = self.block_height();
        let halvings = block_height / crate::HALVING_INTERVAL;
        (crate::INITIAL_REWARD * 10u64.pow(8)) >> halvings
    }

    pub fn create_block_template(&self, pubkey: PublicKey) -> Result<Block> {
        let mut transactions: Vec<Transaction> = self
            .mempool
            .iter()
            .take(crate::BLOCK_TRANSACTION_CAP)
            .map(|(_, tx)| tx.clone())
            .collect();

        let miner_fees = calculate_miner_fees_for_transactions(&transactions, &self.utxos)?;
        let reward = self.calculate_block_reward();

        transactions.insert(
            0,
            Transaction {
                inputs: vec![],
                outputs: vec![TransactionOutput {
                    value: reward + miner_fees,
                    unique_id: Uuid::new_v4(),
                    pubkey,
                }],
            },
        );

        let merkle_root = MerkleRoot::calculate(&transactions);
        let prev_block_hash = self
            .blocks
            .last()
            .map(|last_block| last_block.hash())
            .unwrap_or(Hash::zero());

        Ok(Block::new(
            BlockHeader {
                timestamp: Utc::now(),
                prev_block_hash,
                nonce: 0,
                target: self.target,
                merkle_root,
            },
            transactions,
        ))
    }

    pub fn cleanup_mempool(&mut self) {
        let now = Utc::now();
        let mut utxo_hashes_to_unmark: Vec<Hash> = vec![];
        self.mempool.retain(|(timestamp, transaction)| {
            if (now - *timestamp)
                > chrono::Duration::seconds(crate::MAX_MEMPOOL_TRANSACTION_AGE as i64)
            {
                utxo_hashes_to_unmark.extend(
                    transaction
                        .inputs
                        .iter()
                        .map(|input| input.prev_transaction_output_hash),
                );
                false
            } else {
                true
            }
        });
        for hash in utxo_hashes_to_unmark {
            self.utxos
                .entry(hash)
                .and_modify(|(marked, _)| *marked = false);
        }
    }

    pub fn add_to_mempool(&mut self, transaction: Transaction) -> Result<()> {
        self.cleanup_mempool();

        let mut known_inputs = HashSet::new();

        for input in &transaction.inputs {
            if !self.utxos.contains_key(&input.prev_transaction_output_hash) {
                return Err(BtcError::InvalidTransaction);
            }
            if known_inputs.contains(&input.prev_transaction_output_hash) {
                return Err(BtcError::InvalidTransaction);
            }
            known_inputs.insert(input.prev_transaction_output_hash);
        }

        let new_fee = self.transaction_fee(&transaction)?;

        let mut conflicting_indices = HashSet::new();
        for input in &transaction.inputs {
            if let Some((true, _)) = self.utxos.get(&input.prev_transaction_output_hash) {
                if let Some(idx) =
                    self.find_mempool_index_spending(input.prev_transaction_output_hash)
                {
                    let old_fee = self.transaction_fee(&self.mempool[idx].1)?;
                    if new_fee <= old_fee {
                        return Err(BtcError::InvalidTransaction);
                    }
                    conflicting_indices.insert(idx);
                } else {
                    self.utxos
                        .entry(input.prev_transaction_output_hash)
                        .and_modify(|(marked, _)| *marked = false);
                }
            }
        }

        let mut to_remove: Vec<usize> = conflicting_indices.into_iter().collect();
        to_remove.sort_unstable_by(|a, b| b.cmp(a));
        for idx in to_remove {
            let (_, removed) = self.mempool.remove(idx);
            self.unmark_transaction_utxos(&removed);
        }

        let all_inputs: u64 = transaction
            .inputs
            .iter()
            .map(|input| {
                self.utxos
                    .get(&input.prev_transaction_output_hash)
                    .expect("BUG: impossible")
                    .1
                    .value
            })
            .sum();
        let all_outputs: u64 = transaction.outputs.iter().map(|output| output.value).sum();
        if all_inputs < all_outputs {
            return Err(BtcError::InvalidTransaction);
        }

        self.mark_transaction_utxos(&transaction);
        self.mempool.push((Utc::now(), transaction));

        let mut order: Vec<usize> = (0..self.mempool.len()).collect();
        order.sort_by(|&i, &j| {
            self.transaction_fee(&self.mempool[j].1)
                .unwrap_or(0)
                .cmp(&self.transaction_fee(&self.mempool[i].1).unwrap_or(0))
        });
        let sorted = order.into_iter().map(|i| self.mempool[i].clone()).collect();
        self.mempool = sorted;

        Ok(())
    }

    fn transaction_fee(&self, transaction: &Transaction) -> Result<u64> {
        let input_value: u64 = transaction
            .inputs
            .iter()
            .map(|input| {
                self.utxos
                    .get(&input.prev_transaction_output_hash)
                    .expect("BUG: impossible")
                    .1
                    .value
            })
            .sum();
        let output_value: u64 = transaction.outputs.iter().map(|output| output.value).sum();
        if input_value < output_value {
            return Err(BtcError::InvalidTransaction);
        }
        Ok(input_value - output_value)
    }

    fn find_mempool_index_spending(&self, utxo_hash: Hash) -> Option<usize> {
        self.mempool.iter().enumerate().find_map(|(idx, (_, transaction))| {
            transaction
                .inputs
                .iter()
                .any(|input| input.prev_transaction_output_hash == utxo_hash)
                .then_some(idx)
        })
    }

    fn mark_transaction_utxos(&mut self, transaction: &Transaction) {
        for input in &transaction.inputs {
            if let Some((marked, _)) = self.utxos.get_mut(&input.prev_transaction_output_hash) {
                *marked = true;
            }
        }
    }

    fn unmark_transaction_utxos(&mut self, transaction: &Transaction) {
        for input in &transaction.inputs {
            if let Some((marked, _)) = self.utxos.get_mut(&input.prev_transaction_output_hash) {
                *marked = false;
            }
        }
    }

    pub fn block_height(&self) -> u64 {
        self.blocks.len() as u64
    }

    pub fn utxos(&self) -> &HashMap<Hash, (bool, TransactionOutput)> {
        &self.utxos
    }

    pub fn target(&self) -> U256 {
        self.target
    }

    pub fn blocks(&self) -> impl Iterator<Item = &Block> {
        self.blocks.iter()
    }

    pub fn add_block(&mut self, block: Block) -> Result<()> {
        if self.blocks.is_empty() {
            if block.header.prev_block_hash != Hash::zero() {
                println!("zero hash");
                return Err(BtcError::InvalidBlock);
            }
        } else {
            let last_block = self.blocks.last().unwrap();
            if block.header.prev_block_hash != last_block.hash() {
                println!("prev hash is wrong");
                return Err(BtcError::InvalidBlock);
            }

            if !block.header.hash().matches_target(block.header.target) {
                println!("does not match target; the blocks hash is less than the target");
                return Err(BtcError::InvalidBlock);
            }

            let calculated_merkle_root = MerkleRoot::calculate(&block.transactions);
            if calculated_merkle_root != block.header.merkle_root {
                println!("invalid merkle root");
                return Err(BtcError::InvalidMerkleRoot);
            }

            if block.header.timestamp <= last_block.header.timestamp {
                return Err(BtcError::InvalidBlock);
            }

            block.verify_transactions(self.block_height(), self.utxos())?;
        }

        self.cleanup_mempool();

        let block_transactions: HashSet<_> = block.transactions.iter().map(|tx| tx.hash()).collect();
        self.mempool
            .retain(|(_, tx)| !block_transactions.contains(&tx.hash()));

        self.blocks.push(block);
        self.rebuild_utxos();
        let pending: Vec<(DateTime<Utc>, Transaction)> = self.mempool.clone();
        for (_, tx) in pending {
            self.mark_transaction_utxos(&tx);
        }
        self.try_adjust_target();
        Ok(())
    }

    pub fn try_adjust_target(&mut self) {
        if self.blocks.is_empty() {
            return;
        }

        if self.block_height() % crate::DIFFICULTY_UPDATE_INTERVAL != 0 {
            return;
        }

        let interval = crate::DIFFICULTY_UPDATE_INTERVAL as usize;
        let start_time = self.blocks[self.block_height() as usize - interval].header.timestamp;
        let end_time = self.blocks.last().unwrap().header.timestamp;
        let time_diff = end_time - start_time;
        let time_diff_seconds = time_diff.num_seconds().max(1) as u64;
        let target_seconds = crate::IDEAL_BLOCK_TIME * crate::DIFFICULTY_UPDATE_INTERVAL;

        let new_target = BigDecimal::parse_bytes(self.target.to_string().as_bytes(), 10)
            .expect("BUG: impossible")
            * (BigDecimal::from(time_diff_seconds) / BigDecimal::from(target_seconds));

        let new_target_str = new_target
            .to_string()
            .split('.')
            .next()
            .expect("BUG: Expected a decimal point")
            .to_owned();

        let mut new_target =
            U256::from_str_radix(&new_target_str, 10).expect("BUG: impossible");

        if new_target < self.target / 4 {
            new_target = self.target / 4;
        } else if new_target > self.target * 4 {
            new_target = self.target * 4;
        }

        self.target = new_target.min(crate::MIN_TARGET);
    }

    pub fn rebuild_utxos(&mut self) {
        self.utxos.clear();
        for block in &self.blocks {
            for transaction in &block.transactions {
                for input in &transaction.inputs {
                    self.utxos
                        .remove(&input.prev_transaction_output_hash);
                }

                for output in &transaction.outputs {
                    self.utxos
                        .insert(output.hash(), (false, output.clone()));
                }
            }
        }
    }
}

