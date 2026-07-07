use tracing::*;

use anyhow::{Context, Result};
use btclib::crypto::PublicKey;
use btclib::network::Message;
use btclib::types::{Transaction, TransactionOutput};
use tokio::net::TcpStream;

/// A lightweight client for talking to a running blockchain node.
///
/// Deliberately opens a fresh TCP connection (and performs the handshake)
/// for every single operation rather than holding one persistent
/// connection open: the hub serves many concurrent HTTP requests, and a
/// shared connection would either need its own mutex (serializing every
/// node interaction behind one lock) or reconnect-with-backoff logic to
/// recover from a dropped connection. Paying for a fresh handshake per
/// call is cheap at this scale, and it means a single failed request
/// never affects any other in-flight one.
#[derive(Clone)]
pub struct NodeClient {
    address: String,
}

impl NodeClient {
    pub fn new(address: String) -> Self {
        NodeClient { address }
    }

    async fn connect(&self) -> Result<TcpStream> {
        let mut stream = TcpStream::connect(&self.address)
            .await
            .with_context(|| format!("failed to connect to node at {}", self.address))?;
        btclib::network::perform_handshake_initiator(&mut stream)
            .await
            .map_err(|e| anyhow::anyhow!("handshake with node at {} failed: {e}", self.address))?;
        Ok(stream)
    }

    /// Every UTXO currently belonging to `pubkey`, as reported by the
    /// node -- including whether the node's own mempool view considers
    /// each one already spoken for (`marked`).
    pub async fn fetch_utxos(&self, pubkey: &PublicKey) -> Result<Vec<(bool, TransactionOutput)>> {
        let mut stream = self.connect().await?;
        let message = Message::FetchUTXOs(pubkey.clone());
        message.send_async(&mut stream).await?;
        match Message::receive_async(&mut stream).await? {
            Message::UTXOs(utxos) => Ok(utxos
                .into_iter()
                .map(|(output, marked)| (marked, output))
                .collect()),
            other => anyhow::bail!("unexpected response from node: {other:?}"),
        }
    }

    /// Total spendable balance: everything not already marked as pending
    /// in the node's own mempool view.
    pub async fn balance(&self, pubkey: &PublicKey) -> Result<u64> {
        let utxos = self.fetch_utxos(pubkey).await?;
        Ok(utxos
            .iter()
            .filter(|(marked, _)| !marked)
            .map(|(_, output)| output.value)
            .sum())
    }

    /// Submits a transaction and returns as soon as it's sent -- the node
    /// protocol doesn't send an acknowledgement back for this message
    /// (the wallet and miner both already rely on this same fire-and-
    /// forget behavior), so success here means "accepted for delivery,"
    /// not "confirmed."
    pub async fn submit_transaction(&self, transaction: Transaction) -> Result<()> {
        let mut stream = self.connect().await?;
        let message = Message::SubmitTransaction(transaction);
        message.send_async(&mut stream).await?;
        Ok(())
    }
}
