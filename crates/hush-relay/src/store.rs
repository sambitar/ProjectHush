use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection};
use sha2::{Digest, Sha256};

use crate::auth::{hash_token, tokens_match};
use crate::config::RelayConfig;
use crate::error::RelayError;

/// Ceiling on the bytes one batched drain will return. Bounds a single
/// response when a deep queue meets large blobs; the client pages until a
/// drain comes back empty.
const DRAIN_MAX_PAGE_BYTES: usize = 8 * 1024 * 1024;

pub struct Store {
    conn: Mutex<Connection>,
    config: RelayConfig,
    /// queue_id -> (window_start_secs, count)
    rate: Mutex<std::collections::HashMap<String, (u64, u32)>>,
}

impl Store {
    pub fn open(path: &str, config: RelayConfig) -> Result<Self, RelayError> {
        let conn = Connection::open(path).map_err(|_| RelayError::Internal)?;

        // Durability and concurrency settings, applied before the schema.
        //
        // SQLite's defaults are a rollback journal with `synchronous=FULL`, which
        // makes every autocommitted write a journal create, two fsyncs and an
        // unlink. Measured on ext4 an 8-byte blob cost 22 ms to store and 30 ms
        // to delete; with the same code and the same requests against tmpfs —
        // only the sync removed — those became 1.15 ms and 0.95 ms. The cost was
        // the syscall, never the payload.
        //
        // WAL also lets a reader proceed while a write is open. The store keeps
        // one connection behind a mutex, so without this a `list_blobs` blocks a
        // concurrent `put_blob` outright: twenty writers to twenty distinct
        // queues measured no faster than twenty in sequence.
        //
        // `synchronous=NORMAL` under WAL syncs at checkpoints rather than on
        // every commit, so a crash can lose the last few transactions but cannot
        // corrupt the file. For a store-and-forward relay holding ciphertext the
        // sender also keeps, that is the right trade.
        let mode: String = conn
            .query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))
            .map_err(|_| RelayError::Internal)?;
        // A network filesystem cannot do WAL, and an in-memory database has no
        // journal to choose. Both still work, just without the win, so this warns
        // rather than refusing to start.
        if !mode.eq_ignore_ascii_case("wal") && !mode.eq_ignore_ascii_case("memory") {
            eprintln!("hush-relay: journal_mode is {mode}, not wal; writes will be slow");
        }
        conn.execute_batch("PRAGMA synchronous = NORMAL; PRAGMA busy_timeout = 5000;")
            .map_err(|_| RelayError::Internal)?;

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
            -- `purge_expired` runs on every read and deletes by age, so without
            -- this the sweep is a full scan of every stored blob, once per read.
            -- `drops` and `bulletin` already carry the equivalent
            -- (idx_drops_created, idx_bulletin_inserted); blobs was the one table
            -- that did not.
            CREATE INDEX IF NOT EXISTS idx_blobs_created ON blobs(created_at);
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
            -- Incoming drops: sealed containers keyed by a rendezvous id the
            -- member derives from their published drop key. No token, no
            -- directory: the id is the capability and the body is ciphertext.
            CREATE TABLE IF NOT EXISTS drops (
                id TEXT PRIMARY KEY,
                body BLOB NOT NULL,
                created_at INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_drops_created ON drops(created_at);
            -- The bulletin: one flat, append-only log of opaque blobs addressed
            -- to nobody. There is deliberately no queue column and no foreign
            -- key — the relay authorizes the publisher and then forgets which
            -- member that was, and burning a member's queue must not delete the
            -- corpus. `hash` is the content hash of the stored bytes: the relay
            -- can hash them anyway, so it costs no leak and makes a re-post
            -- idempotent instead of a second copy of the same blob.
            CREATE TABLE IF NOT EXISTS bulletin (
                seq INTEGER PRIMARY KEY,
                hash TEXT NOT NULL,
                body BLOB NOT NULL,
                inserted_at INTEGER NOT NULL
            );
            CREATE UNIQUE INDEX IF NOT EXISTS idx_bulletin_hash ON bulletin(hash);
            CREATE INDEX IF NOT EXISTS idx_bulletin_inserted ON bulletin(inserted_at);
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
            // The window is anchored on the first write in it, so what remains
            // is the rest of that window, not a fresh one.
            let elapsed = now.saturating_sub(entry.0);
            return Err(RelayError::RateLimited {
                retry_after: window.saturating_sub(elapsed).max(1),
            });
        }
        entry.1 += 1;
        Ok(())
    }

    /// The queue row, read on a connection the caller already holds.
    ///
    /// `load_queue` is the locking wrapper around this. It exists so a caller
    /// that wants its read and its write in one transaction does not have to
    /// drop the lock and take it again in between — which is what let a burn land
    /// between an authorize and the insert it authorized.
    fn load_queue_on(conn: &Connection, queue_id: &str) -> Result<Option<QueueRow>, RelayError> {
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

    fn load_queue(&self, queue_id: &str) -> Result<Option<QueueRow>, RelayError> {
        let conn = self.conn.lock().map_err(|_| RelayError::Internal)?;
        Self::load_queue_on(&conn, queue_id)
    }

    /// Bind an unclaimed queue to `token`, or check an existing one against it.
    ///
    /// Takes the connection rather than locking it, so `put_blob` can hold a
    /// single transaction across this and the insert it guards.
    fn authorize_on(conn: &Connection, queue_id: &str, token: &str) -> Result<(), RelayError> {
        match Self::load_queue_on(conn, queue_id)? {
            Some(q) if q.burned => Err(RelayError::Burned),
            Some(q) if tokens_match(&q.token_hash, token) => Ok(()),
            Some(_) => Err(RelayError::Unauthorized),
            None => {
                let hash = hash_token(token);
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
        let blob_id = hex::encode(Sha256::digest(body));
        let created = Self::now_secs() as i64;

        // Authorize, rate-check and insert as one transaction. Held together
        // because otherwise a `report_queue` can land in between: it takes the
        // same mutex, burns the queue and deletes that queue's blobs, and this
        // insert would then write a blob into a queue that is already burned —
        // and a burned queue refuses every read, so nothing would ever drain it.
        //
        // The rate check takes the `rate` mutex, making the lock order here
        // connection-then-rate. Nothing takes them in the other order: every
        // other `check_rate` caller has released the connection first.
        let mut conn = self.conn.lock().map_err(|_| RelayError::Internal)?;
        let tx = conn.transaction().map_err(|_| RelayError::Internal)?;
        Self::authorize_on(&tx, queue_id, token)?;
        self.check_rate(queue_id)?;
        tx.execute(
            "INSERT INTO blobs (id, queue_id, body, created_at) VALUES (?1, ?2, ?3, ?4)",
            params![blob_id, queue_id, body, created],
        )
        .map_err(|_| RelayError::Internal)?;
        tx.commit().map_err(|_| RelayError::Internal)?;
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

    /// Fetch a drain page: up to `limit` blobs in created order, NOT consumed.
    ///
    /// The batched half of the client's list-then-get walk: one request
    /// returns what used to cost one `list` plus one `get` per message. The
    /// page is left in place on purpose — the caller files what it fetched and
    /// then confirms exactly the ids it processed with [`Self::ack_drained`].
    /// Consuming here instead would drop the two hold cases the walk keeps:
    /// a payload for a conclave this vault has not joined yet, and a blob
    /// whose session is not ready — both must survive to be redelivered.
    ///
    /// Authorized with the read token, like every other read. Not
    /// rate-limited, like the bulletin drain: the PUT limit is what keeps a
    /// sender from filling the queue, and counting drains against it would
    /// make a poll compete with a delivery.
    pub fn drain_blobs(
        &self,
        queue_id: &str,
        read_token: &str,
        limit: usize,
    ) -> Result<Vec<(String, Vec<u8>)>, RelayError> {
        let conn = self.conn.lock().map_err(|_| RelayError::Internal)?;
        Self::authorize_read_on(&conn, queue_id, read_token)?;
        let mut stmt = conn
            .prepare("SELECT id, body FROM blobs WHERE queue_id = ?1 ORDER BY created_at")
            .map_err(|_| RelayError::Internal)?;
        let mut rows = stmt
            .query(params![queue_id])
            .map_err(|_| RelayError::Internal)?;
        let mut out: Vec<(String, Vec<u8>)> = Vec::new();
        let mut total = 0usize;
        while out.len() < limit {
            let Some(row) = rows.next().map_err(|_| RelayError::Internal)? else {
                break;
            };
            let id: String = row.get(0).map_err(|_| RelayError::Internal)?;
            let body: Vec<u8> = row.get(1).map_err(|_| RelayError::Internal)?;
            // The first blob always goes in, however large — a single blob
            // over the budget is a page of one, not a stuck queue.
            if !out.is_empty() && total + body.len() > DRAIN_MAX_PAGE_BYTES {
                break;
            }
            total += body.len();
            out.push((id, body));
        }
        Ok(out)
    }

    /// Confirm the ids a caller finished filing, deleting them in one
    /// transaction.
    ///
    /// This is the batched half of the per-message `delete`. Idempotent: an
    /// id another drain already removed deletes zero rows and still succeeds,
    /// so two overlapping drains cannot fail each other. Held blobs simply
    /// never arrive here and stay for the next poll. Authorized with the read
    /// token; every deleted id must also belong to this queue, so a token can
    /// never consume another queue's blobs by naming their ids.
    pub fn ack_drained(
        &self,
        queue_id: &str,
        read_token: &str,
        ids: &[String],
    ) -> Result<(), RelayError> {
        if ids.is_empty() {
            return Ok(());
        }
        let mut conn = self.conn.lock().map_err(|_| RelayError::Internal)?;
        let tx = conn.transaction().map_err(|_| RelayError::Internal)?;
        Self::authorize_read_on(&tx, queue_id, read_token)?;
        for id in ids {
            tx.execute(
                "DELETE FROM blobs WHERE queue_id = ?1 AND id = ?2",
                params![queue_id, id],
            )
            .map_err(|_| RelayError::Internal)?;
        }
        tx.commit().map_err(|_| RelayError::Internal)?;
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

    /// Zero a queue's heartbeat so peers report it offline at once.
    ///
    /// Authorized with the read token, the same one that heartbeats: only the
    /// queue's owner can mark itself offline. A peer holds the delivery token,
    /// which cannot clear this.
    pub fn clear_presence(&self, queue_id: &str, token: &str) -> Result<(), RelayError> {
        self.authorize_read_or_create(queue_id, token)?;
        let conn = self.conn.lock().map_err(|_| RelayError::Internal)?;
        conn.execute(
            "UPDATE queues SET presence_at = 0 WHERE id = ?1",
            params![queue_id],
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
        let mut conn = self.conn.lock().map_err(|_| RelayError::Internal)?;
        // Burn and delete as one unit. Split apart, a crash between them leaves
        // the queue burned with its blobs still in it — and a burned queue
        // refuses every read, so nothing would ever drain them.
        let tx = conn.transaction().map_err(|_| RelayError::Internal)?;
        tx.execute(
            "UPDATE queues SET burned = 1 WHERE id = ?1",
            params![queue_id],
        )
        .map_err(|_| RelayError::Internal)?;
        tx.execute("DELETE FROM blobs WHERE queue_id = ?1", params![queue_id])
            .map_err(|_| RelayError::Internal)?;
        tx.commit().map_err(|_| RelayError::Internal)?;
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
        let conn = self.conn.lock().map_err(|_| RelayError::Internal)?;
        Self::authorize_read_on(&conn, queue_id, read_token)
    }

    /// [`Self::authorize_read`] on a connection the caller already holds, so a
    /// caller that wants its read and its write in one transaction can keep
    /// both under a single commit — the batched drain needs its select and its
    /// deletes to land together.
    fn authorize_read_on(
        conn: &Connection,
        queue_id: &str,
        read_token: &str,
    ) -> Result<(), RelayError> {
        match Self::load_queue_on(conn, queue_id)? {
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
        let now = Self::now_secs() as i64;
        let cutoff = now - self.config.blob_ttl.as_secs() as i64;
        // The bulletin has its own, longer TTL: it is the corpus, not a drained
        // inbox, so one window cannot serve both.
        let bulletin_cutoff = now - self.config.bulletin_ttl.as_secs() as i64;
        let mut conn = self.conn.lock().map_err(|_| RelayError::Internal)?;
        // The three sweeps are one unit of work. Outside a transaction each is
        // its own autocommit, so a crash between them can leave one table
        // swept and another not.
        let tx = conn.transaction().map_err(|_| RelayError::Internal)?;
        tx.execute("DELETE FROM blobs WHERE created_at < ?1", params![cutoff])
            .map_err(|_| RelayError::Internal)?;
        tx.execute("DELETE FROM drops WHERE created_at < ?1", params![cutoff])
            .map_err(|_| RelayError::Internal)?;
        tx.execute(
            "DELETE FROM bulletin WHERE inserted_at < ?1",
            params![bulletin_cutoff],
        )
        .map_err(|_| RelayError::Internal)?;
        tx.commit().map_err(|_| RelayError::Internal)?;
        Ok(())
    }

    /// Park a sealed drop under its rendezvous id.
    ///
    /// The body is ciphertext and opaque — the relay cannot tell one container
    /// version from another, only its size. The id is the whole capability:
    /// whoever knows it can put here, which is what makes the drop mailbox
    /// work, and the per-id rate limit is what keeps that from being free.
    pub fn put_drop(&self, id: &str, body: &[u8]) -> Result<(), RelayError> {
        validate_drop_id(id)?;
        if body.len() > hush_core::MAX_DROP_BYTES {
            return Err(RelayError::TooLarge);
        }
        self.check_rate(&format!("drop:{id}"))?;
        self.purge_expired()?;
        let created = Self::now_secs() as i64;
        let conn = self.conn.lock().map_err(|_| RelayError::Internal)?;
        conn.execute(
            "INSERT OR REPLACE INTO drops (id, body, created_at) VALUES (?1, ?2, ?3)",
            params![id, body, created],
        )
        .map_err(|_| RelayError::Internal)?;
        Ok(())
    }

    /// Fetch a sealed drop. Deliberately non-consuming: a member's drain that
    /// crashes between fetch and open must find the drop still there, so the
    /// client deletes only after it has opened and filed the container.
    pub fn get_drop(&self, id: &str) -> Result<Option<Vec<u8>>, RelayError> {
        validate_drop_id(id)?;
        self.check_rate(&format!("drop:{id}"))?;
        self.purge_expired()?;
        let conn = self.conn.lock().map_err(|_| RelayError::Internal)?;
        let mut stmt = conn
            .prepare("SELECT body FROM drops WHERE id = ?1")
            .map_err(|_| RelayError::Internal)?;
        let mut rows = stmt.query(params![id]).map_err(|_| RelayError::Internal)?;
        match rows.next().map_err(|_| RelayError::Internal)? {
            Some(row) => Ok(Some(row.get(0).map_err(|_| RelayError::Internal)?)),
            None => Ok(None),
        }
    }

    /// Consume a drained drop. Idempotent: two of the member's devices may
    /// drain together, and the second delete finding an empty slot is success.
    pub fn delete_drop(&self, id: &str) -> Result<(), RelayError> {
        validate_drop_id(id)?;
        let conn = self.conn.lock().map_err(|_| RelayError::Internal)?;
        conn.execute("DELETE FROM drops WHERE id = ?1", params![id])
            .map_err(|_| RelayError::Internal)?;
        Ok(())
    }

    /// Append an opaque blob to the bulletin, or refresh it when those exact
    /// bytes are already there.
    ///
    /// Authorized with the queue's **read** token, the private half of the
    /// credential pair. The delivery token is public — it travels in invites —
    /// so accepting it here would let anyone holding an invite publish as that
    /// member and spend their rate budget. The queue is bound on the first
    /// publish (`authorize_read_or_create`), which is what lets a member publish
    /// before anyone has ever delivered to them, and it stamps no presence:
    /// publishing a zine is not a heartbeat.
    ///
    /// A re-post is idempotent on the content hash. Every holder's daemon runs
    /// the same re-post sweep against the same blobs, so without this the lane
    /// would grow one copy per holder per sweep; a duplicate refreshes the row's
    /// TTL instead, which is the sweep's purpose.
    pub fn put_bulletin(
        &self,
        queue_id: &str,
        read_token: &str,
        body: &[u8],
    ) -> Result<i64, RelayError> {
        if body.len() > self.config.bulletin_max_blob_bytes {
            return Err(RelayError::TooLarge);
        }
        self.authorize_read_or_create(queue_id, read_token)?;
        self.check_rate(&format!("bulletin:{queue_id}"))?;
        self.purge_expired()?;

        let hash = hex::encode(Sha256::digest(body));
        let now = Self::now_secs() as i64;
        let conn = self.conn.lock().map_err(|_| RelayError::Internal)?;
        // A re-post keeps its original seq: it is the same blob, and moving it
        // to the end would hand every drainer a second copy of what it has.
        conn.execute(
            "INSERT INTO bulletin (hash, body, inserted_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(hash) DO UPDATE SET inserted_at = ?3",
            params![hash, body, now],
        )
        .map_err(|_| RelayError::Internal)?;
        conn.query_row(
            "SELECT seq FROM bulletin WHERE hash = ?1",
            params![hash],
            |row| row.get(0),
        )
        .map_err(|_| RelayError::Internal)
    }

    /// Drain the bulletin: every blob after `since`, oldest first.
    ///
    /// Authorized with the read token, like the publish. Everyone drains the
    /// same lane, so what a member drains says nothing about what they read —
    /// the reason the bulletin is not a queue per reader.
    ///
    /// Not rate-limited: the publish limit is what keeps a member from filling
    /// the lane, and a drain is a read the HTTP layer can throttle like any
    /// other. Counting drains against the same budget would make a poll tick
    /// compete with a publish.
    pub fn list_bulletin(
        &self,
        queue_id: &str,
        read_token: &str,
        since: i64,
    ) -> Result<Vec<BulletinBlob>, RelayError> {
        self.authorize_read_or_create(queue_id, read_token)?;
        self.purge_expired()?;
        let conn = self.conn.lock().map_err(|_| RelayError::Internal)?;
        let mut stmt = conn
            .prepare("SELECT seq, body, inserted_at FROM bulletin WHERE seq > ?1 ORDER BY seq")
            .map_err(|_| RelayError::Internal)?;
        let rows = stmt
            .query_map(params![since], |row| {
                Ok(BulletinBlob {
                    seq: row.get(0)?,
                    body: row.get(1)?,
                    inserted_at: row.get(2)?,
                })
            })
            .map_err(|_| RelayError::Internal)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(|_| RelayError::Internal)?);
        }
        Ok(out)
    }
}

/// A drop id is exactly 32 lowercase hex characters — the hex of a 16-byte
/// HMAC truncation. Anything else is not an address this relay handed out.
fn validate_drop_id(id: &str) -> Result<(), RelayError> {
    let ok = id.len() == 32
        && id.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
    if ok {
        Ok(())
    } else {
        Err(RelayError::BadRequest)
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

/// One blob as it comes off the bulletin.
///
/// Carries no publisher: the row it came from has no such column. `seq` is the
/// drain cursor — a client stores the highest it has processed and asks for
/// everything after it next time.
#[derive(Clone, Debug)]
pub struct BulletinBlob {
    pub seq: i64,
    pub body: Vec<u8>,
    pub inserted_at: i64,
}
