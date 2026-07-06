mod core;
mod tasks;
mod ui;
mod util;

use anyhow::Result;
use clap::{Parser, Subcommand};
use core::Core;
use cursive::views::TextContent;
use kanal;
use std::path::PathBuf;
use std::sync::Arc;
use tasks::{handle_transactions, ui_task, update_balance, update_utxos};
use tracing::*;
use util::{big_mode_btc, generate_dummy_config, setup_panic_hook, setup_tracing};

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
    #[arg(short, long, value_name = "FILE", default_value_os_t = PathBuf::from("wallet_config.toml"))]
    config: PathBuf,
    #[arg(short, long, value_name = "ADDRESS")]
    node: Option<String>,
}

#[derive(Subcommand)]
enum Commands {
    GenerateConfig {
        #[arg(short, long, value_name = "FILE", default_value_os_t = PathBuf::from("wallet_config.toml"))]
        output: PathBuf,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    setup_tracing()?;
    setup_panic_hook();
    info!("Starting wallet application");

    let cli = Cli::parse();
    match &cli.command {
        Some(Commands::GenerateConfig { output }) => {
            debug!("Generating dummy config at: {:?}", output);
            return generate_dummy_config(output);
        }
        None => (),
    }

    info!("Loading config from: {:?}", cli.config);
    let mut core = Core::load(cli.config.clone()).await?;
    if let Some(node) = cli.node {
        info!("Overriding default node with: {}", node);
        core.config.default_node = node;
    }

    let (tx_sender, tx_receiver) = kanal::bounded(10);
    core.tx_sender = tx_sender;
    let core = Arc::new(core);

    info!("Starting background tasks");
    let balance_content = TextContent::new(big_mode_btc(&core));
    tokio::select! {
        _ = ui_task(core.clone(), balance_content.clone()) => (),
        _ = update_utxos(core.clone()) => (),
        _ = handle_transactions(tx_receiver.clone_async(), core.clone()) => (),
        _ = update_balance(core.clone(), balance_content) => (),
    }

    info!("Application shutting down");
    Ok(())
}

// --- CLI implementation (disabled; swap in place of TUI startup below if needed) ---
//
// use core::Recipient;
// use btclib::types::Transaction;
// use std::io::{self, Write};
// use tokio::time::{self, Duration};
//
// async fn update_utxos_cli(core: Arc<Core>) {
//     let mut interval = time::interval(Duration::from_secs(20));
//     loop {
//         interval.tick().await;
//         if let Err(e) = core.fetch_utxos().await {
//             eprintln!("Failed to update UTXOs: {}", e);
//         }
//     }
// }
//
// async fn handle_transactions_cli(
//     rx: kanal::AsyncReceiver<Transaction>,
//     core: Arc<Core>,
// ) {
//     while let Ok(transaction) = rx.recv().await {
//         if let Err(e) = core.send_transaction(transaction).await {
//             eprintln!("Failed to send transaction: {}", e);
//         }
//     }
// }
//
// async fn run_cli(core: Arc<Core>) -> Result<()> {
//     loop {
//         print!("> ");
//         io::stdout().flush()?;
//         let mut input = String::new();
//         io::stdin().read_line(&mut input)?;
//         let parts: Vec<&str> = input.trim().split_whitespace().collect();
//         if parts.is_empty() {
//             continue;
//         }
//         match parts[0] {
//             "balance" => {
//                 println!("Current balance: {} satoshis", core.get_balance());
//             }
//             "send" => {
//                 if parts.len() != 3 {
//                     println!("Usage: send <recipient> <amount>");
//                     continue;
//                 }
//                 let recipient = parts[1];
//                 let amount: u64 = parts[2].parse()?;
//                 let recipient_key = core
//                     .config
//                     .contacts
//                     .iter()
//                     .find(|r| r.name == recipient)
//                     .ok_or_else(|| anyhow::anyhow!("Recipient not found"))?
//                     .load()?
//                     .key;
//                 if let Err(e) = core.fetch_utxos().await {
//                     println!("failed to fetch utxos: {e}");
//                 }
//                 let transaction = core.create_transaction(&recipient_key, amount)?;
//                 core.tx_sender.send(transaction)?;
//                 println!("Transaction sent successfully");
//                 core.fetch_utxos().await?;
//             }
//             "exit" => break,
//             _ => println!("Unknown command"),
//         }
//     }
//     Ok(())
// }
//
// Replace the TUI block in main with:
//
//     tokio::spawn(update_utxos_cli(core.clone()));
//     tokio::spawn(handle_transactions_cli(
//         tx_receiver.clone_async(),
//         core.clone(),
//     ));
//     run_cli(core).await?;
//     Ok(())
//
// fn generate_dummy_config(path: &PathBuf) -> Result<()> {
//     use core::{Config, FeeConfig, FeeType, Recipient};
//     let dummy_config = Config {
//         my_keys: vec![],
//         contacts: vec![
//             Recipient {
//                 name: "Alice".to_string(),
//                 key: PathBuf::from("alice.pub.pem"),
//             },
//             Recipient {
//                 name: "Bob".to_string(),
//                 key: PathBuf::from("bob.pub.pem"),
//             },
//         ],
//         default_node: "127.0.0.1:9000".to_string(),
//         fee_config: FeeConfig {
//             fee_type: FeeType::Percent,
//             value: 0.1,
//         },
//     };
//     let config_str = toml::to_string_pretty(&dummy_config)?;
//     std::fs::write(path, config_str)?;
//     println!("Dummy config generated at: {}", path.display());
//     Ok(())
// }
