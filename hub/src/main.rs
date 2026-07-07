use tracing::*;

mod auth;
mod board;
mod handlers;
mod node_client;
mod store;

use anyhow::Result;
use argh::FromArgs;
use axum::routing::{get, post};
use axum::Router;
use board::TaskBoard;
use btclib::crypto::{PrivateKey, PublicKey};
use btclib::util::Saveable;
use node_client::NodeClient;
use std::sync::Arc;
use store::HubStore;
use tokio::sync::RwLock;
use tokio::time::{interval, Duration};

/// Shared state handed to every HTTP handler. `board` is the only piece
/// that changes after startup, so it's the only field behind a lock --
/// `store`/`node` are internally either lock-free (redb) or open a fresh
/// connection per call (see `NodeClient`), and the operator keys never
/// change for the process's lifetime.
pub struct AppState {
    pub board: RwLock<TaskBoard>,
    pub store: HubStore,
    pub node: NodeClient,
    pub operator_private_key: PrivateKey,
    pub operator_public_key: PublicKey,
}

#[derive(FromArgs)]
/// itx agent hub -- HTTP API for posting/claiming tasks, faucet grants, and
/// agent reputation, backed by an itx blockchain node.
struct Args {
    #[argh(option, default = "9100")]
    /// port to listen on
    port: u16,
    #[argh(option, default = "String::from(\"127.0.0.1:9000\")")]
    /// address of the blockchain node to talk to
    node_address: String,
    #[argh(option, default = "String::from(\"./hub.redb\")")]
    /// path to the hub's durable store (a redb database file)
    store_file: String,
    #[argh(option, default = "String::from(\"./hub_operator.priv.cbor\")")]
    /// path to the operator's private key (generated on first run if missing)
    operator_key_file: String,
}

fn load_or_create_operator_key(path: &str) -> Result<PrivateKey> {
    match PrivateKey::load_from_file(path) {
        Ok(key) => Ok(key),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            println!("no operator key found at {path}, generating a new one...");
            let key = PrivateKey::new_key();
            key.save_to_file(path)?;
            Ok(key)
        }
        Err(e) => Err(e.into()),
    }
}

/// Periodically reopens abandoned claims and sweeps the auth replay guard.
/// Runs for the lifetime of the process.
async fn sweep_loop(state: Arc<AppState>) {
    let mut ticker = interval(Duration::from_secs(60));
    loop {
        ticker.tick().await;

        let reopened = {
            let mut board = state.board.write().await;
            board.expire_claims(chrono::Utc::now())
        };
        for task_id in reopened {
            let task = state.board.read().await.get_task(task_id).cloned();
            if let Some(task) = task {
                if let Err(e) = state.store.save_task(&task) {
                    println!("failed to persist expired-claim task {task_id}: {e}");
                }
            }
        }

        auth::cleanup_replay_guard();
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let args: Args = argh::from_env();

    let operator_private_key = load_or_create_operator_key(&args.operator_key_file)?;
    let operator_public_key = operator_private_key.public_key();
    println!("================================================================");
    println!("hub operator address -- fund this so the hub can pay out tasks/faucet grants:");
    println!("{operator_public_key}");
    println!("================================================================");

    let store = HubStore::open_or_create(&args.store_file)?;
    let mut board = TaskBoard::new();
    for task in store.load_all_tasks()? {
        board.restore_task(task);
    }
    for (pubkey, reputation) in store.load_all_reputation()? {
        board.restore_reputation(pubkey, reputation);
    }
    for pubkey in store.load_all_faucet_grants()? {
        board.restore_faucet_grant(pubkey);
    }
    println!(
        "loaded {} task(s), {} reputation record(s), {} faucet grant(s) from store",
        board.all_tasks().count(),
        board.all_reputation().count(),
        board.all_faucet_grants().count()
    );

    let state = Arc::new(AppState {
        board: RwLock::new(board),
        store,
        node: NodeClient::new(args.node_address),
        operator_private_key,
        operator_public_key,
    });

    tokio::spawn(sweep_loop(state.clone()));

    let app = Router::new()
        .route("/tasks", get(handlers::list_tasks).post(handlers::create_task))
        .route("/tasks/:id", get(handlers::get_task))
        .route("/tasks/:id/claim", post(handlers::claim_task))
        .route("/tasks/:id/submit", post(handlers::submit_task))
        .route("/faucet", post(handlers::faucet_claim))
        .route("/reputation/:pubkey", get(handlers::get_reputation))
        .route("/leaderboard", get(handlers::leaderboard))
        .route("/llms.txt", get(handlers::llms_txt))
        .with_state(state);

    let addr = format!("0.0.0.0:{}", args.port);
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    println!("hub listening on {addr}");
    axum::serve(listener, app).await?;
    Ok(())
}
