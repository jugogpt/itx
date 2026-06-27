use crate::crypto::{PublicKey, Signature};
use crate::error::{BtcError, Result};
use crate::sha256::Hash;
use crate::util::MerkleRoot;
use crate::U256;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use bigdecimal::BigDecimal;
use std::collections::{HashMap, HashSet};
use uuid::Uuid;


#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Block {
    pub header: BlockHeader,
    pub transactions: Vec<Transaction>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct BlockHeader {
    pub timestamp: DateTime<Utc>,
    pub nonce: u64,
    pub prev_block_hash: Hash,
    pub merkle_root: MerkleRoot,
    pub target: U256,
}

impl BlockHeader {
    pub fn new(
        timestamp: DateTime<Utc>,
        nonce: u64,
        prev_block_hash: Hash,
        merkle_root: MerkleRoot,
        target: U256,
    ) -> Self {
        BlockHeader {
            timestamp,
            nonce,
            prev_block_hash,
            merkle_root,
            target,
        }
    }

    pub fn hash(&self) -> Hash {
        Hash::hash(self)
    }

    pub fn mine(&mut self, steps:usize) -> bool {


        // the reason why we only do a finite number of steps at a time is because we may want to interrupt the mining if we receive an update froom the network that 
        //that we should work on a neww block (because a new block has been found in the meantime)


        //if the block already matches target, return early 
        if self.hash().matches_target(self.target) {
            return true; //this means that we already mined or don't need to mine
        }

        for _ in 0..steps {
            if let Some(new_nonce) = self.nonce.checked_add(1)
            {
                self.nonce = new_nonce;
            } else {
                self.nonce = 0;
                self.timestamp = Utc::now()
            }
            if self.hash().matches_target(self.target) {
                return true;
            }
        }
        false

    }


}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct TransactionInput {
    pub prev_transaction_output_hash: Hash,
    pub signature: Signature,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct TransactionOutput {
    pub value: u64,
    pub unique_id: Uuid,
    pub pubkey: PublicKey,
}

impl TransactionOutput {
    pub fn hash(&self) -> Hash {
        Hash::hash(self)
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Transaction {
    pub inputs: Vec<TransactionInput>,
    pub outputs: Vec<TransactionOutput>,
}

impl Transaction {
    pub fn new(inputs: Vec<TransactionInput>, outputs: Vec<TransactionOutput>) -> Self {
        Transaction { inputs, outputs }
    }

    pub fn hash(&self) -> Hash {
        Hash::hash(self)
    }
}


#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Blockchain {
    utxos: HashMap<Hash, (bool,TransactionOutput)>,
    target: U256,
    blocks: Vec<Block>,
    #[serde(default, skip_serializing)]
    mempool: Vec<(DateTime<Utc>, Transaction)>,
}



impl Blockchain{
    //the natrue of bitconi is that we have e a netowrk of nodes that share information between each other in order to maintain a single ssource truth -- the blochain itself
    //curntly the methods we ahve created on our Blockchain type are only enough if we ever plan to have just one node, which defeats the purose of a blockchain
    // We need to adda coupel of methods that will help us share the blockchain
    // first, we want to imporvoe our desing by making our fields private and create methods that expose them


    pub fn new() -> Self {
        Blockchain {
            utxos: HashMap::new(),
            blocks: vec![],
            target: crate::MIN_TARGET,
            mempool: vec![],
        }
    }


    // mempool
    pub fn mempool(&self) -> &[(DateTime<Utc>, Transaction)] {
        &self.mempool
    }

    pub fn cleanup_mempool(&mut self) {
        let now = Utc::now();
        let mut utxo_hashes_to_unmark: Vec<Hash> = vec![];
        self.mempool.retain(|(timestamp, transaction)| {
            if now - *timestamp
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
                    // orphaned mark: no mempool tx references this UTXO
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



    //block height 
    pub fn block_height(&self) -> u64 {
        self.blocks.len() as u64
    }

   // utxos
    pub fn utxos(&self) -> &HashMap<Hash, (bool, TransactionOutput)> {
        &self.utxos
    }
    // target
    pub fn target(&self) -> U256 {
        self.target
    }
    // blocks
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

        //remvoe transactions from mempool that are now in the block

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

        // multiply the current target by actual time divided by ideal time
        let new_target = BigDecimal::parse_bytes(self.target.to_string().as_bytes(), 10)
            .expect("BUG: impossible")
            * (BigDecimal::from(time_diff_seconds) / BigDecimal::from(target_seconds));

        // cut off decimal point and everything after it from string representation
        let new_target_str = new_target
            .to_string()
            .split('.')
            .next()
            .expect("BUG: Expected a decimal point")
            .to_owned();

        let mut new_target =
            U256::from_str_radix(&new_target_str, 10).expect("BUG: impossible");

        // clamp to at most 4x easier or 4x harder than the current target; this is to prevent runaway loops that make mining impossibly hard or very very easy
        if new_target < self.target / 4 {
            new_target = self.target / 4;
        } else if new_target > self.target * 4 {
            new_target = self.target * 4;
        }

        // cap at the easiest allowed difficulty
        self.target = new_target.min(crate::MIN_TARGET);
    }





    pub fn rebuild_utxos(&mut self) {
        self.utxos.clear();
        //probably redo this code to make it more performant, this is an On**3 operation 
        for block in &self.blocks {
            for transaction in &block.transactions {
                for input in &transaction.inputs {
                    self.utxos
                        .remove(&input.prev_transaction_output_hash); //removing the past hashes
                }

                for output in &transaction.outputs {
                    self.utxos
                        .insert(output.hash(), (false, output.clone()));
                }
            }
        }
    }
}

impl Block {
    pub fn new(header: BlockHeader, transactions: Vec<Transaction>) -> Self {
        Block {
            header,
            transactions,
        }
    }

    pub fn hash(&self) -> Hash {
        Hash::hash(self)
    }

    pub fn verify_transactions(
        &self,
        predicted_block_height: u64,
        utxos: &HashMap<Hash, (bool, TransactionOutput)>,
    ) -> Result<()> {
        let mut inputs: HashMap<Hash, TransactionOutput> = HashMap::new();

        if self.transactions.is_empty() {
            return Err(BtcError::InvalidTransaction);
        }

        self.verify_coinbase_transaction(predicted_block_height, utxos)?;

        for transaction in self.transactions.iter().skip(1) {
            let mut input_value = 0;
            let mut output_value = 0;

            for input in &transaction.inputs {
                let prev_output = utxos
                    .get(&input.prev_transaction_output_hash)
                    .map(|(_, output)| output);

                if prev_output.is_none() {
                    return Err(BtcError::InvalidTransaction);
                }

                let prev_output = prev_output.unwrap();

                if inputs.contains_key(&input.prev_transaction_output_hash) {
                    return Err(BtcError::InvalidTransaction);
                }

                if !input
                    .signature
                    .verify(&prev_output.hash(), &prev_output.pubkey)
                {
                    return Err(BtcError::InvalidSignature);
                }

                input_value += prev_output.value;
                inputs.insert(
                    input.prev_transaction_output_hash,
                    prev_output.clone(),
                );
            }

            for output in &transaction.outputs {
                output_value += output.value;
            }

            if input_value < output_value {
                return Err(BtcError::InvalidTransaction);
            }
        }

        Ok(())
    }

    pub fn verify_coinbase_transaction(
        &self,
        predicted_block_height: u64,
        utxos: &HashMap<Hash, (bool, TransactionOutput)>,
    ) -> Result<()> {
        let coinbase_transaction = &self.transactions[0];

        if !coinbase_transaction.inputs.is_empty() {
            return Err(BtcError::InvalidTransaction);
        }

        let miner_fees = self.calculate_miner_fees(utxos)?;
        let block_reward = crate::INITIAL_REWARD
            * 10u64.pow(8)
            / 2u64.pow((predicted_block_height / crate::HALVING_INTERVAL) as u32);
        let total_coinbase_outputs: u64 = coinbase_transaction
            .outputs
            .iter()
            .map(|output| output.value)
            .sum();

        if total_coinbase_outputs != block_reward + miner_fees {
            return Err(BtcError::InvalidTransaction);
        }

        Ok(())
    }

    pub fn calculate_miner_fees(
        &self,
        utxos: &HashMap<Hash, (bool, TransactionOutput)>,
    ) -> Result<u64> {
        let mut total_fees = 0u64;

        for transaction in self.transactions.iter().skip(1) {
            let mut input_value = 0;
            let mut output_value = 0;

            for input in &transaction.inputs {
                let prev_output = utxos
                    .get(&input.prev_transaction_output_hash)
                    .map(|(_, output)| output);

                if prev_output.is_none() {
                    return Err(BtcError::InvalidTransaction);
                }

                input_value += prev_output.unwrap().value;
            }

            for output in &transaction.outputs {
                output_value += output.value;
            }

            if input_value < output_value {
                return Err(BtcError::InvalidTransaction);
            }

            total_fees += input_value - output_value;
        }

        Ok(total_fees)
    }
}