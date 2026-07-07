use tracing::*;

use crate::sha256::Hash;
use crate::types::Block;
use redb::{ReadableTable, TableDefinition};
use std::path::Path;
use thiserror::Error;

const BLOCKS_TABLE: TableDefinition<&[u8], &[u8]> = TableDefinition::new("blocks");
const META_TABLE: TableDefinition<&str, &[u8]> = TableDefinition::new("meta");
// IP address (its string form) -> ban expiry, as a Unix timestamp. So a
// banned peer stays banned across a restart instead of getting a free
// pass the moment the node happens to restart for any reason.
const BANS_TABLE: TableDefinition<&str, i64> = TableDefinition::new("bans");

const ACTIVE_CHAIN_KEY: &str = "active_chain";
const SCHEMA_VERSION_KEY: &str = "schema_version";

/// Bump this whenever the on-disk representation changes (new table, new
/// key layout, a change to what put_block/set_active_chain actually
/// store), and add a corresponding entry to `MIGRATIONS` so an existing
/// store gets upgraded in place instead of just being rejected.
const SCHEMA_VERSION: u32 = 2;

/// A migration brings a store from the version immediately below the one
/// it's named after up to that version, run inside the same write
/// transaction that will also stamp the new version number -- so a
/// migration that fails partway can't leave the store stamped as a
/// version it doesn't actually match.
type Migration = fn(&redb::WriteTransaction) -> Result<()>;

/// Every migration this build knows how to apply, ordered oldest first.
/// `open_or_create` applies every entry whose version is greater than
/// whatever the store is currently stamped with. To add one when bumping
/// `SCHEMA_VERSION`: write the function, then append `(new_version, fn)`.
const MIGRATIONS: &[(u32, Migration)] = &[(2, migrate_v1_to_v2)];

/// v1 stores (from before ban persistence existed) have no BANS_TABLE.
/// There's no existing data to transform -- just make sure the table is
/// there so every later version can assume it always exists.
fn migrate_v1_to_v2(txn: &redb::WriteTransaction) -> Result<()> {
    txn.open_table(BANS_TABLE)?;
    Ok(())
}

#[derive(Debug, Error)]
pub enum StoreError {
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
    #[error("failed to (de)serialize a block: {0}")]
    Serialization(String),
    #[error("stored active-chain record is corrupt: length {0} is not a multiple of 32")]
    CorruptActiveChain(usize),
    #[error("store was created by schema version {found}, this build expects {expected}")]
    UnsupportedSchemaVersion { found: u32, expected: u32 },
    #[error("stored schema version record is corrupt")]
    CorruptSchemaVersion,
}

pub type Result<T> = std::result::Result<T, StoreError>;

/// Durable, crash-safe storage for the blockchain: every known block,
/// keyed by hash, plus the ordered list of hashes making up the current
/// active chain. Each write is a single atomic redb transaction, so a
/// crash mid-write can no longer corrupt previously-stored data the way
/// re-serializing the entire chain to one flat file periodically could.
pub struct BlockStore {
    db: redb::Database,
}

impl BlockStore {
    pub fn open_or_create<P: AsRef<Path>>(path: P) -> Result<Self> {
        let db = redb::Database::create(path)?;
        // BLOCKS_TABLE's shape has never changed across any schema
        // version so far, so it's always safe to just ensure it exists
        // up front regardless of what version we end up finding below.
        let write_txn = db.begin_write()?;
        {
            write_txn.open_table(BLOCKS_TABLE)?;
            let mut meta = write_txn.open_table(META_TABLE)?;

            let stored_version = match meta.get(SCHEMA_VERSION_KEY)? {
                Some(value) => {
                    let bytes: [u8; 4] = value
                        .value()
                        .try_into()
                        .map_err(|_| StoreError::CorruptSchemaVersion)?;
                    Some(u32::from_be_bytes(bytes))
                }
                None => None,
            };

            match stored_version {
                None => {
                    // brand new store: nothing to migrate, just create
                    // whatever the current schema needs directly
                    write_txn.open_table(BANS_TABLE)?;
                }
                Some(found) if found > SCHEMA_VERSION => {
                    return Err(StoreError::UnsupportedSchemaVersion {
                        found,
                        expected: SCHEMA_VERSION,
                    });
                }
                Some(found) if found < SCHEMA_VERSION => {
                    println!("migrating store from schema version {found} to {SCHEMA_VERSION}...");
                    for (version, migration) in MIGRATIONS {
                        if *version > found {
                            migration(&write_txn)?;
                            println!("applied migration to schema version {version}");
                        }
                    }
                }
                Some(_) => {} // already current, nothing to do
            }

            meta.insert(SCHEMA_VERSION_KEY, SCHEMA_VERSION.to_be_bytes().as_slice())?;
        }
        write_txn.commit()?;
        Ok(BlockStore { db })
    }

    /// Stores a block, keyed by its own hash. Idempotent: storing the same
    /// block twice is harmless.
    pub fn put_block(&self, block: &Block) -> Result<()> {
        let hash = block.hash();
        let mut bytes = Vec::new();
        ciborium::into_writer(block, &mut bytes)
            .map_err(|e| StoreError::Serialization(e.to_string()))?;

        let write_txn = self.db.begin_write()?;
        {
            let mut table = write_txn.open_table(BLOCKS_TABLE)?;
            table.insert(hash.as_bytes().as_slice(), bytes.as_slice())?;
        }
        write_txn.commit()?;
        Ok(())
    }

