use tracing::*;

use anyhow::Result;
use argh::FromArgs;
use btclib::store::BlockStore;
use btclib::types::Blockchain;
use dashmap::DashMap;
use static_init::dynamic;
use std::sync::OnceLock;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::RwLock;

mod ban;
mod handler;
mod time_sync;
mod util;

#[dynamic]
pub static BLOCKCHAIN: RwLock<Blockchain> = RwLock::new(Blockchain::new());

#[dynamic]
pub static NODES: DashMap<String, TcpStream> = DashMap::new();

// Initialized once at startup with the CLI-provided path, then read from
// everywhere else. A plain OnceLock (rather than the #[dynamic] statics
// above) because its value depends on a runtime argument, not just a
// no-argument constructor.
pub static BLOCK_STORE: OnceLock<BlockStore> = OnceLock::new();

#[derive(FromArgs)]
/// toy blockchain node
struct Args {
    #[argh(option, default = "9000")]
    /// port number
    port: u16,
    #[argh(option, default = "String::from(\"./blockchain.redb\")")]
    /// path to the durable block store (a redb database file)
    blockchain_file: String,
    #[argh(positional)]
    /// addresses of initial nodes
    nodes: Vec<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args: Args = argh::from_env();
    let port = args.port;
    let blockchain_file = args.blockchain_file;
    let nodes = args.nodes;

    let store = BlockStore::open_or_create(&blockchain_file)?;
    let existing_chain = store.get_active_chain()?;
    BLOCK_STORE
        .set(store)
        .unwrap_or_else(|_| panic!("BLOCK_STORE initialized twice"));
    let store = BLOCK_STORE.get().expect("just initialized it above");
    ban::load_persisted(store);

    if !existing_chain.is_empty() {
        println!(
            "found {} blocks in the local store, loading...",
            existing_chain.len()
        );
        util::load_from_store(store, &existing_chain).await?;
        println!("blockchain loaded from store");
    } else {
        // Every node derives the canonical genesis locally rather than
        // asking a peer for it -- a fresh node should never have to trust
        // whichever peer happens to answer first for something this
        // foundational. Only blocks AFTER genesis are worth syncing.
        println!("no local blockchain found, establishing the canonical genesis...");
        {
            let mut blockchain = BLOCKCHAIN.write().await;
            blockchain
                .add_block(Blockchain::genesis_block().clone())
                .expect("BUG: the canonical genesis must always be accepted by a fresh chain");
        }
        util::persist_chain_state(store, Blockchain::genesis_block()).await?;
        println!("genesis established");
    }

    // Whether we just established genesis or loaded existing history from
    // disk, always try to catch up on anything peers have beyond what
    // we've got -- a node that was offline for a while (or just started
    // fresh) shouldn't sit at a stale tip forever waiting for someone else
    // to dial in first.
    println!("trying to sync any further blocks from peers...");
    util::populate_connections(&nodes).await?;
    println!("total amount of known nodes: {}", NODES.len());
    if nodes.is_empty() {
        println!("no initial nodes provided, starting as a seed node");
    } else {
        let (best_name, best_height) = util::find_best_chain_node().await?;
        if best_name.is_empty() {
            println!("no usable peer found to sync from, continuing with what we have");
        } else {
            util::download_blockchain(store, &best_name, best_height).await?;
            println!("blockchain downloaded from {}", best_name);
        }
    }

    let addr = format!("0.0.0.0:{}", port);
    let listener = TcpListener::bind(&addr).await?;
    println!("Listening on {}", addr);

    tokio::spawn(util::cleanup());

    loop {
        let (socket, addr) = listener.accept().await?;
        if ban::is_banned(addr.ip()) {
            println!("rejecting connection from banned peer {addr}");
            continue;
        }
        tokio::spawn(handler::handle_connection(socket));
    }
}

