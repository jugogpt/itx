use tracing::*;

use anyhow::{Context, Result};
use btclib::network::Message;
use btclib::sha256::Hash;
use btclib::store::BlockStore;
use btclib::types::{Block, Blockchain};
use std::net::{IpAddr, SocketAddr};
use tokio::net::TcpStream;
use tokio::time;

use crate::{BLOCKCHAIN, NODES};

/// Best-effort extraction of the IP out of a "host:port" peer address, for
/// checking/recording bans. Addresses that don't parse as a plain
/// `ip:port` (e.g. a hostname) are simply not ban-checked.
fn addr_ip(addr: &str) -> Option<IpAddr> {
    addr.parse::<SocketAddr>().ok().map(|a| a.ip())
}

/// Sends `message` to every currently-known peer, logging (rather than
/// failing) on individual send errors so one dead peer doesn't stop the
/// broadcast from reaching the rest. Builds the message once and reuses it
/// for every peer instead of re-cloning its payload per recipient.
pub async fn broadcast(message: &Message) {
    let nodes = NODES.iter().map(|x| x.key().clone()).collect::<Vec<_>>();
    for node in nodes {
        if let Some(mut stream) = NODES.get_mut(&node) {
            if message.send_async(&mut *stream).await.is_err() {
                println!("failed to send message to {}", node);
            }
        }
    }
}

/// Replays every block in `chain` (genesis-to-tip order, as read from the
/// store) through `Blockchain::add_block`. This rebuilds all in-memory
/// state (utxos, target, mempool bookkeeping) incrementally and
/// re-validates the whole history in the process, instead of trusting a
/// single serialized blob the way the old flat-file loader did.
pub async fn load_from_store(store: &BlockStore, chain: &[Hash]) -> Result<()> {
    // A store predating genesis-pinning (or one from an incompatible
    // network/build) would otherwise fail deep inside add_block with a
    // generic InvalidBlock error that gives no hint what's actually wrong.
    if let Some(&first_hash) = chain.first() {
        if first_hash != Blockchain::genesis_hash() {
            anyhow::bail!(
                "the local store's first block doesn't match this build's canonical genesis -- \
                 it may be from an incompatible network or an older, pre-genesis-pinning build. \
                 Delete the store file and let this node resync from peers."
            );
        }
    }

    for (i, hash) in chain.iter().enumerate() {
        let block = store
            .get_block(hash)?
            .with_context(|| format!("store is missing block {i} of its own active chain"))?;
        let mut blockchain = BLOCKCHAIN.write().await;
        blockchain.add_block(block)?;
    }

    // Replaying the whole history can itself trigger pruning (it fires
    // every DIFFICULTY_UPDATE_INTERVAL blocks, same as normal operation);
    // without draining it here those hashes would never be deleted from
    // disk, since prunable_side_blocks only ever looks at in-memory state.
    let pruned = {
        let mut blockchain = BLOCKCHAIN.write().await;
        blockchain.take_pruned_hashes()
    };
    store.delete_blocks(&pruned)?;
    if !pruned.is_empty() {
        println!(
            "pruned {} abandoned side-branch block(s) from storage during replay",
            pruned.len()
        );
    }

    let blockchain = BLOCKCHAIN.read().await;
    println!(
        "loaded {} blocks, current target: {}",
        blockchain.block_height(),
        blockchain.target()
    );
    Ok(())
}

/// Persists a block that was just successfully applied and refreshes the
/// store's record of the active chain (cheap: just a list of hashes, not
/// the block contents). Called after every successful `add_block`, so a
/// crash can lose at most the single most recent block instead of
/// corrupting the whole chain the way periodically re-dumping one flat
/// file could.
///
/// Also deletes from the store whatever side-branch blocks the in-memory
/// chain just pruned (see `Blockchain::take_pruned_hashes`) -- otherwise
/// the store would keep every abandoned fork forever even though memory
/// has already let them go.
pub async fn persist_chain_state(store: &BlockStore, block: &Block) -> Result<()> {
    store.put_block(block)?;
    let (chain, pruned) = {
        let mut blockchain = BLOCKCHAIN.write().await;
        (
            blockchain.active_chain().to_vec(),
            blockchain.take_pruned_hashes(),
        )
    };
    store.set_active_chain(&chain)?;
    store.delete_blocks(&pruned)?;
    if !pruned.is_empty() {
        println!("pruned {} abandoned side-branch block(s) from storage", pruned.len());
    }
    Ok(())
}

