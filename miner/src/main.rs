use tracing::*;

use anyhow::{anyhow, Result};
use btclib::crypto::PublicKey;
use btclib::network::Message;
use btclib::types::Block;
use btclib::util::Saveable;
use clap::Parser;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::thread;
use tokio::net::TcpStream;
use tokio::sync::Mutex;
use tokio::time::{interval, timeout, Duration};
use tracing_subscriber::prelude::*;

/// How many nonce attempts one `mine()` call makes -- also the size of
/// the per-thread nonce window `thread_start_nonce` partitions across
/// mining threads.
const STEPS_PER_CALL: usize = 2_000_000;
/// How long `submit_block` waits on any single node before giving up on
/// it and moving to the next -- so one unreachable node can't stall (or,
/// without a bound at all, hang forever on) submission to the rest.
const SUBMIT_TIMEOUT: Duration = Duration::from_secs(5);

/// The nonce a mining thread should start its search from, given its
/// index among `N` threads all mining the same cloned template in
/// parallel. Partitions each pass into per-thread windows -- thread `t`
/// searches `[t*steps, (t+1)*steps]` -- rather than every thread
/// redundantly searching the identical `[0, steps]` window `mine`'s own
/// "always start from self.nonce (0 for a fresh template), +1 per step"
/// behavior would otherwise produce if N threads ran the unmodified
/// single-thread logic. (Windows overlap by exactly one nonce at each
/// boundary, since `mine(steps)` checks `steps+1` values, not `steps` --
/// immaterial at a 1-in-2,000,000 scale, not worth engineering around.)
///
/// Deliberately NOT rolling/advancing across outer-loop passes: every
/// pass re-clones the same still-nonce-0 template and calls this again,
/// so thread `t` rescans the same window every pass until the template
/// actually changes -- exactly the property the single-threaded version
/// already had before this change (it only ever rescanned `[0, steps]`
/// until a new template arrived), just now running N-wide instead of
/// introducing a new class of staleness.
fn thread_start_nonce(thread_index: usize, steps: usize) -> u64 {
    thread_index as u64 * steps as u64
}

/// Repeatedly mines against whatever's currently in `template` using a
/// nonce window starting at `thread_start_nonce(thread_index, STEPS_PER_CALL)`,
/// sending anything found down `sender`. A free function (not a `Miner`
/// method) so it only depends on the shared mining state, not a live
/// network connection -- letting it be spawned and tested directly
/// without a real `Miner`/node round trip.
fn spawn_mining_thread(
    thread_index: usize,
    template: Arc<std::sync::Mutex<Option<Block>>>,
    mining: Arc<AtomicBool>,
    sender: flume::Sender<Block>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || loop {
        if mining.load(Ordering::Relaxed) {
            if let Some(mut block) = template.lock().unwrap().clone() {
                block.header.nonce = thread_start_nonce(thread_index, STEPS_PER_CALL);
                println!(
                    "Mining block with target: {} (thread {thread_index})",
                    block.header.target
                );
                if block.header.mine(STEPS_PER_CALL) {
                    println!("Block mined: {}", block.hash());
                    sender.send(block).expect("Failed to send mined block");
                    mining.store(false, Ordering::Relaxed);
                }
            }
        } else {
            // Genuinely idle (no template yet, or the last one's already
            // exhausted/submitted): sleep instead of busy-spinning via
            // `yield_now()`. One OS thread per logical core, all of them
            // spinning as fast as the scheduler allows, saturates every
            // core continuously even while doing no useful work -- which
            // starves this same process's own tokio runtime (the one
            // driving `run()`'s 5-second template-refresh timer and the
            // network I/O to actually fetch a new template) of scheduling
            // time. A tick that's ready but never promptly polled because
            // no worker thread can get scheduled might as well not have
            // fired; this is what actually made a fresh mempool
            // transaction sit unpicked-up for minutes, not just which
            // request `fetch_and_validate_template` happened to send.
            // 10ms caps the added latency to notice `mining` flip back to
            // true at a level that's negligible next to that.
            thread::sleep(Duration::from_millis(10));
        }
    })
}

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Cli {
    /// comma-separated node addresses. The first is used as the template
    /// source; every one of them (including the first) receives a
    /// submitted block once mined.
    #[arg(short, long, value_delimiter = ',')]
    addresses: Vec<String>,
    #[arg(short, long)]
    public_key_file: String,
}

