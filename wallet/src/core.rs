use anyhow::Result;
use crossbeam_skiplist::SkipMap;
use serde::{Deserialize, Serialize};
use tokio::net::TcpStream;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use btclib::crypto::{PrivateKey, PublicKey};
use btclib::network::Message;
use btclib::types::{Transaction, TransactionOutput};
use btclib::util::Saveable;
#[derive(Serialize, Deserialize, Clone)]
pub struct Key {
    public: PathBuf,
    private: PathBuf

}
#[derive(Clone)]
struct LoadedKey {
    public: PublicKey,
    private: PrivateKey,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct Recipient {
    pub name: String,
    pub key: PublicKey,
}

impl Recipient { 
    pub fn load(&self) -> Result<LoadedRecipient> {
        let key PublicKey::load_from_file(&self.key)?;
        Ok(LoadedRecipient {
            name: self.name.clone(),
            key,
        })
    }
}

#[derive(Serialize, Deserialize, Clone)]
pub enum FeeType {
    Fixed, 
    Percent,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct FeeConfig {
    pub fee_type: FeeType ,
    pub value: f64,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct Config {
    pub my_keys: Vec<Key>,
    pub contacts: Vec<Recipient>,
    pub default_node: String,
    pub fee_config: FeeConfig,
}