    /// Removes a batch of blocks from storage in a single transaction.
    /// Used to prune side-branch blocks that have fallen far enough behind
    /// the active chain that a reorg back to them is no longer realistic,
    /// so there's no point paying to keep them around forever. A no-op
    /// (no transaction opened) if `hashes` is empty.
    pub fn delete_blocks(&self, hashes: &[Hash]) -> Result<()> {
        if hashes.is_empty() {
            return Ok(());
        }
        let write_txn = self.db.begin_write()?;
        {
            let mut table = write_txn.open_table(BLOCKS_TABLE)?;
            for hash in hashes {
                table.remove(hash.as_bytes().as_slice())?;
            }
        }
        write_txn.commit()?;
        Ok(())
    }

    pub fn get_block(&self, hash: &Hash) -> Result<Option<Block>> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(BLOCKS_TABLE)?;
        match table.get(hash.as_bytes().as_slice())? {
            Some(value) => {
                let block = ciborium::from_reader(value.value())
                    .map_err(|e: ciborium::de::Error<_>| StoreError::Serialization(e.to_string()))?;
                Ok(Some(block))
            }
            None => Ok(None),
        }
    }

    /// Overwrites the record of which hashes make up the active chain, in
    /// order from genesis to tip. Cheap even for long chains: it's just a
    /// flat list of 32-byte hashes, not the block contents themselves.
    pub fn set_active_chain(&self, chain: &[Hash]) -> Result<()> {
        let mut bytes = Vec::with_capacity(chain.len() * 32);
        for hash in chain {
            bytes.extend_from_slice(&hash.as_bytes());
        }

        let write_txn = self.db.begin_write()?;
        {
            let mut table = write_txn.open_table(META_TABLE)?;
            table.insert(ACTIVE_CHAIN_KEY, bytes.as_slice())?;
        }
        write_txn.commit()?;
        Ok(())
    }

    pub fn get_active_chain(&self) -> Result<Vec<Hash>> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(META_TABLE)?;
        match table.get(ACTIVE_CHAIN_KEY)? {
            Some(value) => {
                let bytes = value.value();
                if bytes.len() % 32 != 0 {
                    return Err(StoreError::CorruptActiveChain(bytes.len()));
                }
                Ok(bytes
                    .chunks_exact(32)
                    .map(|chunk| {
                        let array: [u8; 32] =
                            chunk.try_into().expect("chunks_exact(32) guarantees this");
                        Hash::from_bytes(array)
                    })
                    .collect())
            }
            None => Ok(vec![]),
        }
    }

    /// Records that `ip` is banned until `until_unix` (seconds since the
    /// Unix epoch), surviving a node restart. Overwrites any existing ban
    /// for the same IP.
    pub fn save_ban(&self, ip: &str, until_unix: i64) -> Result<()> {
        let write_txn = self.db.begin_write()?;
        {
            let mut table = write_txn.open_table(BANS_TABLE)?;
            table.insert(ip, until_unix)?;
        }
        write_txn.commit()?;
        Ok(())
    }

    /// Loads every persisted ban as (ip, expiry as Unix seconds). Expired
    /// entries are returned too -- the caller decides what to do with
    /// them (the in-memory ban tracker already knows how to treat a
    /// past expiry as "not banned").
    pub fn load_bans(&self) -> Result<Vec<(String, i64)>> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(BANS_TABLE)?;
        table
            .iter()?
            .map(|entry| {
                let (key, value) = entry?;
                Ok((key.value().to_string(), value.value()))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    fn temp_db_path(name: &str) -> std::path::PathBuf {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "itx_store_test_{name}_{}_{n}.redb",
            std::process::id()
        ))
    }

    #[test]
    fn open_or_create_migrates_a_v1_store_to_current() {
        let path = temp_db_path("migrate_v1");
        // simulate an old store: schema_version=1, no BANS_TABLE at all
        {
            let db = redb::Database::create(&path).unwrap();
            let write_txn = db.begin_write().unwrap();
            {
                write_txn.open_table(BLOCKS_TABLE).unwrap();
                let mut meta = write_txn.open_table(META_TABLE).unwrap();
                meta.insert(SCHEMA_VERSION_KEY, 1u32.to_be_bytes().as_slice())
                    .unwrap();
            }
            write_txn.commit().unwrap();
        }

        // opening with the current build should migrate it in place
        // instead of rejecting it
        let store = BlockStore::open_or_create(&path).expect("should migrate, not error");

        // the migration's whole job was to make sure BANS_TABLE exists;
        // prove it by actually using it
        store.save_ban("127.0.0.1", 123).unwrap();
        assert_eq!(
            store.load_bans().unwrap(),
            vec![("127.0.0.1".to_string(), 123)]
        );

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn open_or_create_rejects_a_store_from_a_newer_unknown_version() {
        let path = temp_db_path("newer_store");
        {
            let db = redb::Database::create(&path).unwrap();
            let write_txn = db.begin_write().unwrap();
            {
                write_txn.open_table(BLOCKS_TABLE).unwrap();
                write_txn.open_table(BANS_TABLE).unwrap();
                let mut meta = write_txn.open_table(META_TABLE).unwrap();
                meta.insert(
                    SCHEMA_VERSION_KEY,
                    (SCHEMA_VERSION + 1).to_be_bytes().as_slice(),
                )
                .unwrap();
            }
            write_txn.commit().unwrap();
        }

        let result = BlockStore::open_or_create(&path);
        assert!(matches!(
            result,
            Err(StoreError::UnsupportedSchemaVersion { .. })
        ));

        std::fs::remove_file(&path).ok();
    }
}
