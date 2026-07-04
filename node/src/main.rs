use argh::FromArgs;
use dashmap::DashMap;
use static_init::dynamic;
use anyhow::Result;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::RwLock;
use btclib::types::Blockchain;
use std::path::Path;

mod handler;
mod util;



#[derive(FromArgs)]
//toy blockchain node
struct Args {
    #[args(option, default = "9000")]
    // port number 
    port: u16
    #[argh(
        option,
        default = "String::from(\"./blockchain.cbor\")"
    )]
    //blockhcain file location 
    blockchain_file: String,
    #[argh(positional)]
    //addresses of inital nodes 
    nodes: Vec<String>,
}


#[tokio::main]
async fn main() -> Result<()>{
    //Parse command line arguments
    let args: Args = argh::from_env();
    // our node expects three inputs: (1) a port to listen to (2) a path to store/load the blockchain from (3) a list of other nodes to connect to an communicate with
    let port = args.port;
    let blockchain_file = args.blockchain_file;
    let nodes = args.nodes;

    
}
