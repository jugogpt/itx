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

pub async fn download_blockchain(store: &BlockStore, node: &str, count: u32) -> Result<()> {
    // Start from whatever height we're already at (at minimum, our own
    // locally-established genesis) rather than always from 0 -- otherwise
    // we'd re-fetch genesis itself from the peer and reject it as a
    // "competing" one, since it wouldn't extend our current tip.
    let start = BLOCKCHAIN.read().await.block_height() as usize;
    let mut stream = NODES.get_mut(node).unwrap();
    for i in start..count as usize {
        let message = Message::FetchBlock(i);
        message.send_async(&mut *stream).await?;
        let message = Message::receive_async(&mut *stream).await?;
        match message {
            Message::NewBlock(block) => {
                let result = {
                    let mut blockchain = BLOCKCHAIN.write().await;
                    blockchain.add_block(block.clone())
                };
                match result {
                    Ok(()) => {
                        persist_chain_state(store, &block).await?;
                    }
                    // Not the peer's fault -- our own chain state can
                    // shift between the AskChainTip that picked this peer
                    // and a later FetchBlock in the same session. Abort
                    // this sync attempt without punishing them for it.
                    Err(btclib::error::BtcError::OrphanBlock) => {
                        anyhow::bail!(
                            "peer {node} sent a block that doesn't connect to our chain during sync"
                        );
                    }
                    // A real content-validation failure. Same category of
                    // error handler.rs treats leniently (a strike, not an
                    // immediate ban) -- an occasional bad block here isn't
                    // necessarily malicious either.
                    Err(e) => {
                        if let Some(ip) = addr_ip(node) {
                            crate::ban::strike(ip, false);
                        }
                        return Err(e).context("peer sent an invalid block during sync");
                    }
                }
            }
            _ => {
                println!("unexpected message from {}", node);
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
