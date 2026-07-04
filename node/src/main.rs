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
}

//we need to populate the NODES DashMap (essentially an async version of a HashMap data structure)
// we need
pub async fn populate_connections(nodes: &[String]) -> Result<()> {
    println!("tryinig to connect to other nodes")
    for node in nodes {
        println!("connecting to {}", node);
        let mut stream = TcpStream::connect(&node).await?;
        //above we open a new TcpStream connection to every node, 
        // send it a DiscoverNodes message, which will make it reeturn a list of nodes in the NodeList message
        let message = Message::DiscoverNodes;
        message.send_async(&mut stream).await?;
        println!("sent Discoverndoes to {}", node);
        
        let message = Message::receive_async(&mut stream).await?;
        match message {
            Message::NodeList(child_nodes) => {
                println!("received NodeList from {}", node);
                for child_node in child_nodes {
                    println!("adding node {}", child_node);
                    //and then we opepn a connection through every child node.
                    
                    let new_stream  TcpStream::connect(&child_node).await?;
                    //all of thes nodes are added one by one to the NODES dashmap
                    crate::NODES.insert(child_node, new_stream);
                }
            }

            _ => {
                println!("unexpected message from {}", node);
            }
        }
        crate::NODES.insert(node.clone(), stream);
    }
    Ok(())

}


pub async fn find_longest_chain_node() -> Result<(String, u32)> {
    println!("finding node with the highest blockchain length...");
    let mut longest_name = String::new();
    let longest_count = 0;
    let all_nodes = crate::NODES.iter().map(|x| x.key().clone()).collect::<Vec<_>>();
    for node in all_nodes {
        println!("asking {} for blockchain length", node);
        let mut stream = crate::NODES.get_mut(&node).context("no node")?;
    }
}