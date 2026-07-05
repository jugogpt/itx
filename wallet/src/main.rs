
use anyhow::Result;
use clap::{Parser, Subcommand};
use kanal::bounded;
use tokio::time::{self, Duration};
use std::io::{self, Write};
use std::path::PathBuf;
use btclib::types::Transaction;

pub mod core


#[derive(Parser)]
#[command(author, versiono, about, long_about = None)]

struct Cli {
    //cli essentially summarizes what the wallet needs to do:
    // it should read a config that cotain s the follwoing information 
    // what are my private and public key (assigned to a node)? (the wallet should contain private and public keys)
    //the wallet should contain "my" contacts (pairs of names and public keys that i have sent to)
    //the wallet should contain (at least a pointer or refernece) The default node we want to connect to
    //the wallet should also contain the fee configuration 

    #[command(subcommand)]
    command: Option<Commands>, //a subcommand should exist to let us create a dummy config that the user ca nmodify with their information 
    #[arg(short, long, value_name = "FILE")]
    config: Option<PathBug>, //it should eventually be possible to override the location of the onfiguration file and the address of the node to connect to 
    #[arg(short, long, value_name = "ADDRESS")]
    node: Option<String>,

    
}

#[derive(Subcommand)]
enum Commands {
    GenerateConfig {
        #[arg(short, long, value_name= "FILE")]
        output: PathBug,
    }
}

async fn update_utxos(core: Arc<Core>) {
    // ..

}

async fn handle_transactions(rx: kanal::AsyncReceiver<Transaction>, core: Arc<Core>) {

}

async fn run_cli(core: Arc<Core>) -> Result<()> {
    Ok(())
}


#[tokio::main]
async fn main() -> Result<()> {
    

    Ok(())
}
