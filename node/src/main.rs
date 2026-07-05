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

pub static BLOCKCHAIN: RwLock<Blockchain> = Rw::new(Blockchain::new());
// Node pool 
#[dynamic]
pub static NODES: DashMap<String, TcpStream> = DashMap::new();

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
    //extract these three inputs from the args struct that we use to carry them!
    let port = args.port;
    let blockchain_file = args.blockchain_file;
    let nodes = args.nodes;

    //before we do anything, we need to check if the blockcahin exists. if not, we will see if we have any other nodes ot connect to.
    // --> if we have NO other nodes to connect to, we will assume we are the seed node (genisis node some call it) and start a new blockchain

    if Path::new(&blockchain_file).exists() { //abstracting the loading of the blockchain to the util file to make sure this main function is not the size of asia
        util::load_blockchain(&blockchain_file).await?;
    } else { 
        println!("blockchain file does not exist!");
        util::populate_connections(&nodes).await?;
        println!("total amount of known nodes: {}", NODES.len());
        if nodes.is_empty() {
            println!("no intial ndoes provided, starting as a seed node");
        }else{ //if there are other nodes, then go to one of those (preferably the longest one)
            let (longest_name, longest_count) = util::find_longest_chain_node().await?;
            // request the blockchain from the node with the longest blockchain

            util::download_blockchain(
                &longest_name,
                longest_count,
            ).await?;
            println!("blockchain downloaded from {}", longest_name);
            // recalculate utxos
            {
                let mut blockchain = BLOCKCHAIN.write().await;
                blockchain.rebuild_utxos();
            }
            // try to adjust difficulty 
            {
                let mut blockchain = BLOCKCHAIN.write().await;
                blockchain.try_adjust_target();
            }
            
        }
    }
    Ok(())



    //Start the TCP listener on 0.0.0.0:port 
    let addr = format!("0.0.0.0:{}", port);
    let listener = TCPListener::bind(&addr).await?;
    println!("Listening on {}", addr);
    
    //start a task to periodically cleanup the mempool
    // normally, you would want to keep and join the handle 
    tokio::spawn(util::cleanup());
    //and a task to periodically save the blockchain'
    tokio::spawn(util::save(blockchain_file.clone()));


    loop {
        let (socket, _) = listener.accept().await?;
        tokio::spawn(handler::handle_connection(socket));
        // the handler::handle_conncection function is the heart of our application, it's where we will handle every possible message type: but b4 we implement it message type by message type, there are some automatic tasks we msut take care of first:

    }

    
}