struct Miner {
    public_key: PublicKey,
    streams: Vec<Mutex<TcpStream>>,
    current_template: Arc<std::sync::Mutex<Option<Block>>>,
    mining: Arc<AtomicBool>,
    mined_block_sender: flume::Sender<Block>,
    mined_block_receiver: flume::Receiver<Block>,
}

impl Miner {
    async fn connect_and_handshake(address: &str) -> Result<TcpStream> {
        let mut stream = TcpStream::connect(address).await?;
        btclib::network::perform_handshake_initiator(&mut stream)
            .await
            .map_err(|e| anyhow!("handshake with {} failed: {}", address, e))?;
        Ok(stream)
    }

    /// Connects to every address, in order. A failure on the first
    /// (primary, used as the template source) is fatal -- there's no
    /// point running with no template source at all. A failure on any
    /// other (secondary) address is logged and that address is simply
    /// dropped from the submission list -- the whole point of having
    /// several is redundancy, so one being briefly unreachable at
    /// startup shouldn't stop the miner from running against the rest.
    async fn new(addresses: Vec<String>, public_key: PublicKey) -> Result<Self> {
        if addresses.is_empty() {
            return Err(anyhow!("at least one node address is required"));
        }
        let mut streams = Vec::new();
        for (i, address) in addresses.iter().enumerate() {
            match Self::connect_and_handshake(address).await {
                Ok(stream) => streams.push(Mutex::new(stream)),
                Err(e) if i == 0 => {
                    return Err(anyhow!("failed to connect to primary node {address}: {e}"));
                }
                Err(e) => {
                    warn!("failed to connect to secondary node {address}, skipping it: {e}");
                }
            }
        }
        let (mined_block_sender, mined_block_receiver) = flume::unbounded();
        Ok(Self {
            public_key,
            streams,
            current_template: Arc::new(std::sync::Mutex::new(None)),
            mining: Arc::new(AtomicBool::new(false)),
            mined_block_sender,
            mined_block_receiver,
        })
    }

    async fn run(&self) -> Result<()> {
        let thread_count = thread::available_parallelism().map(|n| n.get()).unwrap_or(1);
        println!("starting {thread_count} mining thread(s)");
        for thread_index in 0..thread_count {
            spawn_mining_thread(
                thread_index,
                self.current_template.clone(),
                self.mining.clone(),
                self.mined_block_sender.clone(),
            );
        }
        let mut template_interval = interval(Duration::from_secs(5));
        loop {
            let receiver_clone = self.mined_block_receiver.clone();
            tokio::select! {
                _ = template_interval.tick() => {
                    self.fetch_template().await?;
                }
                Ok(mined_block) = receiver_clone.recv_async() => {
                    self.submit_block(mined_block).await;
                    // With N mining threads searching independent nonce
                    // windows on the same pass, more than one can
                    // legitimately find a valid nonce before either
                    // learns the template is stale (mine() can't be
                    // cancelled mid-call) -- discard anything else
                    // already queued rather than submitting it too.
                    while let Ok(stale) = self.mined_block_receiver.try_recv() {
                        println!("discarding a stale block found by another thread in the same pass: {}", stale.hash());
                    }
                }
            }
        }
    }

    /// Fetches a genuinely fresh template from the primary node and swaps
    /// it in, unconditionally -- called on every timer tick regardless of
    /// whether a mining pass is already in progress. This used to only
    /// happen while idle (a pass in progress got a cheap `ValidateTemplate`
    /// tip-match check instead), which meant a transaction relayed to the
    /// mempool while a pass was already underway had no way to be picked
    /// up until that pass either found a block or the tip moved out from
    /// under it -- unbounded in the worst case, and increasingly likely
    /// to actually bite as retargeting pushes real mining passes toward
    /// taking a non-trivial amount of time (that's the whole point of
    /// `IDEAL_BLOCK_TIME`). Refetching every tick bounds that wait to
    /// this interval instead. It also happens to close a related latent
    /// issue for free: each mining thread's nonce window is fixed and
    /// never advances within a single template (see `thread_start_nonce`'s
    /// own doc comment), so a template that never changed would have
    /// every thread fruitlessly rescanning the exact same nonces forever
    /// if no valid one existed in their windows -- a fresh template every
    /// few seconds (a new timestamp alone changes every thread's hash
    /// landscape over that same nonce range) means that's no longer a
    /// permanent stall, just a wasted pass.
    async fn fetch_template(&self) -> Result<()> {
        println!("Fetching new template");
        let message = Message::FetchTemplate(self.public_key.clone());
        // Template fetch/validate deliberately only ever talks to the
        // primary (index 0) node -- there's no multi-source-template
        // feature here, only multi-target submission (see `submit_block`).
        let mut stream_lock = self.streams[0].lock().await;
        message.send_async(&mut *stream_lock).await?;
        drop(stream_lock);

        let mut stream_lock = self.streams[0].lock().await;
        match Message::receive_async(&mut *stream_lock).await? {
            Message::Template(template) => {
                drop(stream_lock);
                println!(
                    "Received new template with target: {}",
                    template.header.target
                );
                *self.current_template.lock().unwrap() = Some(template);
                self.mining.store(true, Ordering::Relaxed);
                Ok(())
            }
            _ => Err(anyhow!("Unexpected message received when fetching template")),
        }
    }

