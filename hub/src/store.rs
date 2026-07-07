use tracing::*;

use crate::board::{Reputation, Task};
use btclib::crypto::PublicKey;
use redb::{ReadableTable, TableDefinition};
use std::path::Path;
use thiserror::Error;

const TASKS_TABLE: TableDefinition<&[u8], &[u8]> = TableDefinition::new("tasks");
const REPUTATION_TABLE: TableDefinition<&[u8], &[u8]> = TableDefinition::new("reputation");
// pubkey sec1 bytes -> grant time (Unix seconds). Mirrors the node's own
// bans table: same "durable set of pubkeys/IPs with a timestamp" shape.
const FAUCET_GRANTS_TABLE: TableDefinition<&[u8], i64> = TableDefinition::new("faucet_grants");
const META_TABLE: TableDefinition<&str, &[u8]> = TableDefinition::new("meta");

const SCHEMA_VERSION_KEY: &str = "schema_version";
// No migrations exist yet since this is the hub's first schema, but the
// version is still stamped from day one -- retrofitting that detection
// after the fact (rather than before the first real store exists) is
// exactly the mistake this project already made once with BlockStore.
const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Error)]
pub enum HubStoreError {
    #[error("database error: {0}")]
    Database(#[from] redb::DatabaseError),
    #[error("transaction error: {0}")]
    Transaction(#[from] redb::TransactionError),
    #[error("table error: {0}")]
    Table(#[from] redb::TableError),
    #[error("storage error: {0}")]
    Storage(#[from] redb::StorageError),
    #[error("commit error: {0}")]
    Commit(#[from] redb::CommitError),
    #[error("failed to (de)serialize: {0}")]
    Serialization(String),
    #[error("malformed public key in storage: {0}")]
    BadPublicKey(String),
    #[error("store was created by schema version {found}, this build expects {expected}")]
    UnsupportedSchemaVersion { found: u32, expected: u32 },
    #[error("stored schema version record is corrupt")]
    CorruptSchemaVersion,
}

pub type Result<T> = std::result::Result<T, HubStoreError>;

/// Durable, crash-safe persistence for the task board, mirroring
/// `btclib::store::BlockStore`'s design: each write is one atomic redb
/// transaction, and every entity (task, reputation record, faucet grant)
/// is stored individually rather than as one big serialized blob that has
/// to be rewritten in full on every change.
pub struct HubStore {
    db: redb::Database,
}

impl HubStore {
    pub fn open_or_create<P: AsRef<Path>>(path: P) -> Result<Self> {
        let db = redb::Database::create(path)?;
        let write_txn = db.begin_write()?;
        {
            write_txn.open_table(TASKS_TABLE)?;
            write_txn.open_table(REPUTATION_TABLE)?;
            write_txn.open_table(FAUCET_GRANTS_TABLE)?;
            let mut meta = write_txn.open_table(META_TABLE)?;

            let stored_version = match meta.get(SCHEMA_VERSION_KEY)? {
                Some(value) => {
                    let bytes: [u8; 4] = value
                        .value()
                        .try_into()
                        .map_err(|_| HubStoreError::CorruptSchemaVersion)?;
                    Some(u32::from_be_bytes(bytes))
                }
                None => None,
            };
            match stored_version {
                Some(found) if found != SCHEMA_VERSION => {
                    return Err(HubStoreError::UnsupportedSchemaVersion {
                        found,
                        expected: SCHEMA_VERSION,
                    });
                }
                Some(_) => {}
                None => {
                    meta.insert(SCHEMA_VERSION_KEY, SCHEMA_VERSION.to_be_bytes().as_slice())?;
                }
            }
        }
        write_txn.commit()?;
        Ok(HubStore { db })
    }

    pub fn save_task(&self, task: &Task) -> Result<()> {
        let mut bytes = Vec::new();
        ciborium::into_writer(task, &mut bytes)
            .map_err(|e| HubStoreError::Serialization(e.to_string()))?;
        let write_txn = self.db.begin_write()?;
        {
            let mut table = write_txn.open_table(TASKS_TABLE)?;
            table.insert(task.id.as_bytes().as_slice(), bytes.as_slice())?;
        }
        write_txn.commit()?;
        Ok(())
    }

    pub fn load_all_tasks(&self) -> Result<Vec<Task>> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(TASKS_TABLE)?;
        table
            .iter()?
            .map(|entry| {
                let (_, value) = entry?;
                ciborium::from_reader(value.value())
                    .map_err(|e: ciborium::de::Error<_>| HubStoreError::Serialization(e.to_string()))
            })
            .collect()
    }

    pub fn save_reputation(&self, pubkey: &PublicKey, reputation: &Reputation) -> Result<()> {
        let mut bytes = Vec::new();
        ciborium::into_writer(reputation, &mut bytes)
            .map_err(|e| HubStoreError::Serialization(e.to_string()))?;
        let write_txn = self.db.begin_write()?;
        {
            let mut table = write_txn.open_table(REPUTATION_TABLE)?;
            table.insert(pubkey.to_sec1_bytes().as_slice(), bytes.as_slice())?;
        }
        write_txn.commit()?;
        Ok(())
    }

    pub fn load_all_reputation(&self) -> Result<Vec<(PublicKey, Reputation)>> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(REPUTATION_TABLE)?;
        table
            .iter()?
            .map(|entry| {
                let (key, value) = entry?;
                let pubkey = PublicKey::from_sec1_bytes(key.value())
                    .map_err(|e| HubStoreError::BadPublicKey(e.to_string()))?;
                let reputation = ciborium::from_reader(value.value())
                    .map_err(|e: ciborium::de::Error<_>| HubStoreError::Serialization(e.to_string()))?;
                Ok((pubkey, reputation))
            })
            .collect()
    }

    pub fn save_faucet_grant(&self, pubkey: &PublicKey, granted_at_unix: i64) -> Result<()> {
        let write_txn = self.db.begin_write()?;
        {
            let mut table = write_txn.open_table(FAUCET_GRANTS_TABLE)?;
            table.insert(pubkey.to_sec1_bytes().as_slice(), granted_at_unix)?;
        }
        write_txn.commit()?;
        Ok(())
    }

    pub fn load_all_faucet_grants(&self) -> Result<Vec<PublicKey>> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(FAUCET_GRANTS_TABLE)?;
        table
            .iter()?
            .map(|entry| {
                let (key, _) = entry?;
                PublicKey::from_sec1_bytes(key.value())
                    .map_err(|e| HubStoreError::BadPublicKey(e.to_string()))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::board::TaskStatus;
    use btclib::crypto::PrivateKey;
    use btclib::sha256::Hash;
    use chrono::Utc;
    use std::sync::atomic::{AtomicU32, Ordering};
    use uuid::Uuid;

    fn temp_db_path(name: &str) -> std::path::PathBuf {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "itx_hub_store_test_{name}_{}_{n}.redb",
            std::process::id()
        ))
    }

    #[test]
    fn round_trips_a_task_reputation_and_faucet_grant() {
        let path = temp_db_path("roundtrip");
        let store = HubStore::open_or_create(&path).unwrap();

        let poster = PrivateKey::new_key().public_key();
        let task = Task {
            id: Uuid::new_v4(),
            description: "do a thing".to_string(),
            bounty: 42,
            expected_output_hash: Hash::hash_bytes(b"answer"),
            poster,
            status: TaskStatus::Open,
            claimant: None,
            claim_deadline: None,
            failed_attempts: 0,
            created_at: Utc::now(),
        };
        store.save_task(&task).unwrap();
        let loaded = store.load_all_tasks().unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].id, task.id);
        assert_eq!(loaded[0].bounty, 42);

        let agent = PrivateKey::new_key().public_key();
        let reputation = Reputation {
            completed: 3,
            failed: 1,
            total_earned: 300,
        };
        store.save_reputation(&agent, &reputation).unwrap();
        let loaded_rep = store.load_all_reputation().unwrap();
        assert_eq!(loaded_rep.len(), 1);
        assert_eq!(loaded_rep[0].0, agent);
        assert_eq!(loaded_rep[0].1.total_earned, 300);

        store.save_faucet_grant(&agent, Utc::now().timestamp()).unwrap();
        let grants = store.load_all_faucet_grants().unwrap();
        assert_eq!(grants, vec![agent]);

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn rejects_a_store_from_a_newer_unknown_version() {
        let path = temp_db_path("newer_version");
        {
            let db = redb::Database::create(&path).unwrap();
            let write_txn = db.begin_write().unwrap();
            {
                write_txn.open_table(TASKS_TABLE).unwrap();
                write_txn.open_table(REPUTATION_TABLE).unwrap();
                write_txn.open_table(FAUCET_GRANTS_TABLE).unwrap();
                let mut meta = write_txn.open_table(META_TABLE).unwrap();
                meta.insert(
                    SCHEMA_VERSION_KEY,
                    (SCHEMA_VERSION + 1).to_be_bytes().as_slice(),
                )
                .unwrap();
            }
            write_txn.commit().unwrap();
        }

        let result = HubStore::open_or_create(&path);
        assert!(matches!(
            result,
            Err(HubStoreError::UnsupportedSchemaVersion { .. })
        ));

        std::fs::remove_file(&path).ok();
    }
}
