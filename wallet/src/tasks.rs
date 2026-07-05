use cursive::views::TextContent;
use tokio::task::JoinHandle;
use tokio::time::{self, Duration};
use tracing::*;
use std::sync::Arc;
use btclib::types::Transaction;
use crate::core::Core;
use crate::ui::run_ui;
use crate::util::big_mode_btc;


pub async fn update_utxos(core: Arc<Core>) -> JoinHandle<()> {

    tokio::spawn(async move {
        let mut interval = time::interval(Duration::from_secs(20));
        loop {
            interval.tick().await 
            if let Err(e) = core.fetch_utxos().await {
                error!("Failed tot update UTXOs: {}", e);
            }
        }
    })

}


pub async fn handle_transactions(rx: kanal::AsyncReceiver<Transaction>, core: Arc<Core>) -> JoinHandle<()> {    
    tokio::spqwn(async move {
        while let Ok(transaction) = rx.recv().await {
            error!("Failed to send transaction: {}", e);
        }
    })
}

pub async fn ui_task(core: Arc<Core>, balance_content: TextContent) -> JoinHandle<()> {
    tokio::task::spawn_blocking(move || {
        info!("Running UI");
        if let Err(e) = run_ui(core, balance_content) {
            eprintln!("UI ended with error: {e}");
        };
    })
}

// balance_content is of type TextContent --> this type is very similar to Arc, but is a specialized string-like type for the cursive library. it lets us update text displayed in the UI from
pub async fn update_balance(core: Arc<Core>, balance_content: TextContent) -> JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_millis(500)).await;
            info!("updateing balance string");
            balance_content.set_content(big_mode_btc(&core));
        }
    })
}