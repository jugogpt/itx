use tracing::*;

use std::io::{Error as IoError, ErrorKind as IoErrorKind, Read, Write};

use crate::crypto::PublicKey;
use crate::types::{Block, Transaction, TransactionOutput};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

#[derive(Debug, Clone, Deserialize, Serialize)]
pub enum Message {
    /// Sent immediately by whichever side opens the connection, before any
    /// other message. Lets both ends refuse to talk further if they are
    /// not speaking the same protocol/version, instead of misinterpreting
    /// each other's bytes. `timestamp` is the sender's own clock at the
    /// moment of sending, so the receiver can sample how far the sender's
    /// clock seems to differ from its own (see `perform_handshake_*`).
    Hello {
        magic: u32,
        version: u32,
        timestamp: DateTime<Utc>,
    },
    /// Reply to Hello. `accepted` is false if the magic/version did not
    /// match, in which case the sender will close the connection right
    /// after sending this.
    HelloAck {
        magic: u32,
        version: u32,
        accepted: bool,
        timestamp: DateTime<Utc>,
    },
    // we need to fetch all utxos belonging to a public key 
    FetchUTXOs(PublicKey),
    //utxos beloning to a public key. contains the output of transactions for bookkeeping and retrival as well as a bool to determine if marked
    UTXOs(Vec<(TransactionOutput, bool)>),
    //send a transction to the network 
    SubmitTransaction(Transaction), //the wallet sends this to the node which it will then verify
    // Broadcast a new transaction to other nodes 
    NewTransaction(Transaction),
    //ask the node to prepare the optimal block template 
    // with the coinbase trnasaction paying the specified 
    // public key 
    FetchTemplate(PublicKey),
    /// The template
    Template(Block),
    /// Ask the node to validate a block template.
    /// This is to prevent the node from mining an invalid
    /// block (e.g. if one has been found in the meantime,
    /// or if transactions have been removed from the mempool)
    ValidateTemplate(Block),
    /// If template is valid
    TemplateValidity(bool),
    /// Submit a mined block to a node
    SubmitTemplate(Block),
    /// Ask a node to report all the other nodes it knows
    /// about
    DiscoverNodes,
    /// This is the response to DiscoverNodes
    NodeList(Vec<String>),
    /// Ask a node for its current chain tip: how many blocks it has and
    /// how much cumulative proof-of-work backs its chain. Used to decide
    /// which peer to sync from -- the chain with the most work wins, not
    /// simply the one with the most blocks.
    AskChainTip,
    /// This is the response to AskChainTip: (block height, cumulative work)
    ChainTip(u32, crate::U256),
    /// Ask a node to send a block with the specified height
    FetchBlock(usize),
    /// Broadcast a new block to other nodes
    NewBlock(Block),
}


// we are going to use length-prefixed encoding for message (this can definitely be updated)
impl Message {

    pub fn encode(
        &self,
    ) -> Result<Vec<u8>, ciborium::ser::Error<IoError>> {
        let mut bytes = Vec::new();
        ciborium::into_writer(self, &mut bytes)?;
        Ok(bytes)
    }
    pub fn decode(
        data: &[u8],
    ) -> Result<Self, ciborium::de::Error<IoError>> {
        ciborium::from_reader(data)
    }
    pub fn send(
        &self,
        stream: &mut impl Write,
    ) -> Result<(), ciborium::ser::Error<IoError>> {
        let bytes = self.encode()?;
        let len = bytes.len() as u64;
        stream.write_all(&len.to_be_bytes())?;
        stream.write_all(&bytes)?;
        Ok(())
    }
    pub fn recieve(
        stream: &mut impl Read,
    ) -> Result<Self, ciborium::de::Error<IoError>> {
        let mut len_bytes = [0u8; 8];
        stream.read_exact(&mut len_bytes)?;
        let len = u64::from_be_bytes(len_bytes) as usize;
        if len > crate::MAX_MESSAGE_SIZE {
            return Err(oversized_message_error());
        }
        let mut data = vec![0u8; len];
        stream.read_exact(&mut data)?;
        Self::decode(&data)

    }


    pub async fn send_async(&self, stream: &mut (impl AsyncWrite + Unpin)) -> Result<(), ciborium::ser::Error<IoError>> {
        let bytes = self.encode()?;
        let len = bytes.len() as u64;
        stream.write_all(&len.to_be_bytes()).await?;
        stream.write_all(&bytes).await?;
        Ok(())
    }

