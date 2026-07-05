use anyhow::Result;
use argh::FromArgs;
use btclib::types::Blockchain;
use dashmap::DashMap;
use static_init::dynamic;
use std::path::Path;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::RwLock;

mod handler;
mod util;

#[dynamic]
pub static BLOCKCHAIN: RwLock<Blockchain> = RwLock::new(Blockchain::new());

#[dynamic]
pub static NODES: DashMap<String, TcpStream> = DashMap::new();

#[derive(FromArgs)]
/// toy blockchain node
struct Args {
    #[argh(option, default = "9000")]
    /// port number
    port: u16,
    #[argh(option, default = "String::from(\"./blockchain.cbor\")")]
    /// blockchain file location
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

    if Path::new(&blockchain_file).exists() {
        util::load_blockchain(&blockchain_file).await?;
    } else {
        println!("blockchain file does not exist!");
        util::populate_connections(&nodes).await?;
        println!("total amount of known nodes: {}", NODES.len());
        if nodes.is_empty() {
            println!("no initial nodes provided, starting as a seed node");
        } else {
            let (longest_name, longest_count) = util::find_longest_chain_node().await?;
            util::download_blockchain(&longest_name, longest_count).await?;
            println!("blockchain downloaded from {}", longest_name);
            {
                let mut blockchain = BLOCKCHAIN.write().await;
                blockchain.rebuild_utxos();
            }
            {
                let mut blockchain = BLOCKCHAIN.write().await;
                blockchain.try_adjust_target();
            }
        }
    }

    let addr = format!("0.0.0.0:{}", port);
    let listener = TcpListener::bind(&addr).await?;
    println!("Listening on {}", addr);

    tokio::spawn(util::cleanup());
    tokio::spawn(util::save(blockchain_file.clone()));

    loop {
        let (socket, _) = listener.accept().await?;
        tokio::spawn(handler::handle_connection(socket));
    }
}
