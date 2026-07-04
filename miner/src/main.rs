use btclib::crypto::PublicKey;
use btclib::util::Saveable;
use std::env;
use std::process::exit;
use anyhow::{anyhow, Result};
use btclib::crypto::PublicKey;
use btclib::network::Message;
use btclib::types::Block;
use btclib::util::Saveable;
use clap::Parser;
use std::sync::{atomic::{AtomicBool, Ordering}, Arc};
use std::thread;
use tokio::net::TcpStream;
use tokio::sync::Mutex;
use tokio::time::{interval, Duration};



#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Cli {
    #[arg(short, long)]
    address: String,
    #[arg(short, long)]
    public_key_file: String,
}
struct Miner{
    public_key: PublicKey,
    stream: Mutex<TcpStream>, //we wrap the stream and the current_template in a Mutex because we want Miner to be safety accessible from multiple threads / tokio tasks
    current_template: Arc<std::sync::Mutex<Option<Block>>>,
    mining: Arc<AtomicBool>, //we use an atomic bool instead of a regular bool because then we do not need to make miner mutable, but can still update a bool in the same way as if it were a mutable class and mutable bool
    mined_block_sender: flume::Sender<Block>,
    mined_block_receiver: flume::Receiver<Block>,
}
impl Miner {
    async fn new(
        address: String,
        public_key: PublicKey,
    ) -> Result<Self> {
        let stream = TcpStream::connect(&address).await?;
        let (mined_block_sender, mined_block_receiver) = flume::unbounded();
        Ok(Self {
            public_key, 
            stream: Mutex::new(stream),
            current_template: Arc::new(std::sync::Mutex::new(
                None,
            )),
            mining: Arc::new(AtomicBool::new(false)),
            mined_block_sender,
            mined_block_receiver,
        })

    }
    async fn run(&self) -> Result<()> {
        //spawning the mining thread, and then we loop over a tokio select!() macro--> tokio's select!() macro allows conceurrnt waitiing on multiple asynchronous operations...
        //select!() macro allows ocncurrent waiting on multiple asynchronous operations, executing the branch of the first oepration that completes
        self.spawn_mining_thread();
        let mut template_interval = interval(Duration::from_secs(5));
        loop {
            let receiver_clone = self.mined_block_receiver.clone();
            tokio::select! {
                // we are waiting on two futures: (1) the first one is the ticking of the tokio Interval, every 5 seconds to be precise, which will fetch and/or validate the template (2) the second one is 
                //the watiing waiting to revieve mined blocks from the hardware threads, so that htey can bee submitted to the network.
                _ = template_interval.tick() => {
                    self.fetch_and_validate_template().await?;
                }
                Ok(mined_block) = receiver_clone.recv_async() => {
                    self.submit_block(mined_block).await?;
                }
            }
        }
    }
    fn spawn_mining_thread(&self) -> thread::JoinHandle<()> {
        //the creation of a mining thread to do the asynchronous mining:
        let template = self.current_template.clone();
        let mining  = self.mining.clone();
        let sender = self.mined_block_sender.clone();
        thread::spawn(move || loop {
            if mining.load(Ordering::Relaxed) {
                if let Some(mut block) = template.lock().unwrap().clone() {
                    println!("Mining block with target: {}" block.header.target);
                    if block.header.mine(2000000) { //this is the part that is actually doing the mining 

                        println!("Block mined: {}", block.hash());
                        sender.send(block).expect("Failed to send mined block");
                        mining.store(false, Ordering::Relaxed);
                    }
                }
            }
            thread::yield_now();
        })
        
    }
    async fn fetch_and_validate_template(&self) -> Result<()> {
        //if we are not mining -> fetch a template
        if !self.mining.load(Ordering::Relaxed) {
            //when working with atomics, we need too specify the ordering of atomic operations we would ike to use. This ranges from
            // Order::Relaxed (which roughly translates to "please just be atomic bro") or Ordering::SeqCst (which roughly translates to be "YOU SHALL NOT PASS")
            // all atomic operations before this one stay before it, all after it stay after it 
            //atomic operations guarantee the whole read-modify-write happens as one uninterruptible unit
            self.fetch_template().await?;
        } else { // if we are mining, we validate the current template
            self.validate_template().await?;
        }
        Ok(())
    }
    async fn fetch_template(&self) -> Result<()> {
        println!("Fetching new template");
        let message = Message::FetchTemplate(self.public_key.clone());
        let mut stream_lock = self.stream.lock().await;
        message.send_async(&mut *stream_lock).await?;
        drop(stream_lock);
        let mut stream_lock = self.stream.lock().await?;
        match Message::receive_async(&mut *stream_lock).await? {
            Message::Template(template) => {
                drop(stream_lock);
                println!("Received new template with target: {}", template.header.target);
                *self.current_template.lock().unwrap() = Some(template);
                self.mining.store(true, Ordering::Relaxed);
                Ok(())
            }

            _ => Err(anyhow!("Unexpected message receiveed when fetching template")),
        }
       
    }
    async fn validate_template(&self) -> Result<()> {
       
    }
    async fn submit_block(&self, block: Block) -> Result<()> {
        
    }
    
}


fn usage() -> ! {
    eprintln!("Usage: {} <address> <public_key_file>", env::args().next().unwrap());
    exit(1);
}

#[tokio::main]
async fn main() {
    let address = match env::args().nth(1) {
        Some(address) => address,
        None => usage(),
    };
    let public_key_file = match env::args().nth(2) {
        Some(public_key_file) => public_key_file,
        None => usage(),
    };
    let Ok(public_key) = PublicKey::load_from_file(&public_key_file) else {
        eprintln!("Error reading public key from file {}", public_key_file);
        exit(1);
    };
    println!("Connecting to {address} to mine with {public_key}");

    //here we connect to the node 
    let mut stream match TcpStream::connect(&address).await {
        Ok(stream) => stream,
        Err(e) => {
            eprint!("Failed to connect to server: {}", e);
            exit(1);
        }
    };

    // ask the node for work
    println!("requresting work from {address}");
    let message = Message::FetchTemplate(public_key);
    message.send(&mut stream);

    let miner = Miner::new(cli.address, public_key).await?;
    miner.run().await
}
