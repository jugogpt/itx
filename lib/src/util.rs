use tracing::*;
use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_subscriber::{fmt, prelude::*, EnvFilter};
use crate::core::{Config, Core, FeeConfig, FeeType, Recipient};
use anyhow::Result;
use std::panic;
use std::path::PathBuf;
use crate::sha256::Hash;
use crate::types::Transaction;
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::{Read, Result as IoResult, Write};
use std::path::Path;

pub trait Saveable
where
    Self: Sized,
{
    fn load<I: Read>(reader: I) -> IoResult<Self>;
    fn save<O: Write>(&self, writer: O) -> IoResult<()>;
    fn save_to_file<P: AsRef<Path>>(&self, path: P) -> IoResult<()> {
        let file = File::create(path)?;
        self.save(file)
    }
    fn load_from_file<P: AsRef<Path>>(path: P) -> IoResult<Self> {
        let file = File::open(path)?;
        Self::load(file)
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct MerkleRoot(Hash);

impl MerkleRoot {
    pub fn calculate(transactions: &[Transaction]) -> MerkleRoot {
        let mut layer: Vec<Hash> = vec![];
        for transaction in transactions {
            layer.push(Hash::hash(transaction));
        }

        while layer.len() > 1 {
            let mut new_layer = vec![];
            for pair in layer.chunks(2) {
                let left = pair[0];
                let right = pair.get(1).unwrap_or(&pair[0]);
                new_layer.push(Hash::hash(&[left, *right]));
            }
            layer = new_layer;
        }

        MerkleRoot(layer[0])
    }
}

pub fn setup_tracing() -> Result<()> {
    let file_apprender = RollingFileAppender::new(
        Rotation::DAILY,
        "logs",
        "wallet.log",
    );
    tracing_subscriber::registry().with(fmt::layer().with_writer(file_appender)).with(EnvFilter::from_default_env().add_directive(tracing::Level::TRACE.into())).init();
    Ok(())
}

// Make sure tracing is able to log panics occurring in the wallet 
pub fn setup_panic_hook() {
    panic::set_hook(Box::new(|panic_info| {
        let backtrace = 
            std::backtrace::Backtrace::force)capture();
        error!("Application panicked!");
        error!("Panic info: {:?}", panic_info);
        error!("Backtrace: {:?}", backtrace);      
    }));
}

//generate a dummy config 
pub fn generate_dummy_config(path: &PathBuf) -> Result<()> {
    let dummy_config = Config {
        my_keys: vec![],
        contacts: vec![
            Recipient {
                name: "Alice".to_string(),
                key: PathBuf::from("alice.pub.pem"),
            },
            Recipient {
                name: "Bob".to_string(),
                key: PathBuf::from("bob.pub.pem"),
            },
        ],
        default_node: "127.0.0.1:9000".to_string(),
        fee_config: FeeConfig {
            fee_type: FeeType::Percent,
            value: 0.1,
        },
    };
    let config_str = toml::to_string_pretty(&dummy_config)?;
    std::fs::write(path, config_str)?;
    println!("Dummy config generated at: {}", path.display());
    Ok(())
}

// Convert satoshis to a BTC string 
pub fn sats_to_btc(sats: u64) -> String {
    let btc = sats as f64 / 100_000_000.0;
    format!("{} BTC", btc)
}

pub fn big_mode_btc(core: &Core) -> String {
    text_to_ascii_art::convert(sats_to_btc(core.get_balance())).unwrap()
}