    /// Submits `block` to every configured node, independently -- a dead
    /// or slow node must not stop (or, by propagating an error out of the
    /// `select!` arm that calls this, crash) submission to the rest,
    /// since the whole point of multiple targets is redundancy.
    async fn submit_block(&self, block: Block) {
        println!("submitting mined block to {} node(s)", self.streams.len());
        let message = Message::SubmitTemplate(block);
        for (i, stream) in self.streams.iter().enumerate() {
            let mut stream_lock = stream.lock().await;
            match timeout(SUBMIT_TIMEOUT, message.send_async(&mut *stream_lock)).await {
                Ok(Ok(())) => {}
                Ok(Err(e)) => warn!("failed to submit block to node {i}: {e}"),
                Err(_) => warn!("timed out submitting block to node {i}"),
            }
        }
        self.mining.store(false, Ordering::Relaxed);
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer())
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();
    let public_key = PublicKey::load_from_file(&cli.public_key_file)
        .map_err(|e| anyhow!("Error reading public key: {}", e))?;
    let miner = Miner::new(cli.addresses, public_key).await?;
    miner.run().await
}

#[cfg(test)]
mod tests {
    use super::*;
    use btclib::crypto::PrivateKey;
    use btclib::sha256::Hash;
    use btclib::types::{BlockHeader, Transaction, TransactionOutput};
    use btclib::util::MerkleRoot;
    use chrono::Utc;
    use tokio::net::TcpListener;
    use uuid::Uuid;

    fn dummy_block(target: btclib::U256) -> Block {
        let private_key = PrivateKey::new_key();
        let coinbase = Transaction::new(
            vec![],
            vec![TransactionOutput {
                value: 1,
                unique_id: Uuid::new_v4(),
                pubkey: private_key.public_key(),
            }],
        );
        let merkle_root = MerkleRoot::calculate(&[coinbase.clone()]);
        let header = BlockHeader::new(Utc::now(), 0, Hash::zero(), merkle_root, target);
        Block::new(header, vec![coinbase])
    }

