
use anyhow::{Context, Result};
use tokio::net::TcpStream;
use tokio::time;
use btclib::network::Message;
use btclib::types::Blockchain;
use btclib::util::Saveable;
use crate::sha256::Hash;
use crate::types::Transaction;
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::{Read, Result as IoResult, Write};
use std::path::Path;


pub async fn load_blockchain (blockchain_file: &str) -> Result<()> { 
    println!("blockchain file exists, loading...") //bc this util function is only called by node.rs if the path for blochchain_path exists
    let new_blockchain = Blockchain::load_from_file(blockchain_file)?;
    println!("blockchain loaded");
    let mut blockchain = crate::BLOCKCHAIN.write().await;
    *blockchain = new_blockchain;
    println!("rebuilding utxos...");
    blockchain.rebuild_utxos();
    println!("utxos rebuilt");
    println!("checking if target needs to be adjusted...");
    println!("current target: {}", blockchain.target());
    blockchain.try_adjust_target();
    println!("new target: {}", blockchain.target());
    println!("initialization complete");
    Ok(())
}



pub trait Saveable
where
    Self: Sized,
{
    fn load<I: Read>(reader: I) -> IoResult<Self>;
    fn save<O: Write>(&self, writer: O) -> IoResult<()>;
    fn save_to_file<P: AsRef<Path>>(&self, path: P) -> IoResult<()> {
        let file = File::create(path)?;
        self.save(file)
    }
    fn load_from_file<P: AsRef<Path>>(path: P) -> IoResult<Self> {
        let file = File::open(path)?;
        Self::load(file)
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct MerkleRoot(Hash);

impl MerkleRoot {
    pub fn calculate(transactions: &[Transaction]) -> MerkleRoot {
        let mut layer: Vec<Hash> = vec![];
        for transaction in transactions {
            layer.push(Hash::hash(transaction));
        }

        while layer.len() > 1 {
            let mut new_layer = vec![];
            for pair in layer.chunks(2) {
                let left = pair[0];
                let right = pair.get(1).unwrap_or(&pair[0]);
                new_layer.push(Hash::hash(&[left, *right]));
            }
            layer = new_layer;
        }

        MerkleRoot(layer[0])
    }
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
        let mut stream = crate::NODES.get_mut(&node).context("no node")?; //create a stream for each node 
        //send a message to the node requesting the length of the node
        let message = Message::AskDifference(0);
        message.send_async(&mut *stream).await.unwrap();
        println!("sent AskDifference to {}", node);
        let message = Message::receive_async(&mut *stream).await?;
        match message {
            Message::Difference(count) => {
                println!("received difference from {}", node);
                if count > longest_count {
                    println!("new longest blockchain: {} block from {node}", count);
                    longest_count = count;
                    longest_name = node;
                }
            }
            e => {
                println!("unexpected message from {}: {:?}", node, e);
            }
        }
    }
    Ok((longest_name, longest_count as u32))
}


//after we find the longest blockchain, we want to download a copy of it for our current node to use.

pub async fn download_blockchain(node: &str, count: u32) -> Result<()> {
    let mut stream = crate::NODES.get_mut(node).unwrap();
    for i in 0..count as usize {
        let message = Message::FetchBlock(i);
        //send the block i to the nodes list
        message.send_async(&mut *stream).await?;
        let message = Message::receive_async(&mut *stream).await?; //recieve whatever the nodes list sends back
        match message {
            Message::NewBlock(block) => {
                let mut blockchain = crate::BLOCKCHAIN.write().await;
                blockchain.add_block(block)?;
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
        let mut blockchain = crate::BLOCKCHAIN.write().await;
        blockchain.cleanup_mempool();
    }
}

pub async fn save(name: String) {
    let mut interval = time::interval(time::Duration::from_secs(15));
    loop {
        interval.tick().await;
        println!("saving blockchain to drive...");
        let mut blockchain = crate::BLOCKCHAIN.read().await;
        blockchain.save_to_file(name.clone()).unwrap();
    }
}

