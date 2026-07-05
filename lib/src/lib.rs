use tracing::*;

pub mod crypto;
pub mod error;
pub mod network;
pub mod sha256;
pub mod types;
pub mod util;

use serde::{Deserialize, Serialize};
use uint::construct_uint;

construct_uint! {
    // construct an unsigned 256-bit integer consisting of 4 x 64-bit words
    #[derive(Serialize, Deserialize)]
    pub struct U256(4);
}

// initial reward in bitcoin - multiply by 10*S*8 to get satoshis
pub const INITIAL_REWARD: u64 = 50;
// halving interval in blocks
pub const HALVING_INTERVAL: u64 = 210;
// ideal block time in second
pub const IDEAL_BLOCK_TIME: u64 = 10;
// minimum target
pub const MIN_TARGET: U256 = U256([
    0xFFFF_FFFF_FFFF_FFFF,
    0xFFFF_FFFF_FFFF_FFFF,
    0xFFFF_FFFF_FFFF_FFFF,
    0x0000_FFFF_FFFF_FFFF,
]);
//difficulty update interval in blocks
pub const DIFFICULTY_UPDATE_INTERVAL: u64 = 50;

// maximum time a transaction may stay in the mempool
pub const MAX_MEMPOOL_TRANSACTION_AGE: u64 = 600;

//DEF: the difficulty is how unlikely, roughly, it should be to encounter the correct hash while a node is mining

pub const BLOCK_TRANSACTION_CAP: usize = 20;
