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
    mempool: Vec<Transaction>,
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
    pub fn mempool(&self) -> &[Transaction] {
        &self.mempool
    }


    pub fn add_to_mempool(&mut self, transaction: Transaction) -> Result<()> {
        //NEW: validate each transction before insertion
        // all inputs must match knwon UTXOs, and must be unique


        //verify that our inputted input hash is one that we know exists
        let mut known_inputs = HashSet::new();



        //populate the known_inputs hashset 
        for input in &transaction.inputs {
            if !self.utxos.contains_key(&input.prev_transaction_output_hash) { //the exclaimation mark means that we are chekcing if utxos does NOT contain one of the inputs in the passed Transaction transactiono
                return Err(BtcError::InvalidTransaction);
            }
            if known_inputs.contains(&input.prev_transaction_output_hash) { //why would the 
                return Err(BtcError::InvalidTransaction);
            }
            //if all valid, insert the transaction input into known_inputs

            known_inputs.insert(input.prev_transaction_output_hash);
        }




        // all inputs must be lower than all outputs
        let all_inputs = transaction
           .inputs
           .iter()
           .map(|input| {
            self.utxos
                   .get(
                       &input.prev_transaction_output_hash,
                   )
                   .expect("BUG: impossible").1  //the .1 just gets the second value of the the hash, being the TransactionOutput
                   .value
           }).sum::<u64>();
            let all_outputs = transaction.outputs.iter().map(|output| output.value).sum();
            if all_inputs < all_outputs {
                return Err(BtcError::InvalidTransaction);   
            }


        self.mempool.push(transaction);  //add the parameter transaction to the list that is the mempool of needed transactions

        //sort by miner fee
        self.mempool.sort_by_key(|transaction| {
            let all_inputs = transaction.inputs.iter().map(|input| {
                self.utxos.get(&input.prev_transaction_output_hash).expect("BUG: impossible").1.value
            }).sum::<u64>();


            let all_output: u64 = transaction.outputs.iter().map(|output| output.value).sum();

            let miner_fee = all_inputs - all_outputs;
            miner_fee 

        });

        Ok(())
        
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

        let block_transactions: HashSet<_> = block.transactions.iter().map(|tx| tx.hash()).collect();
        self.mempool.retain(|tx| !block_transactions.contains(&tx.hash()));
    
        self.blocks.push(block);
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