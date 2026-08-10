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
///
/// Holds an ordered list of node addresses rather than one: `connect()`
/// always tries `addresses[0]` first and only falls through to the next
/// on failure. This is deliberately *not* load-balanced/round-robin --
/// the hub's double-spend safety (`payout_lock`/`exchange_custody_payout_lock`)
/// assumes every call sees one consistent mempool view, so spreading
/// normal traffic across two independently-converging node mempools would
/// reintroduce race risk. With ordered failover, all traffic goes to the
/// primary in the healthy case (identical behavior to a single-node
/// setup), and a secondary only takes over during an actual primary
/// outage.
#[derive(Clone)]
pub struct NodeClient {
    addresses: Vec<String>,
}

impl NodeClient {
    pub fn new(addresses: Vec<String>) -> Self {
        NodeClient { addresses }
    }

    async fn connect(&self) -> Result<TcpStream> {
        let mut last_err = None;
        for address in &self.addresses {
            match Self::connect_one(address).await {
                Ok(stream) => return Ok(stream),
                Err(e) => {
                    warn!("node at {address} unreachable, trying next: {e}");
                    last_err = Some(e);
                }
            }
        }
        Err(last_err.unwrap_or_else(|| anyhow::anyhow!("no node addresses configured")))
    }

    async fn connect_one(address: &str) -> Result<TcpStream> {
        let mut stream = TcpStream::connect(address)
            .await
            .with_context(|| format!("failed to connect to node at {address}"))?;
        btclib::network::perform_handshake_initiator(&mut stream)
            .await
            .map_err(|e| anyhow::anyhow!("handshake with node at {address} failed: {e}"))?;
        Ok(stream)
    }

    /// Current chain height, from whichever configured node answers first
    /// -- see `connect()`'s doc comment for the failover order. Used by
    /// `GET /health` to prove the hub can actually reach a node, not just
    /// that its own process is alive.
    pub async fn chain_tip(&self) -> Result<u32> {
        let mut stream = self.connect().await?;
        let message = Message::AskChainTip;
        message.send_async(&mut stream).await?;
        match Message::receive_async(&mut stream).await? {
            Message::ChainTip(height, _work) => Ok(height),
            other => anyhow::bail!("unexpected response from node: {other:?}"),
        }
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
