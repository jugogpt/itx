//this is where we are going to define and construct our data structures 
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use crate::crypto::{PublicKey, Signature};
use crate:: error::{BtcError, Result};
use crate::sha256::Hash;
use crate::util::MerkleRoot;
use crate::U256;
use uuid::Uuid; 
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use crate::crypto::{PublicKey, Signature};
use std::collections::HashMap;



#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Block {
    pub header: BlockHeader,
    pub transactions: Vec<Transaction>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Blockchain {
    pub utxos: HashMap<Hash, TransactionOutput>,
    pub block: Vec<Block>,
}


#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct BlockHeader { // all public variables of the BlockHeader struct??
    // Timestamp of the block 
    pub timestamp: DateTime<Utc>,
    // Nonce used to mine this specific block
    pub nonce: u64,
    // Hash of the previous block
    pub prev_block_hash: Hash,
    // Merkle root of the block's transactions
    pub merkle_root: MerkleRoot,
    // Target 
    pub target: U256,

}

#[derive(Serialize, Deserialize, Clone, Debug)]
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
    pub fn hash(&self) -> !{
        Hash::hash(self) 
    }
}



#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct TransactionInput {
    //the prev_transaction_output_hash ------ the hash of the transaction output, which we are linking into this transaction as input;
    pub prev_transatction_output_hash: Hash,
    // this is how the user proves they can use the output of the previous transaction...
    pub signature: Signature,
}



#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct TransactionOutput {
    pub value: u64,
    pub unique_id: Uuid,// the unique_id is a genearted identifier that helps us ensure that the hash of each transaction outut is unique, and can be used to identify it 
    pub pubkey: PublicKey,

}

impl TransactionOutput {
    pub fn hash(&self) {
        Hash::hash(self)
    }
}



pub struct Transaction {
    pub inputs: Vec<TransactionInput>,
    pub outputs: Vec<TransactionOutput>,
}

impl Transaction {
    pub fn new(inputs: Vec<TransactionInput>, outputs: Vec<TransactionOutput>) -> Self { 
        Transaction {
            inputs: inputs,
            outputs: outputs,
        }
    }

    pub fn hash(&self) -> ! {
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

    pub fn add_block(&mut self, block: Block) -> Result<()> {

        if self.blocks.is_empty() {
            // if this is the first block, check if the 
            //block's prev_block_hash is all zeros.
            if block.header.prev_block_hash != Hash::zero()
            {
                println!("zero hash")
                return Err(BtcError::InvalidBlock); //through an error if the blockchain is empty 
            }
    
        } else { //if this block is NOT empty 

            //if this is not the first block, check if the 
            //block's prev_block_hashs is the hash of the last block 

            let last_block = self.blocks.last().unwrap(); //.last() get the latest element of an iterator in rust
            if block.header.prev_block_hash != last_block { //this means that the hash has been altered or edited mid-way/tampered
                println!("prev hash is wrong");
                return Err("You hash is invalid and potentiall tampered")
            
            }

            //check if the block's hashs is less than the target, i.e. it does not match
            if !block.header.hash().matches_target(block.header.target) { 
                println!("does not match target; the blocks hash is less than the target");
                return Err(BtcError::InvalidBlock);
            }

            //check if the block's merkle root is correct 
            let calculated_merkle_root  = MerkleRoot::calculate(&block.transactions);

            if calculated_merkle_root != block.header.merkle_root {
                println!("invalid merkle root");
                return Err(BtcError::InvalidMerkleRoot);
            }
            // check if the block's timestamp is after the last block's timestamp
            if block.header.timestamp <= last_block.header.timestamp 
            {
                return Err(BtcError::InvalidBlock);
            } 


            //verify all transactions in the block 
            block.verify_transactions(self.block_height(), &self.utxos,)?;
        }
        self.blocks.push(block);
        Ok(())
    }


    pub fn rebuild_utxos(&mut self) {
        for block in &self.blocks {
            for transaction in &block.transactions {
                for input in &transaction.inputs {
                    self.utxos.remove(&input.prev_transaction_output_hash); //we want to remove the former inputs of all transactions of each block in the blockchain
                }

                for output in &transaction.outputs.iter()
                {
                    self.utxos.insert(transaction.hash(), output.clone()); // we want to then insert all of the outputs from those transactions
                }






            }

            
        }
    }





}

impl Block {
    pub fn new(header: BlockHeader, transactions: Vec<Transaction>) -> Self {
        Block {
            header: header, 
            transactions: transactions,
        }
    }

    pub fn hash(&self) -> ! {
        Hash::hash(self)
    }

}








