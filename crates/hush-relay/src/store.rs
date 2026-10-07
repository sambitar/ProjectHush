use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection};
use sha2::{Digest, Sha256};

use crate::auth::{hash_token, tokens_match};
use crate::config::RelayConfig;
use crate::error::RelayError;

pub struct Store {
    conn: Mutex<Connection>,
    config: RelayConfig,
    /// queue_id -> (window_start_secs, count)
    rate: Mutex<std::collections::HashMap<String, (u64, u32)>>,
}

impl Store {
    pub fn open(path: &str, config: RelayConfig) -> Result<Self, RelayError> {
        let conn = Connection::open(path).map_err(|_| RelayError::Internal)?;
        conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS queues (
                id TEXT PRIMARY KEY,
                token_hash BLOB NOT NULL,
                burned INTEGER NOT NULL DEFAULT 0,
                presence_at INTEGER NOT NULL DEFAULT 0
            );
            CREATE TABLE IF NOT EXISTS blobs (
                id TEXT PRIMARY KEY,
                queue_id TEXT NOT NULL,
                body BLOB NOT NULL,
                created_at INTEGER NOT NULL,
                FOREIGN KEY (queue_id) REFERENCES queues(id)
            );
            CREATE INDEX IF NOT EXISTS idx_blobs_queue ON blobs(queue_id);
            CREATE TABLE IF NOT EXISTS device_dirs (
                account_id TEXT PRIMARY KEY,
                blob BLOB NOT NULL,
                updated_unix INTEGER NOT NULL
            );
            ",
        )
        .map_err(|_| RelayError::Internal)?;
        let _ = conn.execute(
            "ALTER TABLE queues ADD COLUMN presence_at INTEGER NOT NULL DEFAULT 0",
            [],
        );
        Ok(Self {
            conn: Mutex::new(conn),
            config,
            rate: Mutex::new(std::collections::HashMap::new()),
        })
    }

    fn now_secs() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or(Duration::ZERO)
            .as_secs()
    }

    fn check_rate(&self, queue_id: &str) -> Result<(), RelayError> {
        let now = Self::now_secs();
        let window = self.config.rate_limit_window.as_secs().max(1);
        let max = self.config.rate_limit_max;
        let mut rate = self.rate.lock().map_err(|_| RelayError::Internal)?;
        let entry = rate.entry(queue_id.to_string()).or_insert((now, 0));
        if now - entry.0 >= window {
            *entry = (now, 0);
        }
        if entry.1 >= max {
            return Err(RelayError::RateLimited);
        }
        entry.1 += 1;
        Ok(())
    }

    fn load_queue(&self, queue_id: &str) -> Result<Option<QueueRow>, RelayError> {
        let conn = self.conn.lock().map_err(|_| RelayError::Internal)?;
        let mut stmt = conn
            .prepare("SELECT token_hash, burned FROM queues WHERE id = ?1")
            .map_err(|_| RelayError::Internal)?;
        let mut rows = stmt
            .query(params![queue_id])
            .map_err(|_| RelayError::Internal)?;
        if let Some(row) = rows.next().map_err(|_| RelayError::Internal)? {
            let hash: Vec<u8> = row.get(0).map_err(|_| RelayError::Internal)?;
            let burned: i64 = row.get(1).map_err(|_| RelayError::Internal)?;
            return Ok(Some(QueueRow {
                token_hash: hash,
                burned: burned != 0,
            }));
        }
        Ok(None)
    }

    fn authorize(&self, queue_id: &str, token: &str) -> Result<(), RelayError> {
        match self.load_queue(queue_id)? {
            Some(q) if q.burned => Err(RelayError::Burned),
            Some(q) if tokens_match(&q.token_hash, token) => Ok(()),
            Some(_) => Err(RelayError::Unauthorized),
            None => {
                let hash = hash_token(token);
                let conn = self.conn.lock().map_err(|_| RelayError::Internal)?;
                conn.execute(
                    "INSERT INTO queues (id, token_hash, burned) VALUES (?1, ?2, 0)",
                    params![queue_id, hash.as_slice()],
                )
                .map_err(|_| RelayError::Internal)?;
                Ok(())
            }
        }
    }

    pub fn put_blob(
        &self,
        queue_id: &str,
        token: &str,
        body: &[u8],
    ) -> Result<String, RelayError> {
        if body.len() > self.config.max_blob_bytes {
            return Err(RelayError::TooLarge);
        }
        self.authorize(queue_id, token)?;
        self.check_rate(queue_id)?;

        let blob_id = hex::encode(Sha256::digest(body));
        let created = Self::now_secs() as i64;
        let conn = self.conn.lock().map_err(|_| RelayError::Internal)?;
        conn.execute(
            "INSERT INTO blobs (id, queue_id, body, created_at) VALUES (?1, ?2, ?3, ?4)",
            params![blob_id, queue_id, body, created],
        )
        .map_err(|_| RelayError::Internal)?;
        Ok(blob_id)
    }

    pub fn list_blobs(&self, queue_id: &str, token: &str) -> Result<Vec<BlobMeta>, RelayError> {
        self.authorize_existing(queue_id, token)?;
        self.purge_expired()?;
        let conn = self.conn.lock().map_err(|_| RelayError::Internal)?;
        let mut stmt = conn
            .prepare("SELECT id, created_at FROM blobs WHERE queue_id = ?1 ORDER BY created_at")
            .map_err(|_| RelayError::Internal)?;
        let rows = stmt
            .query_map(params![queue_id], |row| {
                Ok(BlobMeta {
                    id: row.get(0)?,
                    created_at: row.get(1)?,
                })
            })
            .map_err(|_| RelayError::Internal)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(|_| RelayError::Internal)?);
        }
        Ok(out)
    }

    pub fn get_blob(
        &self,
        queue_id: &str,
        token: &str,
        blob_id: &str,
    ) -> Result<Vec<u8>, RelayError> {
        self.authorize_existing(queue_id, token)?;
        self.purge_expired()?;
        let conn = self.conn.lock().map_err(|_| RelayError::Internal)?;
        let mut stmt = conn
            .prepare("SELECT body FROM blobs WHERE queue_id = ?1 AND id = ?2")
            .map_err(|_| RelayError::Internal)?;
        let mut rows = stmt
            .query(params![queue_id, blob_id])
            .map_err(|_| RelayError::Internal)?;
        if let Some(row) = rows.next().map_err(|_| RelayError::Internal)? {
            let body: Vec<u8> = row.get(0).map_err(|_| RelayError::Internal)?;
            return Ok(body);
        }
        Err(RelayError::NotFound)
    }

    pub fn delete_blob(
        &self,
        queue_id: &str,
        token: &str,
        blob_id: &str,
    ) -> Result<(), RelayError> {
        self.authorize_existing(queue_id, token)?;
        let conn = self.conn.lock().map_err(|_| RelayError::Internal)?;
        let n = conn
            .execute(
                "DELETE FROM blobs WHERE queue_id = ?1 AND id = ?2",
                params![queue_id, blob_id],
            )
            .map_err(|_| RelayError::Internal)?;
        if n == 0 {
            return Err(RelayError::NotFound);
        }
        Ok(())
    }

    pub fn touch_presence(&self, queue_id: &str, token: &str) -> Result<(), RelayError> {
        self.authorize(queue_id, token)?;
        let now = Self::now_secs() as i64;
        let conn = self.conn.lock().map_err(|_| RelayError::Internal)?;
        conn.execute(
            "UPDATE queues SET presence_at = ?1 WHERE id = ?2",
            params![now, queue_id],
        )
        .map_err(|_| RelayError::Internal)?;
        Ok(())
    }

    pub fn presence_last_at(&self, queue_id: &str, token: &str) -> Result<u64, RelayError> {
        self.authorize_existing(queue_id, token)?;
        let conn = self.conn.lock().map_err(|_| RelayError::Internal)?;
        let mut stmt = conn
            .prepare("SELECT presence_at FROM queues WHERE id = ?1")
            .map_err(|_| RelayError::Internal)?;
        let mut rows = stmt
            .query(params![queue_id])
            .map_err(|_| RelayError::Internal)?;
        if let Some(row) = rows.next().map_err(|_| RelayError::Internal)? {
            let at: i64 = row.get(0).map_err(|_| RelayError::Internal)?;
            return Ok(at.max(0) as u64);
        }
        Ok(0)
    }

    pub fn report_queue(&self, queue_id: &str, token: &str) -> Result<(), RelayError> {
        self.authorize_existing(queue_id, token)?;
        let conn = self.conn.lock().map_err(|_| RelayError::Internal)?;
        conn.execute(
            "UPDATE queues SET burned = 1 WHERE id = ?1",
            params![queue_id],
        )
        .map_err(|_| RelayError::Internal)?;
        conn.execute("DELETE FROM blobs WHERE queue_id = ?1", params![queue_id])
            .map_err(|_| RelayError::Internal)?;
        Ok(())
    }

    /// Store an account's signed device list.
    ///
    /// The blob must be a device list signed by **device 1** of that account,
    /// verified against `account_id`, which *is* device 1's identity key hash
    /// (Q7). That single check replaces any bearer credential, and it is what
    /// makes revocation real: a revoked device does not hold device 1's private
    /// key, so it cannot produce a list the relay will accept. There is no token
    /// to rotate and therefore no window in which a revoked device could race
    /// the account and lock it out.
    ///
    /// The relay still does not *trust* the contents. A peer verifies the same
    /// signature independently, so a relay that stored something else would
    /// simply have it rejected on the far side. This check is about refusing to
    /// be a party to an unauthorized write, not about vouching for the data.
    pub fn put_device_dir(&self, account_id: &str, body: &[u8]) -> Result<(), RelayError> {
        if body.len() > self.config.max_blob_bytes {
            return Err(RelayError::TooLarge);
        }
        if hush_core::DeviceList::verify(body, account_id).is_err() {
            return Err(RelayError::Unauthorized);
        }
        let now = Self::now_secs() as i64;
        let conn = self.conn.lock().map_err(|_| RelayError::Internal)?;
        conn.execute(
            "INSERT INTO device_dirs (account_id, blob, updated_unix) VALUES (?1, ?2, ?3)
             ON CONFLICT(account_id) DO UPDATE SET blob = ?2, updated_unix = ?3",
            params![account_id, body, now],
        )
        .map_err(|_| RelayError::Internal)?;
        Ok(())
    }

    /// Read an account's device list.
    ///
    /// Deliberately unauthenticated: a peer fetching a contact's list holds no
    /// credential for that account and does not need one. The signature is what
    /// makes the contents trustworthy, so access control here would buy nothing
    /// while making the list unreadable by the only parties that need it. The
    /// account id is an identity key hash, so the directory is not enumerable
    /// without already knowing the account.
    pub fn get_device_dir(&self, account_id: &str) -> Result<Vec<u8>, RelayError> {
        let conn = self.conn.lock().map_err(|_| RelayError::Internal)?;
        let mut stmt = conn
            .prepare("SELECT blob FROM device_dirs WHERE account_id = ?1")
            .map_err(|_| RelayError::Internal)?;
        let mut rows = stmt
            .query(params![account_id])
            .map_err(|_| RelayError::Internal)?;
        if let Some(row) = rows.next().map_err(|_| RelayError::Internal)? {
            let body: Vec<u8> = row.get(0).map_err(|_| RelayError::Internal)?;
            return Ok(body);
        }
        Err(RelayError::NotFound)
    }

    fn authorize_existing(&self, queue_id: &str, token: &str) -> Result<(), RelayError> {
        match self.load_queue(queue_id)? {
            Some(q) if q.burned => Err(RelayError::Burned),
            Some(q) if tokens_match(&q.token_hash, token) => Ok(()),
            Some(_) => Err(RelayError::Unauthorized),
            None => Err(RelayError::NotFound),
        }
    }

    fn purge_expired(&self) -> Result<(), RelayError> {
        let ttl = self.config.blob_ttl.as_secs() as i64;
        let cutoff = Self::now_secs() as i64 - ttl;
        let conn = self.conn.lock().map_err(|_| RelayError::Internal)?;
        conn.execute("DELETE FROM blobs WHERE created_at < ?1", params![cutoff])
            .map_err(|_| RelayError::Internal)?;
        Ok(())
    }
}

struct QueueRow {
    token_hash: Vec<u8>,
    burned: bool,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct BlobMeta {
    pub id: String,
    pub created_at: i64,
}