pub async fn populate_connections(nodes: &[String]) -> Result<()> {
    println!("trying to connect to other nodes");
    for node in nodes {
        if addr_ip(node).is_some_and(crate::ban::is_banned) {
            println!("skipping banned node {}", node);
            continue;
        }

        println!("connecting to {}", node);
        let mut stream = TcpStream::connect(node).await?;
        let offset = btclib::network::perform_handshake_initiator(&mut stream)
            .await
            .context("handshake failed")?;
        if let Some(ip) = addr_ip(node) {
            crate::time_sync::record_sample(ip, offset).await;
        }
        let message = Message::DiscoverNodes;
        message.send_async(&mut stream).await?;
        println!("sent DiscoverNodes to {}", node);

        let message = Message::receive_async(&mut stream).await?;
        match message {
            Message::NodeList(child_nodes) => {
                println!("received NodeList from {}", node);
                for child_node in child_nodes {
                    if addr_ip(&child_node).is_some_and(crate::ban::is_banned) {
                        println!("skipping banned discovered node {}", child_node);
                        continue;
                    }
                    println!("adding node {}", child_node);
                    let mut new_stream = TcpStream::connect(&child_node).await?;
                    match btclib::network::perform_handshake_initiator(&mut new_stream).await {
                        Ok(offset) => {
                            if let Some(ip) = addr_ip(&child_node) {
                                crate::time_sync::record_sample(ip, offset).await;
                            }
                        }
                        Err(e) => {
                            println!("handshake with {} failed: {e}, skipping", child_node);
                            continue;
                        }
                    }
                    NODES.insert(child_node, new_stream);
                }
            }
            _ => {
                println!("unexpected message from {}", node);
            }
        }
        NODES.insert(node.clone(), stream);
    }
    Ok(())
}

pub async fn find_best_chain_node() -> Result<(String, u32)> {
    println!("finding node with the most cumulative proof-of-work...");
    let mut best_name = String::new();
    let mut best_height = 0u32;
    let mut best_work = btclib::U256::zero();
    let all_nodes = NODES.iter().map(|x| x.key().clone()).collect::<Vec<_>>();
    for node in all_nodes {
        println!("asking {} for its chain tip", node);
        let mut stream = NODES.get_mut(&node).context("no node")?;
        let message = Message::AskChainTip;
        message.send_async(&mut *stream).await.unwrap();
        println!("sent AskChainTip to {}", node);
        let message = Message::receive_async(&mut *stream).await?;
        match message {
            Message::ChainTip(height, work) => {
                println!(
                    "received chain tip from {}: height {height}, work {work}",
                    node
                );
                if work > best_work {
                    println!("new best chain: {} blocks, {work} work, from {node}", height);
                    best_work = work;
                    best_height = height;
                    best_name = node;
                }
            }
            e => {
                println!("unexpected message from {}: {:?}", node, e);
            }
        }
    }
    Ok((best_name, best_height))
}

/// Downloads and applies every block between our current height and
/// `target_height` (an absolute height, not a delta -- e.g. from
/// `find_best_chain_node`'s `AskChainTip` round trip), in batches of up to
/// `btclib::BLOCKS_PER_FETCH_BATCH` blocks per round trip via
/// `Message::FetchBlocks`/`Message::Blocks` rather than one `FetchBlock`
/// round trip per block.
pub async fn download_blockchain(store: &BlockStore, node: &str, target_height: u32) -> Result<()> {
    let mut stream = NODES.get_mut(node).context("no node")?;
    loop {
        // Re-read our height every pass (at minimum, our own
        // locally-established genesis) rather than assuming it only ever
        // advances by exactly one batch -- otherwise we'd re-fetch
        // already-applied blocks or, worse, blocks at/before genesis and
        // reject them as "competing" ones, since they wouldn't extend our
        // current tip.
        let start = BLOCKCHAIN.read().await.block_height() as usize;
        if start as u32 >= target_height {
            break;
        }
        let count = (target_height - start as u32).min(btclib::BLOCKS_PER_FETCH_BATCH as u32);
        let message = Message::FetchBlocks { start, count };
        message.send_async(&mut *stream).await?;
        let message = Message::receive_async(&mut *stream).await?;
        match message {
            Message::Blocks(blocks) => {
                if blocks.is_empty() {
                    // The peer has nothing more past `start`, whatever the
                    // reason -- stop instead of looping forever asking for
                    // blocks it's already told us it doesn't have.
                    break;
                }
                for block in blocks {
                    let result = {
                        let mut blockchain = BLOCKCHAIN.write().await;
                        blockchain.add_block(block.clone())
                    };
                    match result {
                        Ok(()) => {
                            persist_chain_state(store, &block).await?;
                        }
                        // Not the peer's fault -- our own chain state can
                        // shift between the AskChainTip that picked this
                        // peer and a later FetchBlocks in the same
                        // session. Abort this sync attempt without
                        // punishing them for it.
                        Err(btclib::error::BtcError::OrphanBlock) => {
                            anyhow::bail!(
                                "peer {node} sent a block that doesn't connect to our chain during sync"
                            );
                        }
                        // A real content-validation failure. Same category
                        // of error handler.rs treats leniently (a strike,
                        // not an immediate ban) -- an occasional bad block
                        // here isn't necessarily malicious either. Abort
                        // rather than process the rest of this batch.
                        Err(e) => {
                            if let Some(ip) = addr_ip(node) {
                                crate::ban::strike(ip, false);
                            }
                            return Err(e).context("peer sent an invalid block during sync");
                        }
                    }
                }
            }
            _ => {
                println!("unexpected message from {}, aborting sync", node);
                break;
            }
        }
    }
    Ok(())
}

