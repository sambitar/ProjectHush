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
            -- Left over from the multi-device model. An account has one device
            -- now, so the directory is gone and the row is dropped on upgrade;
            -- it held only public key material, so nothing is lost.
            DROP TABLE IF EXISTS device_dirs;
            -- One prekey record per account. The composite key survives from the
            -- multi-device schema because SQLite cannot add a unique constraint
            -- to an existing table; `device_id` is always the account's one
            -- device, kept only so an existing database keeps working.
            CREATE TABLE IF NOT EXISTS prekey_records (
                account_id TEXT NOT NULL,
                device_id INTEGER NOT NULL,
                blob BLOB NOT NULL,
                updated_unix INTEGER NOT NULL,
                PRIMARY KEY (account_id, device_id)
            );
            -- Left over from outsider mail (hush-drop-v1 deliveries). Mail is
            -- members-only now, so the table is gone; the rows were ciphertext
            -- with a TTL, so nothing durable is lost.
            DROP TABLE IF EXISTS deliveries;
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
        self.authorize_read(queue_id, token)?;
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
        self.authorize_read(queue_id, token)?;
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
        self.authorize_read(queue_id, token)?;
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
        self.authorize_read_or_create(queue_id, token)?;
        let now = Self::now_secs() as i64;
        let conn = self.conn.lock().map_err(|_| RelayError::Internal)?;
        conn.execute(
            "UPDATE queues SET presence_at = ?1 WHERE id = ?2",
            params![now, queue_id],
        )
        .map_err(|_| RelayError::Internal)?;
        Ok(())
    }

    /// Read a queue's last heartbeat.
    ///
    /// Authorized with the *delivery* token, not the read token: presence is
    /// something a peer checks about a contact, and a peer only ever holds the
    /// delivery token. It reveals whether the contact is online, which the peer
    /// already learns by trying to deliver.
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
        self.authorize_read(queue_id, token)?;
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

    /// Store an account's published prekey record.
    ///
    /// Verified against the account's own identity key, which `account_id` *is*
    /// the hex of. The record carries that key and signs its signed prekey and
    /// kyber prekey with the matching private key, so accepting it proves the
    /// publisher holds the identity private key the account id names. That is
    /// what authorizes the write: knowing a public account id is not enough to
    /// replace that account's prekeys. There is no bearer credential and no
    /// device list involved, so there is nothing to rotate on revocation.
    pub fn put_prekey_record(&self, account_id: &str, body: &[u8]) -> Result<(), RelayError> {
        if body.len() > self.config.max_blob_bytes {
            return Err(RelayError::TooLarge);
        }
        let record = hush_core::PreKeyRecord::decode(body).map_err(|_| RelayError::BadRequest)?;
        let expected = hex::decode(account_id).map_err(|_| RelayError::BadRequest)?;
        record
            .verify(&expected)
            .map_err(|_| RelayError::Unauthorized)?;

        let now = Self::now_secs() as i64;
        let conn = self.conn.lock().map_err(|_| RelayError::Internal)?;
        conn.execute(
            "INSERT INTO prekey_records (account_id, device_id, blob, updated_unix)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(account_id, device_id) DO UPDATE SET blob = ?3, updated_unix = ?4",
            params![account_id, hush_core::DEVICE_ID, body, now],
        )
        .map_err(|_| RelayError::Internal)?;
        Ok(())
    }

    /// Take an account's prekey record, consuming one one-time prekey.
    ///
    /// This is deliberately a take rather than a read. A one-time prekey is only
    /// one-time if a second fetch cannot get it, so the pop and the write-back
    /// happen under the same lock: two peers fetching at once must not walk away
    /// with the same key. If the pool is empty the record is returned unchanged
    /// and the peer falls back to the signed prekey.
    pub fn take_prekey_record(&self, account_id: &str) -> Result<Vec<u8>, RelayError> {
        let conn = self.conn.lock().map_err(|_| RelayError::Internal)?;
        let stored: Vec<u8> = {
            let mut stmt = conn
                .prepare("SELECT blob FROM prekey_records WHERE account_id = ?1 AND device_id = ?2")
                .map_err(|_| RelayError::Internal)?;
            let mut rows = stmt
                .query(params![account_id, hush_core::DEVICE_ID])
                .map_err(|_| RelayError::Internal)?;
            match rows.next().map_err(|_| RelayError::Internal)? {
                Some(row) => row.get(0).map_err(|_| RelayError::Internal)?,
                None => return Err(RelayError::NotFound),
            }
        };

        let mut stored_record =
            hush_core::PreKeyRecord::decode(&stored).map_err(|_| RelayError::Internal)?;
        // Pop first, then write back the remainder: the popped key goes to the
        // caller and must not be left in storage, or the next peer would get it
        // too. Returning the record as-is after popping would hand the peer a
        // pool that has quietly lost its next key.
        let taken = stored_record.pop_one_time_prekey();
        if taken.is_some() {
            let next = stored_record.encode().map_err(|_| RelayError::Internal)?;
            conn.execute(
                "UPDATE prekey_records SET blob = ?3 WHERE account_id = ?1 AND device_id = ?2",
                params![account_id, hush_core::DEVICE_ID, next],
            )
            .map_err(|_| RelayError::Internal)?;
        }

        let mut response = stored_record;
        if let Some(key) = taken {
            response.one_time_prekeys = vec![key];
        }
        response.encode().map_err(|_| RelayError::Internal)
    }

    fn authorize_existing(&self, queue_id: &str, token: &str) -> Result<(), RelayError> {
        match self.load_queue(queue_id)? {
            Some(q) if q.burned => Err(RelayError::Burned),
            Some(q) if tokens_match(&q.token_hash, token) => Ok(()),
            Some(_) => Err(RelayError::Unauthorized),
            None => Err(RelayError::NotFound),
        }
    }

    /// Authorize a *read* action — list, read, delete, burn — with the queue's
    /// private read token.
    ///
    /// The stored hash is always over the *delivery* token, which is the public
    /// one that travels in invites. A read request presents the read token
    /// instead, and is accepted only when the delivery token derived from it is
    /// what the queue was bound to.
    ///
    /// Deliberately does *not* also accept the presented token verbatim. That
    /// fallback is what the split exists to remove: it would let the published
    /// delivery token list, delete and burn the inbox.
    fn authorize_read(&self, queue_id: &str, read_token: &str) -> Result<(), RelayError> {
        match self.load_queue(queue_id)? {
            Some(q) if q.burned => Err(RelayError::Burned),
            Some(q) => {
                let delivery = hush_core::queue_delivery_token(read_token);
                if tokens_match(&q.token_hash, &delivery) {
                    Ok(())
                } else {
                    Err(RelayError::Unauthorized)
                }
            }
            None => Err(RelayError::NotFound),
        }
    }

    /// Authorize a read action, creating the queue when it does not exist yet.
    ///
    /// This is how the owner binds its own queue: the row is stored against the
    /// delivery token derived from the read token, so a later delivery from a
    /// peer — which presents the delivery token — matches. Only the owner can
    /// perform this first step, because only the owner holds a read token whose
    /// MAC is the delivery token.
    fn authorize_read_or_create(
        &self,
        queue_id: &str,
        read_token: &str,
    ) -> Result<(), RelayError> {
        let delivery = hush_core::queue_delivery_token(read_token);
        match self.load_queue(queue_id)? {
            Some(q) if q.burned => Err(RelayError::Burned),
            Some(q) if tokens_match(&q.token_hash, &delivery) => Ok(()),
            Some(_) => Err(RelayError::Unauthorized),
            None => {
                let hash = hash_token(&delivery);
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
