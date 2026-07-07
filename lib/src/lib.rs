use tracing::*;

pub mod crypto;
pub mod error;
pub mod network;
pub mod payment;
pub mod sha256;
pub mod store;
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

// maximum serialized size (in bytes) of the transactions included in a
// mined block, not counting the coinbase transaction. Chosen to mirror
// Bitcoin's original 1MB block size limit.
pub const BLOCK_BYTE_CAP: usize = 1_000_000;

// magic bytes exchanged during the peer handshake so nodes/miners/wallets
// refuse to talk to something that isn't speaking this protocol
pub const PROTOCOL_MAGIC: u32 = 0x49_54_58_00;
// bump this whenever the wire protocol changes in an incompatible way.
// v2: Hello/HelloAck gained a timestamp field for network time-offset
// sampling.
pub const PROTOCOL_VERSION: u32 = 2;

// upper bound on a single length-prefixed wire message. Well above
// BLOCK_BYTE_CAP to leave headroom for e.g. large UTXO-set responses,
// while still rejecting a peer that sends a bogus multi-gigabyte length
// prefix before we ever allocate a buffer for it.
pub const MAX_MESSAGE_SIZE: usize = 10 * 1024 * 1024;

// how far ahead of our own clock a block's timestamp is allowed to be
// before we consider it invalid. Mirrors Bitcoin's 2-hour rule; without
// some bound, a block claiming to be from the year 3000 would be accepted
// as long as it's otherwise valid, corrupting future difficulty-adjustment
// math (which relies on timestamps being roughly honest).
pub const MAX_FUTURE_BLOCK_DRIFT_SECONDS: i64 = 2 * 60 * 60;

// how many blocks deep a side branch's fork point must be, relative to
// the current tip, before we consider a reorg back to it realistically
// impossible and prune it from memory/storage. Twice the difficulty
// retarget interval, so pruning never runs ahead of what a single
// retarget window could plausibly still reorganize.
pub const SIDE_BRANCH_PRUNE_DEPTH: u64 = 2 * DIFFICULTY_UPDATE_INTERVAL;

/// Converts a PoW target into the amount of expected work required to
/// find a hash meeting it, so that chains can be compared by cumulative
/// work rather than simply by block count (a chain with more, easier
/// blocks is not necessarily "more work" than one with fewer, harder
/// blocks). Mirrors Bitcoin Core's GetBlockProof.
pub fn work_from_target(target: U256) -> U256 {
    let max = U256::max_value();
    // work = 2^256 / (target + 1), computed without overflowing U256 by
    // using (~target) = (2^256 - 1 - target) in place of (2^256 - target)
    (max - target) / (target + 1) + 1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn harder_target_means_more_work() {
        let easy = MIN_TARGET;
        let hard = MIN_TARGET / 100;
        assert!(work_from_target(hard) > work_from_target(easy));
    }

    #[test]
    fn min_target_has_positive_work() {
        assert!(work_from_target(MIN_TARGET) > U256::zero());
    }
}
