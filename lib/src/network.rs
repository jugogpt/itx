use tracing::*;

use std::io::{Error as IoError, Read, Write};

use crate::crypto::PublicKey;
use crate::types::{Block, Transaction, TransactionOutput};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

#[derive(Debug, Clone, Deserialize, Serialize)]
pub enum Message {
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
    /// Ask a node whats the highest block it knows about
    /// in comparison to the local blockchain
    AskDifference(u32),
    /// This is the response to AskDifference
    Difference(i32),
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
        let mut data = vec![0u8; len];
        stream.read_exact(&mut data).await?;
        Self::decode(&data)
    }

}
