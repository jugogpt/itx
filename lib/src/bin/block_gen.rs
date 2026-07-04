use btclib::crypto::PrivateKey;
use btclib::sha256::Hash;
use btclib::types::{Block, BlockHeader, Transaction, TransactionOutput};
use btclib::util::{MerkleRoot, Saveable};
use chrono::Utc;
use uuid::Uuid;
use std::env;
use std::process::exit;

fn main() {
    println!("Hello from block generator!");

    let path = if let Some(arg) = env::args().nth(1) {
        arg
    } else {
        eprintln!("Usage: block_gen <block_file>");
        exit(1);
    };
    let private_key = PrivateKey::new_key();
    let transactions = vec![Transaction::new(
        vec![],
        vec![TransactionOutput {
            unique_id: Uuid::new_v4(),
            value: btclib::INITIAL_REWARD * 10u64.pow(8),
            pubkey: private_key.public_key(),
        }],
    )];
    let merkle_root = MerkleRoot::calculate(&transactions);
    let block = Block::new( //here we create the gensis block for the new blockchain 
        BlockHeader::new(
            Utc::now(),
            0, //the nonce is 0 at the gensis block
            Hash::zero(), //at the gensis block there is zero hash 
            merkle_root,
            btclib::MIN_TARGET, //the target starts at the minimum for the gensis block
        ),
        transactions,
    );
    block.save_to_file(path).expect("Failed to save block"); //


}