pub async fn cleanup() {
    let mut interval = time::interval(time::Duration::from_secs(30));
    loop {
        interval.tick().await;
        println!("cleaning the mempool from old transactions");
        let mut blockchain = BLOCKCHAIN.write().await;
        blockchain.cleanup_mempool();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use btclib::crypto::PrivateKey;
    use btclib::types::{Block, BlockHeader, Transaction, TransactionOutput};
    use btclib::util::MerkleRoot;
    use btclib::U256;
    use chrono::Utc;
    use std::collections::VecDeque;
    use tokio::net::TcpListener;
    use uuid::Uuid;

    fn temp_store_path(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("itx_node_util_test_{name}_{}.redb", Uuid::new_v4()))
    }

    /// Mirrors `blockchain.rs`'s own private test helper of the same
    /// shape (not importable across the crate boundary) -- mines a valid
    /// single-coinbase child block extending `parent`.
    fn mined_child_block(parent: Hash, target: U256, reward: u64) -> Block {
        let private_key = PrivateKey::new_key();
        let coinbase = Transaction::new(
            vec![],
            vec![TransactionOutput {
                value: reward,
                unique_id: Uuid::new_v4(),
                pubkey: private_key.public_key(),
            }],
        );
        let merkle_root = MerkleRoot::calculate(&[coinbase.clone()]);
        let mut header = BlockHeader::new(Utc::now(), 0, parent, merkle_root, target);
        assert!(
            header.mine(1_000_000),
            "failed to find a valid nonce within the test mining budget"
        );
        Block::new(header, vec![coinbase])
    }

    /// Ensures `crate::BLOCKCHAIN` (a global shared by every test in this
    /// binary) has at least a genesis block. Caller must already hold
    /// `crate::TEST_GLOBAL_STATE_LOCK`. Returns the current height.
    async fn ensure_genesis() -> u64 {
        let mut blockchain = BLOCKCHAIN.write().await;
        if blockchain.block_height() == 0 {
            blockchain
                .add_block(Blockchain::genesis_block().clone())
                .expect("BUG: canonical genesis must always be accepted by a fresh chain");
        }
        blockchain.block_height()
    }

    /// A fake peer that replies to each `FetchBlocks` it receives with the
    /// next canned batch from `responses`, in order; once exhausted, every
    /// further request gets an empty `Blocks` (simulating "nothing more
    /// past here"). Good enough to drive `download_blockchain`'s client
    /// loop without a real second `node` process.
    struct FakeBlockServer {
        addr: String,
    }

    impl FakeBlockServer {
        async fn spawn(responses: Vec<Vec<Block>>) -> Self {
            let mut responses: VecDeque<Vec<Block>> = responses.into();
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = format!("127.0.0.1:{}", listener.local_addr().unwrap().port());
            tokio::spawn(async move {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                loop {
                    let message = match Message::receive_async(&mut socket).await {
                        Ok(m) => m,
                        Err(_) => return,
                    };
                    match message {
                        Message::FetchBlocks { .. } => {
                            let batch = responses.pop_front().unwrap_or_default();
                            if Message::Blocks(batch).send_async(&mut socket).await.is_err() {
                                return;
                            }
                        }
                        _ => return,
                    }
                }
            });
            FakeBlockServer { addr }
        }
    }

    /// Connects to `fake` and registers the connection in the real
    /// `crate::NODES` map under a fresh, unique key -- `download_blockchain`
    /// looks its target node up there, same as real peer connections
    /// established by `populate_connections`.
    async fn connect_and_register(fake: &FakeBlockServer) -> String {
        let stream = TcpStream::connect(&fake.addr).await.unwrap();
        let key = format!("fake-{}", Uuid::new_v4());
        NODES.insert(key.clone(), stream);
        key
    }

    #[tokio::test]
    async fn download_blockchain_batches_multiple_round_trips_until_target_height() {
        let _guard = crate::TEST_GLOBAL_STATE_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let base_height = ensure_genesis().await;

        // Two batches, the first deliberately shorter than the client
        // will ask for -- exercising the fix where "shorter than
        // requested" must NOT be read as "peer's tip reached."
        let (batch1, batch2) = {
            let blockchain = BLOCKCHAIN.read().await;
            let mut parent = blockchain.blocks().last().unwrap().hash();
            let target = blockchain.target();
            let reward = blockchain.calculate_block_reward();
            let mut make = |n: usize| {
                (0..n)
                    .map(|_| {
                        let b = mined_child_block(parent, target, reward);
                        parent = b.hash();
                        b
                    })
                    .collect::<Vec<_>>()
            };
            (make(2), make(1))
        };

        let target_height = base_height as u32 + 3;
        let fake = FakeBlockServer::spawn(vec![batch1, batch2]).await;
        let node_key = connect_and_register(&fake).await;
        let store_path = temp_store_path("multi_batch");
        let store = BlockStore::open_or_create(&store_path).unwrap();

        download_blockchain(&store, &node_key, target_height)
            .await
            .unwrap();

        assert_eq!(BLOCKCHAIN.read().await.block_height(), target_height as u64);

        NODES.remove(&node_key);
        std::fs::remove_file(&store_path).ok();
    }

    #[tokio::test]
    async fn download_blockchain_stops_on_an_empty_reply_even_if_target_not_yet_reached() {
        let _guard = crate::TEST_GLOBAL_STATE_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let base_height = ensure_genesis().await;

        let one_block = {
            let blockchain = BLOCKCHAIN.read().await;
            let parent = blockchain.blocks().last().unwrap().hash();
            vec![mined_child_block(parent, blockchain.target(), blockchain.calculate_block_reward())]
        };

        // Claim a target height far beyond what the fake peer will
        // actually supply (one batch of 1, then it goes empty) --
        // simulating a stale/optimistic target_height, e.g. a peer that
        // looked ahead at AskChainTip time but has since fallen behind.
        let target_height = base_height as u32 + 50;
        let fake = FakeBlockServer::spawn(vec![one_block]).await;
        let node_key = connect_and_register(&fake).await;
        let store_path = temp_store_path("stale_target");
        let store = BlockStore::open_or_create(&store_path).unwrap();

        download_blockchain(&store, &node_key, target_height)
            .await
            .unwrap();

        assert_eq!(
            BLOCKCHAIN.read().await.block_height(),
            base_height + 1,
            "must stop cleanly once the peer runs dry, not loop or error"
        );

        NODES.remove(&node_key);
        std::fs::remove_file(&store_path).ok();
    }

    #[tokio::test]
    async fn download_blockchain_aborts_on_an_invalid_block_but_keeps_earlier_progress_in_the_same_batch() {
        let _guard = crate::TEST_GLOBAL_STATE_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let base_height = ensure_genesis().await;

        // one genuinely valid block, followed by one with a bogus parent
        // hash (never connects to our chain) in the SAME batch reply
        let (valid_block, bad_block) = {
            let blockchain = BLOCKCHAIN.read().await;
            let parent = blockchain.blocks().last().unwrap().hash();
            let target = blockchain.target();
            let reward = blockchain.calculate_block_reward();
            let valid = mined_child_block(parent, target, reward);
            let bad = mined_child_block(Hash::hash_bytes(b"not a real parent"), target, reward);
            (valid, bad)
        };

        let target_height = base_height as u32 + 10;
        let fake = FakeBlockServer::spawn(vec![vec![valid_block.clone(), bad_block]]).await;
        let node_key = connect_and_register(&fake).await;
        let store_path = temp_store_path("invalid_mid_batch");
        let store = BlockStore::open_or_create(&store_path).unwrap();

        let result = download_blockchain(&store, &node_key, target_height).await;
        assert!(result.is_err(), "an invalid block in the batch must abort the sync");

        let blockchain = BLOCKCHAIN.read().await;
        assert_eq!(
            blockchain.block_height(),
            base_height + 1,
            "the valid block before the bad one must still have been applied"
        );
        assert_eq!(blockchain.blocks().last().unwrap().hash(), valid_block.hash());
        drop(blockchain);

        NODES.remove(&node_key);
        std::fs::remove_file(&store_path).ok();
    }
}