    pub async fn receive_async(stream: &mut (impl AsyncRead + Unpin)) -> Result<Self, ciborium::de::Error<IoError>> {
        let mut len_bytes = [0u8; 8];
        stream.read_exact(&mut len_bytes).await?;
        let len = u64::from_be_bytes(len_bytes) as usize;
        if len > crate::MAX_MESSAGE_SIZE {
            return Err(oversized_message_error());
        }
        let mut data = vec![0u8; len];
        stream.read_exact(&mut data).await?;
        Self::decode(&data)
    }

}

/// True if a `receive`/`receive_async` error is just the peer having
/// closed the connection (EOF/reset/aborted/broken-pipe), as opposed to
/// having actually sent something malformed. Only a peer that already
/// completed the handshake can trigger this at all, so treating a
/// graceful hangup as harmless -- rather than the same "protocol
/// violation" as garbled bytes -- doesn't open any new door for an
/// attacker; it just stops perfectly ordinary short-lived clients (e.g.
/// a service that opens a fresh connection per request instead of
/// holding one open) from being penalized for disconnecting normally.
pub fn is_benign_disconnect(e: &ciborium::de::Error<IoError>) -> bool {
    matches!(e, ciborium::de::Error::Io(io_err) if matches!(
        io_err.kind(),
        IoErrorKind::UnexpectedEof
            | IoErrorKind::ConnectionReset
            | IoErrorKind::ConnectionAborted
            | IoErrorKind::BrokenPipe
    ))
}

fn oversized_message_error() -> ciborium::de::Error<IoError> {
    IoError::new(
        IoErrorKind::InvalidData,
        "peer sent a message exceeding MAX_MESSAGE_SIZE",
    )
    .into()
}

/// Errors that can occur while performing the peer handshake.
#[derive(Debug, Error)]
pub enum HandshakeError {
    #[error("network error during handshake: {0}")]
    Network(String),
    #[error("peer speaks an incompatible protocol (magic {0:#x}, version {1})")]
    Incompatible(u32, u32),
    #[error("peer sent an unexpected message during handshake")]
    UnexpectedMessage,
}

/// Performs the handshake from the side that opened the connection: send
/// `Hello` first, then wait for the peer's `HelloAck`. Call this right
/// after establishing a TCP connection and before sending anything else.
///
/// Returns the peer's clock offset from ours (their reported time minus
/// our local time when their reply arrived), so the caller can feed it
/// into network time-adjustment. This is a raw, unvalidated single
/// sample -- callers should never trust it alone, only ever as one input
/// to a median across many peers (see `node::time_sync`).
pub async fn perform_handshake_initiator(
    stream: &mut (impl AsyncRead + AsyncWrite + Unpin),
) -> Result<chrono::Duration, HandshakeError> {
    let hello = Message::Hello {
        magic: crate::PROTOCOL_MAGIC,
        version: crate::PROTOCOL_VERSION,
        timestamp: Utc::now(),
    };
    hello
        .send_async(stream)
        .await
        .map_err(|e| HandshakeError::Network(e.to_string()))?;

    let received_at = Utc::now();
    match Message::receive_async(stream)
        .await
        .map_err(|e| HandshakeError::Network(e.to_string()))?
    {
        Message::HelloAck { magic, version, accepted, timestamp } => {
            if !accepted || magic != crate::PROTOCOL_MAGIC || version != crate::PROTOCOL_VERSION {
                return Err(HandshakeError::Incompatible(magic, version));
            }
            Ok(timestamp - received_at)
        }
        _ => Err(HandshakeError::UnexpectedMessage),
    }
}

/// Performs the handshake from the side that accepted the connection:
/// wait for the peer's `Hello`, then reply with `HelloAck`. Call this
/// before processing any other message on a freshly accepted socket.
/// Returns the peer's clock offset, same caveats as
/// `perform_handshake_initiator`.
pub async fn perform_handshake_acceptor(
    stream: &mut (impl AsyncRead + AsyncWrite + Unpin),
) -> Result<chrono::Duration, HandshakeError> {
    match Message::receive_async(stream)
        .await
        .map_err(|e| HandshakeError::Network(e.to_string()))?
    {
        Message::Hello { magic, version, timestamp } => {
            let received_at = Utc::now();
            let accepted = magic == crate::PROTOCOL_MAGIC && version == crate::PROTOCOL_VERSION;
            let ack = Message::HelloAck {
                magic: crate::PROTOCOL_MAGIC,
                version: crate::PROTOCOL_VERSION,
                accepted,
                timestamp: Utc::now(),
            };
            ack.send_async(stream)
                .await
                .map_err(|e| HandshakeError::Network(e.to_string()))?;
            if accepted {
                Ok(timestamp - received_at)
            } else {
                Err(HandshakeError::Incompatible(magic, version))
            }
        }
        _ => Err(HandshakeError::UnexpectedMessage),
    }
}