    /// An address guaranteed to have nothing listening on it -- for
    /// tests that need a deterministically-unreachable node rather than
    /// racing against a real process's timing.
    async fn dead_address() -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        format!("127.0.0.1:{}", listener.local_addr().unwrap().port())
        // `listener` drops here, freeing the port right back up.
    }

    #[test]
    fn thread_start_nonce_partitions_into_per_thread_windows() {
        assert_eq!(thread_start_nonce(0, 1_000), 0);
        assert_eq!(thread_start_nonce(1, 1_000), 1_000);
        assert_eq!(thread_start_nonce(3, 1_000), 3_000);
    }

    #[tokio::test]
    async fn multiple_mining_threads_find_a_block_on_an_easy_target() {
        let template = dummy_block(btclib::MIN_TARGET);
        let shared_template = Arc::new(std::sync::Mutex::new(Some(template)));
        let mining = Arc::new(AtomicBool::new(true));
        let (sender, receiver) = flume::unbounded();

        let thread_count = 4;
        for thread_index in 0..thread_count {
            spawn_mining_thread(thread_index, shared_template.clone(), mining.clone(), sender.clone());
        }

        let mined = timeout(Duration::from_secs(30), receiver.recv_async())
            .await
            .expect("a block should be found well within the timeout on MIN_TARGET")
            .expect("channel should not have closed");

        assert!(mined.header.hash().matches_target(mined.header.target));

        // the winning nonce must fall inside SOME thread's assigned
        // window -- a sanity check that partitioning was actually
        // applied, without assuming which thread had to win
        let nonce = mined.header.nonce;
        let winning_thread = (0..thread_count).find(|&t| {
            let window_start = thread_start_nonce(t, STEPS_PER_CALL);
            nonce >= window_start && nonce <= window_start + STEPS_PER_CALL as u64
        });
        assert!(
            winning_thread.is_some(),
            "mined nonce {nonce} doesn't fall in any thread's assigned window"
        );
    }

    #[tokio::test]
    async fn miner_new_fails_if_the_primary_node_is_unreachable() {
        let dead = dead_address().await;
        let public_key = PrivateKey::new_key().public_key();

        let result = Miner::new(vec![dead], public_key).await;
        assert!(result.is_err(), "no template source at all must be fatal");
    }

    #[tokio::test]
    async fn miner_new_tolerates_an_unreachable_secondary_node() {
        let primary = FakeMinerPeer::spawn_healthy().await;
        let dead_secondary = dead_address().await;
        let public_key = PrivateKey::new_key().public_key();

        let miner = Miner::new(vec![primary.addr.clone(), dead_secondary], public_key)
            .await
            .unwrap();

        assert_eq!(miner.streams.len(), 1, "the dead secondary must be dropped, not fatal");
    }

    /// A minimal fake node/miner peer for exercising `Miner`'s connection
    /// and submission logic without a real `node` process. Mirrors
    /// `hub/src/main.rs`'s `FakeNode` pattern (real TCP, real handshake).
    struct FakeMinerPeer {
        addr: String,
        received: Arc<Mutex<Vec<Block>>>,
    }

    impl FakeMinerPeer {
        /// Completes the handshake, then records every `SubmitTemplate`
        /// block it receives.
        async fn spawn_healthy() -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = format!("127.0.0.1:{}", listener.local_addr().unwrap().port());
            let received = Arc::new(Mutex::new(Vec::new()));
            let received_for_task = received.clone();
            tokio::spawn(async move {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                if btclib::network::perform_handshake_acceptor(&mut socket)
                    .await
                    .is_err()
                {
                    return;
                }
                loop {
                    match Message::receive_async(&mut socket).await {
                        Ok(Message::SubmitTemplate(block)) => {
                            received_for_task.lock().await.push(block);
                        }
                        Ok(_) | Err(_) => return,
                    }
                }
            });
            FakeMinerPeer { addr, received }
        }

        /// `Message::send_async` doesn't wait for the peer to actually
        /// process what it sent (same fire-and-forget shape as
        /// `SubmitTransaction` elsewhere in this workspace) -- a caller
        /// observing `submit_block` return only knows the bytes reached
        /// the OS socket, not that this fake's own accept/receive task
        /// has gotten around to recording them yet. Polls briefly instead
        /// of asserting on `received` immediately.
        async fn wait_for_received_count(&self, expected: usize) -> Vec<Block> {
            for _ in 0..100 {
                let received = self.received.lock().await.clone();
                if received.len() >= expected {
                    return received;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            self.received.lock().await.clone()
        }

        /// Completes the handshake, then immediately drops the
        /// connection -- simulating a peer that was reachable when
        /// `Miner::new` connected but is gone by the time a block is
        /// actually submitted.
        async fn spawn_drops_after_handshake() -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = format!("127.0.0.1:{}", listener.local_addr().unwrap().port());
            tokio::spawn(async move {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                let _ = btclib::network::perform_handshake_acceptor(&mut socket).await;
                // `socket` drops here, closing the connection.
            });
            FakeMinerPeer {
                addr,
                received: Arc::new(Mutex::new(Vec::new())),
            }
        }
    }

    #[tokio::test]
    async fn submit_block_reaches_healthy_nodes_despite_one_dropping_out() {
        let healthy1 = FakeMinerPeer::spawn_healthy().await;
        let flaky = FakeMinerPeer::spawn_drops_after_handshake().await;
        let healthy2 = FakeMinerPeer::spawn_healthy().await;

        let public_key = PrivateKey::new_key().public_key();
        let miner = Miner::new(
            vec![healthy1.addr.clone(), flaky.addr.clone(), healthy2.addr.clone()],
            public_key,
        )
        .await
        .unwrap();
        assert_eq!(miner.streams.len(), 3, "all three connected fine at startup");

        // give the flaky fake a moment to actually close its side after
        // the handshake before we submit
        tokio::time::sleep(Duration::from_millis(100)).await;

        let block = dummy_block(btclib::MIN_TARGET);
        miner.submit_block(block.clone()).await; // must not hang or panic despite the dropped middle connection

        let r1 = healthy1.wait_for_received_count(1).await;
        let r2 = healthy2.wait_for_received_count(1).await;
        assert_eq!(r1.len(), 1);
        assert_eq!(r2.len(), 1);
        assert_eq!(r1[0].hash(), block.hash());
        assert_eq!(r2[0].hash(), block.hash());
    }
}

