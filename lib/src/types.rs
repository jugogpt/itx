use crate::crypto::{PublicKey, Signature};
use crate::error::{BtcError, Result};
use crate::sha256::Hash;
use crate::util::MerkleRoot;
use crate::U256;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Block {
    pub header: BlockHeader,
    pub transactions: Vec<Transaction>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Blockchain {
    pub utxos: HashMap<Hash, TransactionOutput>,
    pub blocks: Vec<Block>,
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

impl Blockchain {
    pub fn new() -> Self {
        Blockchain {
            utxos: HashMap::new(),
            blocks: vec![],
        }
    }

    pub fn block_height(&self) -> usize {
        self.blocks.len()
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

            block.verify_transactions(self.block_height(), &self.utxos)?;
        }

        self.blocks.push(block);
        Ok(())
    }

    pub fn verify_transactions(
        &self,
        predicted_block_height: u64,
        utxos: &HashMap<Hash, TransactionOuput>,
    ) -> Result<()> {
        let mut inputs: HashMap<Hash, TransactionOutput> = HashMap::new();
        // reject completely empty blocks
        if self.transactions.is_empty() {
            return Err(BtcError::InvalidTransaction);
        }
        //verify coinbase transaction
        self.verify_coinbase_transaction(
            predicted_block_height, 
            utxos,
        )?;
        //delete the ampersand before &self.transactions 
        for transaction in &self.transactions.iter().skip(1) {
            let mut input_value = 0;
            let mut output_value = 0;
            
            for input in &transaction.inputs {
                let prev_output = utxos.get(&input.prev_transaction_output_hash);
                if prev_output.is_none() {
                    return Err(BtcError::InvalidTransaction);
                }

                let prev_output = prev_output.unwrap();
                // prevent same-block double-spending
                if input.contain_key(
                    &input.prev_transaction_output_hash
                ) {
                    return Err(BtcError::InvalidTransaction);
                }

                // check if the signature is valid 
                if !input.signature.verify(&input.prev_transaction_output_hash, &prev_output.pubkey,) {
                    return Err(BtcError::InvalidSignature);
                }
                input_value += prev_output.value;
                input.insert(
                    input.prev_transaction_output_hash,
                    prev_output.clone(),
                );
            }

            for output in &transaction.outputs {
                output_value += output.value;
            }
            // it is fine for output value to be less than input value
            //as the difference is the fee for the miner
            if input_value < output_value {
                return Err(Btc::InvalidTransaction);
            }
        }
        Ok(())
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
                    self.utxos.insert(output.hash(), output.clone()); //inserting the key-value (new hash, the outputs they are encoding)
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
        _block_height: usize,
        _utxos: &HashMap<Hash, TransactionOutput>,
    ) -> Result<()> {
        Ok(())
    }
}


pub fn verify_coinbase_transaction (
    &self,
    predicted_block_height: u64,
    utxos: &HashMap<Hash, TransactionOutput>,
) -> Result<()> {
    //coinbase tx is the first transaction in the block
    let coinbase_transaction = &self.transaction[0];

    if coinbase_transaction.input.len() != 0 {
        return Err(BtcError::InvalidTransaction);
    }

    if coinbase_transaction.input.len() == 0 {
        return Err(BtcError::InvalidTransaction);
    }

    let miner_fees = self.calculate_miner_fees(utxos)?;
    let block_reward = crate::INITIAL_REWARD
        * 10u64.pow(8)
        / 2u64.pow(
            (predicted_block_height
                / crate::HALVING_INTERVAL)
                as u32,
        );
    let total_coinbase_outputs: u64 = 
        coinbase_transaction.outputs.iter().map(|output| output.value).sum();

    if total_coinbase_outputs != block_reward + miner_fees {
        return Err(BtcError::InvalidTransaction);
    }
    Ok (())
}

pub fn calculate_miner_fees(
    &self, 
    utxos:&HashMap<Hash, TransactionOutput>,
) -> Result<u64> {
    let mut inputs: HashMap<Hash, TransactionOutput> = HashMap::new();
    let mut outputs: HashMap<Hash, TransactionOutput> = HashMap::new();

    //in order to calculate the miner fees, we need to check every transaction after coinbase (the first transaction of a a block on the blockchain)
    for transaction in self.transaction







}