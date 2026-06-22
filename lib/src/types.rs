//this is where we are going to define and construct our data structures 
use crate::U256;
use uuid::Uuid; 
use chrono::{DateTime, Utc};




pub struct Block {
    pub header: BlockHeader,
    pub transactions: Vec<Transaction>,
}

pub struct Blockchain {
    pub block: Vec<Block>,
}



pub struct BlockHeader { // all public variables of the BlockHeader struct??
    // Timestamp of the block 
    pub timestamp: DateTime<Utc>,
    // Nonce used to mine this specific block
    pub nonce: u64,
    // Hash of the previous block
    pub prev_block_hash: [u8; 32],
    // Merkle root of the block's transactions
    pub merkle_root:[u8; 32],
    // Target 
    pub target: U256,

}

impl BlockHeader {
    pub fn new(
        timestamp: DateTime<Utc>,
        nonce: u64,
        prev_block_hash: [u8: 32],
        merkle_root: [u8; 32],
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
        unimplemented
    }
}

pub struct TransactionInput {
    //the prev_transaction_output_hash ------ the hash of the transaction output, which we are linking into this transaction as input;
    pub prev_transatction_output_hash:[u8; 32],
    // this is how the user proves they can use the output of the previous transaction...
    pub signature: [u8; 64],
}
pub struct TransactionOutput {
    pub value: u64,
    pub unique_id: Uuid,// the unique_id is a genearted identifier that helps us ensure that the hash of each transaction outut is unique, and can be used to identify it 
    pub pubkey: [u8; 33],

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
        unimplemented!()
    }

}

impl Blockchain {
    pub fn new() -> Self {
        Blockchain { blocks: vec![] }
    }

    pub fn add_block(&mut self, block: Block) {
        self.blocks.push(block);
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
        unimplemented!() // 
    }

}








