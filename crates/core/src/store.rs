//! The inbox: one SQLite file per helper.
//!
//! Envelopes and content live in separate tables so a delete leaves a
//! tombstone and the chain still proves nothing else changed. Each reader
//! keeps its own bookmark. Reading never deletes. A message is on disk
//! before `send` returns. Delivery is at-least-once, dedup by id.

use std::path::Path;
use std::sync::Mutex;

use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use rusqlite::{params, Connection, OptionalExtension, Row};
use rusqlite_migration::{Migrations, M};

use crate::error::{Error, Result};
use crate::faults::Faults;
use crate::invite::Invite;
use crate::keys::{Kind, PublicKey};
use crate::limits::Limits;
use crate::message::{Content, DataClass, Message, MessageType, GENESIS_PREV};
use crate::room::{Member, Role, Room};

const MIGRATIONS: &[M<'static>] = &[
    M::up(
        r#"
    CREATE TABLE rooms (
      id TEXT PRIMARY KEY,
      name TEXT NOT NULL UNIQUE,
      about TEXT NOT NULL DEFAULT '',
      owner TEXT NOT NULL,
      created TEXT NOT NULL,
      retention_days INTEGER,
      class TEXT NOT NULL DEFAULT 'internal',
      paused INTEGER NOT NULL DEFAULT 0,
      hold INTEGER NOT NULL DEFAULT 0,
      home_node TEXT NOT NULL,
      home_hints TEXT NOT NULL DEFAULT 'null'
    );
    CREATE TABLE members (
      room_id TEXT NOT NULL REFERENCES rooms(id) ON DELETE CASCADE,
      name TEXT NOT NULL,
      key TEXT NOT NULL,
      kind TEXT NOT NULL,
      role TEXT NOT NULL,
      node TEXT,
      granted_by TEXT NOT NULL,
      expires_at TEXT,
      joined_at TEXT NOT NULL,
      last_seen TEXT,
      muted INTEGER NOT NULL DEFAULT 0,
      revoked INTEGER NOT NULL DEFAULT 0,
      profile TEXT NOT NULL DEFAULT 'null',
      identity TEXT,
      PRIMARY KEY (room_id, name)
    );
    CREATE UNIQUE INDEX members_key ON members(room_id, key);
    CREATE TABLE messages (
      room_id TEXT NOT NULL REFERENCES rooms(id) ON DELETE CASCADE,
      seq INTEGER NOT NULL,
      id TEXT NOT NULL UNIQUE,
      from_name TEXT NOT NULL,
      type TEXT NOT NULL,
      ts TEXT NOT NULL,
      prev TEXT NOT NULL,
      chain_hash TEXT NOT NULL,
      envelope TEXT NOT NULL,
      PRIMARY KEY (room_id, seq)
    );
    CREATE INDEX messages_room_from_ts ON messages(room_id, from_name, ts);
    CREATE INDEX messages_room_ts ON messages(room_id, ts);
    CREATE TABLE contents (
      msg_id TEXT PRIMARY KEY REFERENCES messages(id) ON DELETE CASCADE,
      body TEXT NOT NULL,
      deleted INTEGER NOT NULL DEFAULT 0
    );
    CREATE TABLE bookmarks (
      room_id TEXT NOT NULL,
      reader TEXT NOT NULL,
      seq INTEGER NOT NULL DEFAULT 0,
      PRIMARY KEY (room_id, reader)
    );
    CREATE TABLE outbox (
      msg_id TEXT PRIMARY KEY,
      room_id TEXT NOT NULL,
      message TEXT NOT NULL,
      created TEXT NOT NULL,
      attempts INTEGER NOT NULL DEFAULT 0,
      last_error TEXT
    );
    CREATE TABLE invites (
      nonce TEXT PRIMARY KEY,
      room_id TEXT NOT NULL,
      name TEXT NOT NULL,
      created TEXT NOT NULL,
      expires TEXT NOT NULL,
      used_at TEXT,
      used_by TEXT
    );
    "#,
    ),
    M::up(
        r#"
    ALTER TABLE rooms ADD COLUMN closed INTEGER NOT NULL DEFAULT 0;
    ALTER TABLE messages ADD COLUMN reply_to TEXT;
    UPDATE messages SET reply_to = json_extract(envelope, '$.reply_to');
    CREATE INDEX messages_reply_to ON messages(room_id, reply_to);
    CREATE TABLE approvals_used (
      msg_id TEXT PRIMARY KEY,
      action_hash TEXT NOT NULL,
      used_at TEXT NOT NULL
    );
    "#,
    ),
    // `received` is this helper's own clock when it stored the message.
    // Limits count by it: `ts` is the sender's word and can be backdated.
    M::up(
        r#"
    ALTER TABLE messages ADD COLUMN received TEXT;
    UPDATE messages SET received = ts;
    CREATE INDEX messages_room_from_received ON messages(room_id, from_name, received);
    CREATE INDEX messages_room_received ON messages(room_id, received);
    "#,
    ),
    // The outbox keeps every message it accepted until the home has it in
    // the chain or someone drops it on purpose. `sender` is the lane: one
    // sender's stuck message holds back only that sender's later ones.
    M::up(
        r#"
    ALTER TABLE outbox ADD COLUMN sender TEXT NOT NULL DEFAULT '';
    ALTER TABLE outbox ADD COLUMN state TEXT NOT NULL DEFAULT 'pending';
    ALTER TABLE outbox ADD COLUMN retry_at TEXT;
    ALTER TABLE outbox ADD COLUMN reason_class TEXT;
    ALTER TABLE outbox ADD COLUMN reason_code INTEGER;
    ALTER TABLE outbox ADD COLUMN unknown_code INTEGER;
    ALTER TABLE outbox ADD COLUMN unknown_since TEXT;
    ALTER TABLE outbox ADD COLUMN unknown_count INTEGER NOT NULL DEFAULT 0;
    ALTER TABLE outbox ADD COLUMN updated TEXT;
    ALTER TABLE outbox ADD COLUMN sig TEXT;
    UPDATE outbox SET
      sender = CASE WHEN json_valid(message)
        THEN COALESCE(json_extract(message, '$.from'), '') ELSE '' END,
      sig = CASE WHEN json_valid(message) THEN json_extract(message, '$.sig') END;
    CREATE INDEX outbox_room_state ON outbox(room_id, state, sender, created, msg_id);
    CREATE TABLE meta (
      key TEXT PRIMARY KEY,
      value TEXT NOT NULL
    );
    "#,
    ),
    // A message handed to a reader stays that reader's until it settles
    // it. The bookmark is the settled prefix; it never passes a message
    // still owed.
    M::up(
        r#"
    CREATE TABLE deliveries (
      room_id TEXT NOT NULL,
      reader TEXT NOT NULL,
      seq INTEGER NOT NULL,
      state TEXT NOT NULL,
      token TEXT UNIQUE,
      lease_until TEXT,
      retry_at TEXT,
      attempt INTEGER NOT NULL DEFAULT 0,
      updated TEXT NOT NULL,
      PRIMARY KEY (room_id, reader, seq)
    );
    "#,
    ),
];

/// How many legacy outbox rows are sealed per transaction.
const SEAL_BATCH: usize = 100;

/// The store. Safe to share between threads; one connection, one lock.
pub struct Store {
    conn: Mutex<Connection>,
    /// When set, message content and queued messages are encrypted at
    /// rest. Envelopes stay plain: the chain is public inside the room
    /// anyway.
    cipher: Option<XChaCha20Poly1305>,
    /// Test-only failure points. Never armed in a real helper.
    pub faults: Faults,
}

const ENC_PREFIX: &str = "enc1:";

impl std::fmt::Debug for Store {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Store")
    }
}

/// A local member: a member of a room whose key this helper holds.
#[derive(Debug, Clone)]
pub struct LocalMember {
    pub member: Member,
    /// The identity file name.
    pub identity: String,
}

/// Where a queued message stands. See the outbox section of `Store`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OutboxState {
    /// Queued; due now.
    Pending,
    /// Failed for a reason that can clear; due again at `retry_at`.
    Waiting,
    /// The home refused it for good.
    Failed,
    /// The same unknown answer kept coming back.
    Quarantined,
    /// Given up on, on purpose. Content wiped.
    Dropped,
}

impl OutboxState {
    pub fn as_str(&self) -> &'static str {
        match self {
            OutboxState::Pending => "pending",
            OutboxState::Waiting => "waiting",
            OutboxState::Failed => "failed",
            OutboxState::Quarantined => "quarantined",
            OutboxState::Dropped => "dropped",
        }
    }
}

impl std::str::FromStr for OutboxState {
    type Err = Error;
    fn from_str(s: &str) -> Result<Self> {
        Ok(match s {
            "pending" => OutboxState::Pending,
            "waiting" => OutboxState::Waiting,
            "failed" => OutboxState::Failed,
            "quarantined" => OutboxState::Quarantined,
            "dropped" => OutboxState::Dropped,
            other => return Err(Error::Invalid(format!("unknown outbox state {other}"))),
        })
    }
}

/// One queued message and what has happened to it.
#[derive(Debug, Clone)]
pub struct OutboxEntry {
    pub msg_id: String,
    pub room_id: String,
    /// The member name that signed it: the lane.
    pub sender: String,
    pub state: OutboxState,
    pub created: String,
    pub attempts: u32,
    pub retry_at: Option<String>,
    /// The last error, as text.
    pub reason: Option<String>,
    /// transport, paused, budget, home, refused or unknown.
    pub reason_class: Option<String>,
    pub reason_code: Option<i32>,
    pub updated: Option<String>,
    /// The signed message. `None` once dropped, or if it cannot be
    /// decrypted with this helper's key.
    pub message: Option<Message>,
    /// There is content, but this helper cannot read it.
    pub unreadable: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct OutboxCounts {
    pub pending: u64,
    pub waiting: u64,
    pub failed: u64,
    pub quarantined: u64,
    pub dropped: u64,
}

impl OutboxCounts {
    /// Still to send.
    pub fn queued(&self) -> u64 {
        self.pending + self.waiting
    }
}

/// Where a message stands for one reader. No row means available.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DeliveryState {
    /// Handed out; only the holder of `token` can settle it until
    /// `lease_until`. After that it is available again.
    Leased,
    /// The reader took it on. Not "done": finishing a task is a `done`
    /// message in the room.
    Acked,
    /// Handed back to try later; available again at `retry_at`.
    Delayed,
    /// Handed out the most times allowed and never settled. Out of the
    /// way, kept, and never skipped silently: `replay` brings it back.
    Quarantined,
    /// Brought back from quarantine; available now.
    Replay,
}

impl DeliveryState {
    pub fn as_str(&self) -> &'static str {
        match self {
            DeliveryState::Leased => "leased",
            DeliveryState::Acked => "acked",
            DeliveryState::Delayed => "delayed",
            DeliveryState::Quarantined => "quarantined",
            DeliveryState::Replay => "replay",
        }
    }

    fn parse(s: &str) -> DeliveryState {
        match s {
            "acked" => DeliveryState::Acked,
            "delayed" => DeliveryState::Delayed,
            "quarantined" => DeliveryState::Quarantined,
            "replay" => DeliveryState::Replay,
            _ => DeliveryState::Leased,
        }
    }
}

/// One message handed to one reader.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Delivery {
    pub room_id: String,
    pub reader: String,
    pub seq: u64,
    pub state: DeliveryState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lease_until: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_at: Option<String>,
    /// How many times it has been handed out.
    pub attempt: u32,
    pub updated: String,
}

/// How a reader settles a delivery it holds.
#[derive(Debug, Clone)]
pub enum Settle {
    /// Taken on.
    Ack,
    /// Still working: hold it until this time.
    Renew { lease_until: String },
    /// Not now: hand it out again at this time.
    Nack { retry_at: String },
}

/// A message after the bookmark and where it stands for one reader: seq,
/// sender, type, delivery state, lease_until, retry_at.
type Candidate = (
    i64,
    String,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
);

/// Does `reader` want to be handed this message? Not its own, not helper
/// or owner housekeeping. The rest settle by themselves.
fn wanted(from: &str, kind: &str, reader: &str) -> bool {
    from != reader && kind != "system" && kind != "control"
}

/// Whole seconds from `a` to `b`, both RFC 3339. 0 if either is unreadable.
fn seconds_between(a: &str, b: &str) -> i64 {
    match (
        chrono::DateTime::parse_from_rfc3339(a),
        chrono::DateTime::parse_from_rfc3339(b),
    ) {
        (Ok(a), Ok(b)) => (b - a).num_seconds(),
        _ => 0,
    }
}

impl Store {
    /// Open (or create) the database file and bring the schema up to date.
    /// Mixed versions are normal; migrations run from v0.1 on.
    pub fn open(path: &Path) -> Result<Self> {
        Self::open_with_key(path, None)
    }

    /// Open with a 32-byte key: message content is then encrypted at rest.
    /// Content written before the key was set is still readable.
    ///
    /// Queued messages written in plain text by an older version are
    /// sealed now, a batch per transaction. An interrupted run loses
    /// nothing: plain rows stay readable and the next open carries on.
    pub fn open_with_key(path: &Path, key: Option<[u8; 32]>) -> Result<Self> {
        let store = Self::open_unsealed(path, key)?;
        store.seal_legacy_outbox(SEAL_BATCH)?;
        Ok(store)
    }

    /// Open without sealing legacy outbox rows yet. For tests that stop
    /// the sealing part way.
    #[doc(hidden)]
    pub fn open_unsealed(path: &Path, key: Option<[u8; 32]>) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path)?;
        Self::init(conn, key)
    }

    /// An in-memory store for tests.
    pub fn open_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?, None)
    }

    /// An in-memory encrypted store for tests.
    pub fn open_memory_with_key(key: [u8; 32]) -> Result<Self> {
        Self::init(Connection::open_in_memory()?, Some(key))
    }

    fn init(mut conn: Connection, key: Option<[u8; 32]>) -> Result<Self> {
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "FULL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        // Deleted rows are overwritten with zeros, not just unlinked. This
        // reduces what is left in the file; it does not reach backups,
        // snapshots or copies below the filesystem.
        conn.pragma_update(None, "secure_delete", "ON")?;
        Migrations::from_slice(MIGRATIONS).to_latest(&mut conn)?;
        Ok(Store {
            conn: Mutex::new(conn),
            cipher: key.map(|k| XChaCha20Poly1305::new((&k).into())),
            faults: Faults::default(),
        })
    }

    /// Cap the database at its current size plus `extra` pages, so the
    /// next writes fail as they would on a full disk.
    #[doc(hidden)]
    pub fn cap_size_for_test(&self, extra: u32) -> Result<()> {
        let conn = self.lock();
        let pages: i64 = conn.query_row("PRAGMA page_count", [], |r| r.get(0))?;
        conn.pragma_update(None, "max_page_count", pages + extra as i64)?;
        Ok(())
    }

    /// Fold the write-ahead log back into the database file and empty it,
    /// so pages of deleted rows do not linger there. Cheap when idle; the
    /// helper runs it on a slow tick and at shutdown.
    pub fn checkpoint(&self) -> Result<()> {
        let conn = self.lock();
        conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))?;
        Ok(())
    }

    /// Seal outbox rows an older version wrote in plain text, `batch` rows
    /// per transaction. Returns how many were sealed. A no-op without a key.
    pub fn seal_legacy_outbox(&self, batch: usize) -> Result<u64> {
        if self.cipher.is_none() {
            return Ok(0);
        }
        let mut sealed = 0u64;
        loop {
            let mut conn = self.lock();
            let tx = conn.transaction()?;
            let rows: Vec<(String, String)> = {
                let mut stmt = tx.prepare(
                    "SELECT msg_id, message FROM outbox
                     WHERE message != '' AND message NOT LIKE 'enc1:%' LIMIT ?1",
                )?;
                let rows = stmt.query_map(params![batch as i64], |r| Ok((r.get(0)?, r.get(1)?)))?;
                rows.collect::<rusqlite::Result<Vec<_>>>()?
            };
            if rows.is_empty() {
                tx.execute(
                    "INSERT INTO meta (key, value) VALUES ('outbox_sealed', ?1)
                     ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                    params![crate::message::now_ts()],
                )?;
                tx.commit()?;
                return Ok(sealed);
            }
            for (id, plain) in rows {
                self.faults.check("migration.row")?;
                tx.execute(
                    "UPDATE outbox SET message=?2 WHERE msg_id=?1",
                    params![id, self.seal(&plain)?],
                )?;
                sealed += 1;
            }
            tx.commit()?;
        }
    }

    /// True if content is encrypted at rest.
    pub fn is_encrypted(&self) -> bool {
        self.cipher.is_some()
    }

    fn seal(&self, body: &str) -> Result<String> {
        match &self.cipher {
            None => Ok(body.to_string()),
            Some(c) => {
                let nonce_bytes: [u8; 24] = rand::random();
                let nonce = XNonce::from(nonce_bytes);
                let ct = c
                    .encrypt(&nonce, body.as_bytes())
                    .map_err(|_| Error::Other("encrypt failed".into()))?;
                let mut out = nonce_bytes.to_vec();
                out.extend_from_slice(&ct);
                Ok(format!(
                    "{ENC_PREFIX}{}",
                    data_encoding::BASE64.encode(&out)
                ))
            }
        }
    }

    fn unseal(&self, stored: &str) -> Option<String> {
        let Some(b64) = stored.strip_prefix(ENC_PREFIX) else {
            return Some(stored.to_string());
        };
        let c = self.cipher.as_ref()?;
        let raw = data_encoding::BASE64.decode(b64.as_bytes()).ok()?;
        if raw.len() < 24 {
            return None;
        }
        let (n, ct) = raw.split_at(24);
        let nonce = XNonce::from(<[u8; 24]>::try_from(n).ok()?);
        let pt = c.decrypt(&nonce, ct).ok()?;
        String::from_utf8(pt).ok()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|p| p.into_inner())
    }

    // ---- rooms ---------------------------------------------------------

    pub fn create_room(&self, room: &Room) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "INSERT INTO rooms (id, name, about, owner, created, retention_days, class, paused, hold, home_node, home_hints)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                room.id,
                room.name,
                room.about,
                room.owner.to_string(),
                room.created,
                room.retention_days,
                room.class.as_str(),
                room.paused as i32,
                room.hold as i32,
                room.home_node,
                serde_json::to_string(&room.home_hints)?,
            ],
        )
        .map_err(|e| match e {
            rusqlite::Error::SqliteFailure(f, _) if f.code == rusqlite::ErrorCode::ConstraintViolation => {
                Error::NameTaken(format!("a room named {} already exists here", room.name))
            }
            other => Error::Db(other),
        })?;
        Ok(())
    }

    pub fn update_room(&self, room: &Room) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "UPDATE rooms SET about=?2, retention_days=?3, class=?4, paused=?5, hold=?6, home_node=?7, home_hints=?8, closed=?9 WHERE id=?1",
            params![
                room.id,
                room.about,
                room.retention_days,
                room.class.as_str(),
                room.paused as i32,
                room.hold as i32,
                room.home_node,
                serde_json::to_string(&room.home_hints)?,
                room.closed as i32,
            ],
        )?;
        Ok(())
    }

    fn row_to_room(row: &Row<'_>) -> rusqlite::Result<Room> {
        let owner: String = row.get("owner")?;
        let class: String = row.get("class")?;
        let hints: String = row.get("home_hints")?;
        Ok(Room {
            id: row.get("id")?,
            name: row.get("name")?,
            about: row.get("about")?,
            owner: owner.parse().map_err(|_| rusqlite::Error::InvalidQuery)?,
            created: row.get("created")?,
            retention_days: row.get("retention_days")?,
            class: class.parse().unwrap_or(DataClass::Internal),
            paused: row.get::<_, i32>("paused")? != 0,
            hold: row.get::<_, i32>("hold")? != 0,
            closed: row.get::<_, i32>("closed").unwrap_or(0) != 0,
            home_node: row.get("home_node")?,
            home_hints: serde_json::from_str(&hints).unwrap_or(serde_json::Value::Null),
        })
    }

    pub fn room_by_name(&self, name: &str) -> Result<Option<Room>> {
        let conn = self.lock();
        Ok(conn
            .query_row(
                "SELECT * FROM rooms WHERE name=?1",
                params![name],
                Self::row_to_room,
            )
            .optional()?)
    }

    pub fn room_by_id(&self, id: &str) -> Result<Option<Room>> {
        let conn = self.lock();
        Ok(conn
            .query_row(
                "SELECT * FROM rooms WHERE id=?1",
                params![id],
                Self::row_to_room,
            )
            .optional()?)
    }

    /// Room by name or id. Name first.
    pub fn room(&self, name_or_id: &str) -> Result<Room> {
        if let Some(r) = self.room_by_name(name_or_id)? {
            return Ok(r);
        }
        if let Some(r) = self.room_by_id(name_or_id)? {
            return Ok(r);
        }
        Err(Error::NotInRoom(name_or_id.to_string()))
    }

    pub fn list_rooms(&self) -> Result<Vec<Room>> {
        let conn = self.lock();
        let mut stmt = conn.prepare("SELECT * FROM rooms ORDER BY created")?;
        let rows = stmt.query_map([], Self::row_to_room)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    // ---- members -------------------------------------------------------

    /// Insert or replace a member. `identity` is the local key file name if
    /// this helper holds the member's key.
    pub fn upsert_member(&self, m: &Member, identity: Option<&str>) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "INSERT INTO members (room_id, name, key, kind, role, node, granted_by, expires_at, joined_at, last_seen, muted, revoked, profile, identity)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
             ON CONFLICT(room_id, name) DO UPDATE SET
               key=excluded.key, kind=excluded.kind, role=excluded.role, node=excluded.node,
               granted_by=excluded.granted_by, expires_at=excluded.expires_at,
               last_seen=COALESCE(excluded.last_seen, members.last_seen),
               muted=excluded.muted, revoked=excluded.revoked, profile=excluded.profile,
               identity=COALESCE(excluded.identity, members.identity)",
            params![
                m.room_id,
                m.name,
                m.key.to_string(),
                m.kind.to_string(),
                m.role.as_str(),
                m.node,
                m.granted_by.to_string(),
                m.expires_at,
                m.joined_at,
                m.last_seen,
                m.muted as i32,
                m.revoked as i32,
                serde_json::to_string(&m.profile)?,
                identity,
            ],
        )?;
        Ok(())
    }

    fn row_to_member(row: &Row<'_>) -> rusqlite::Result<Member> {
        let key: String = row.get("key")?;
        let kind: String = row.get("kind")?;
        let role: String = row.get("role")?;
        let granted_by: String = row.get("granted_by")?;
        let profile: String = row.get("profile")?;
        Ok(Member {
            room_id: row.get("room_id")?,
            name: row.get("name")?,
            key: key.parse().map_err(|_| rusqlite::Error::InvalidQuery)?,
            kind: kind.parse().unwrap_or(Kind::Agent),
            role: role.parse().unwrap_or(Role::Observer),
            node: row.get("node")?,
            granted_by: granted_by
                .parse()
                .map_err(|_| rusqlite::Error::InvalidQuery)?,
            expires_at: row.get("expires_at")?,
            joined_at: row.get("joined_at")?,
            last_seen: row.get("last_seen")?,
            muted: row.get::<_, i32>("muted")? != 0,
            revoked: row.get::<_, i32>("revoked")? != 0,
            profile: serde_json::from_str(&profile).unwrap_or(serde_json::Value::Null),
        })
    }

    pub fn member_by_name(&self, room_id: &str, name: &str) -> Result<Option<Member>> {
        let conn = self.lock();
        Ok(conn
            .query_row(
                "SELECT * FROM members WHERE room_id=?1 AND name=?2",
                params![room_id, name],
                Self::row_to_member,
            )
            .optional()?)
    }

    pub fn member_by_key(&self, room_id: &str, key: &PublicKey) -> Result<Option<Member>> {
        let conn = self.lock();
        Ok(conn
            .query_row(
                "SELECT * FROM members WHERE room_id=?1 AND key=?2",
                params![room_id, key.to_string()],
                Self::row_to_member,
            )
            .optional()?)
    }

    pub fn members(&self, room_id: &str) -> Result<Vec<Member>> {
        let conn = self.lock();
        let mut stmt =
            conn.prepare("SELECT * FROM members WHERE room_id=?1 ORDER BY joined_at, name")?;
        let rows = stmt.query_map(params![room_id], Self::row_to_member)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Members of a room whose keys live on this helper.
    pub fn local_members(&self, room_id: &str) -> Result<Vec<LocalMember>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT * FROM members WHERE room_id=?1 AND identity IS NOT NULL ORDER BY joined_at",
        )?;
        let rows = stmt.query_map(params![room_id], |row| {
            Ok(LocalMember {
                member: Self::row_to_member(row)?,
                identity: row.get("identity")?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// The local member of a room for a given identity file, if any.
    pub fn local_member(&self, room_id: &str, identity: &str) -> Result<Option<LocalMember>> {
        let conn = self.lock();
        Ok(conn
            .query_row(
                "SELECT * FROM members WHERE room_id=?1 AND identity=?2",
                params![room_id, identity],
                |row| {
                    Ok(LocalMember {
                        member: Self::row_to_member(row)?,
                        identity: row.get("identity")?,
                    })
                },
            )
            .optional()?)
    }

    pub fn set_member_role(
        &self,
        room_id: &str,
        name: &str,
        role: Role,
        expires_at: Option<&str>,
    ) -> Result<bool> {
        let conn = self.lock();
        let n = conn.execute(
            "UPDATE members SET role=?3, expires_at=?4 WHERE room_id=?1 AND name=?2",
            params![room_id, name, role.as_str(), expires_at],
        )?;
        Ok(n == 1)
    }

    pub fn set_member_muted(&self, room_id: &str, name: &str, muted: bool) -> Result<bool> {
        let conn = self.lock();
        let n = conn.execute(
            "UPDATE members SET muted=?3 WHERE room_id=?1 AND name=?2",
            params![room_id, name, muted as i32],
        )?;
        Ok(n == 1)
    }

    pub fn set_member_revoked(&self, room_id: &str, name: &str) -> Result<bool> {
        let conn = self.lock();
        let n = conn.execute(
            "UPDATE members SET revoked=1, node=NULL WHERE room_id=?1 AND name=?2",
            params![room_id, name],
        )?;
        Ok(n == 1)
    }

    /// Forget a member's binding to a machine (so it can join again from
    /// another one after a reset by the owner).
    pub fn clear_member_node(&self, room_id: &str, name: &str) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "UPDATE members SET node=NULL WHERE room_id=?1 AND name=?2",
            params![room_id, name],
        )?;
        Ok(())
    }

    pub fn rename_room(&self, room_id: &str, new_name: &str) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "UPDATE rooms SET name=?2 WHERE id=?1",
            params![room_id, new_name],
        )?;
        Ok(())
    }

    /// Record that a member was seen from a node just now.
    pub fn touch_member(
        &self,
        room_id: &str,
        name: &str,
        node: Option<&str>,
        now: &str,
    ) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "UPDATE members SET last_seen=?3, node=COALESCE(?4, node) WHERE room_id=?1 AND name=?2",
            params![room_id, name, now, node],
        )?;
        Ok(())
    }

    // ---- messages ------------------------------------------------------

    /// The last sequence number and chain hash of a room. `(0, GENESIS)`
    /// for an empty room.
    pub fn chain_head(&self, room_id: &str) -> Result<(u64, String)> {
        let conn = self.lock();
        Self::chain_head_in(&conn, room_id)
    }

    fn chain_head_in(conn: &Connection, room_id: &str) -> Result<(u64, String)> {
        let head: Option<(i64, String)> = conn
            .query_row(
                "SELECT seq, chain_hash FROM messages WHERE room_id=?1 ORDER BY seq DESC LIMIT 1",
                params![room_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        Ok(match head {
            Some((seq, hash)) => (seq as u64, hash),
            None => (0, GENESIS_PREV.to_string()),
        })
    }

    /// Store a message that already has `seq` and `prev`. Checks it is the
    /// next link in the chain. Returns `Ok(false)` if the id was already
    /// stored (dedup), `Ok(true)` if it was written now.
    pub fn append(&self, msg: &Message) -> Result<bool> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        let written = self.append_in(&tx, msg)?;
        tx.commit()?;
        Ok(written)
    }

    fn append_in(&self, conn: &Connection, msg: &Message) -> Result<bool> {
        if !msg.is_sequenced() {
            return Err(Error::Invalid("message has no seq/prev yet".into()));
        }
        let exists: Option<i64> = conn
            .query_row(
                "SELECT seq FROM messages WHERE id=?1",
                params![msg.id],
                |r| r.get(0),
            )
            .optional()?;
        if exists.is_some() {
            // Already in the chain, so delivered: a queued copy of this
            // very message is done too. A retried entry that the home had
            // in fact stored ends here.
            Self::unqueue_in(conn, msg)?;
            return Ok(false);
        }
        let (last_seq, last_hash) = Self::chain_head_in(conn, &msg.room)?;
        if msg.seq != last_seq + 1 || msg.prev != last_hash {
            return Err(Error::Invalid(format!(
                "chain break in room {}: got seq {} prev {}, expected seq {} prev {}",
                msg.room,
                msg.seq,
                &msg.prev[..msg.prev.len().min(16)],
                last_seq + 1,
                &last_hash[..16]
            )));
        }
        conn.execute(
            "INSERT INTO messages (room_id, seq, id, from_name, type, ts, prev, chain_hash, envelope, reply_to, received)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                msg.room,
                msg.seq as i64,
                msg.id,
                msg.from,
                msg.kind.as_str(),
                msg.ts,
                msg.prev,
                msg.chain_hash(),
                serde_json::to_string(&msg.envelope())?,
                msg.reply_to,
                crate::message::now_ts(),
            ],
        )?;
        if msg.tombstone {
            conn.execute(
                "INSERT INTO contents (msg_id, body, deleted) VALUES (?1, '', 1)",
                params![msg.id],
            )?;
        } else {
            conn.execute(
                "INSERT INTO contents (msg_id, body, deleted) VALUES (?1, ?2, 0)",
                params![msg.id, self.seal(&serde_json::to_string(&msg.content())?)?],
            )?;
        }
        // In the chain means delivered: it leaves the outbox in the same
        // transaction, whichever way it came back (answer, push or sync).
        Self::unqueue_in(conn, msg)?;
        Ok(true)
    }

    /// Take `msg` out of the outbox: the entry with its id and signature.
    /// A different message that happens to carry the same id stays.
    fn unqueue_in(conn: &Connection, msg: &Message) -> Result<()> {
        conn.execute(
            "DELETE FROM outbox WHERE msg_id=?1 AND (sig IS NULL OR sig=?2)",
            params![msg.id, msg.sig],
        )?;
        Ok(())
    }

    /// Give a message the next place in the chain and store it. This is
    /// what the room's home helper does. Atomic: no two messages get the
    /// same seq.
    ///
    /// Returns false, and leaves `msg` as it was, when a message with this
    /// id is already stored: a resubmit, not a new message.
    pub fn sequence_and_append(&self, msg: &mut Message) -> Result<bool> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        let exists: Option<i64> = tx
            .query_row(
                "SELECT seq FROM messages WHERE id=?1",
                params![msg.id],
                |r| r.get(0),
            )
            .optional()?;
        if exists.is_some() {
            return Ok(false);
        }
        let (last_seq, last_hash) = Self::chain_head_in(&tx, &msg.room)?;
        msg.sequence(last_seq + 1, &last_hash);
        self.append_in(&tx, msg)?;
        tx.commit()?;
        Ok(true)
    }

    fn row_to_message(&self, row: &Row<'_>) -> rusqlite::Result<Message> {
        let envelope: String = row.get("envelope")?;
        let body: Option<String> = row.get("body")?;
        let deleted: Option<i32> = row.get("deleted")?;
        let mut v: serde_json::Value =
            serde_json::from_str(&envelope).map_err(|_| rusqlite::Error::InvalidQuery)?;
        let (content, tombstone) = match (body, deleted) {
            (Some(b), Some(0)) => match self.unseal(&b) {
                Some(plain) => (serde_json::from_str(&plain).unwrap_or_default(), false),
                None => (Content::default(), true),
            },
            _ => (Content::default(), true),
        };
        if let Some(obj) = v.as_object_mut() {
            if tombstone {
                obj.insert("tombstone".into(), serde_json::Value::Bool(true));
            } else {
                obj.remove("content_hash");
            }
            obj.insert("text".into(), serde_json::Value::String(content.text));
            obj.insert(
                "action".into(),
                serde_json::to_value(content.action).unwrap_or(serde_json::Value::Null),
            );
            obj.insert("data".into(), content.data);
        }
        serde_json::from_value(v).map_err(|_| rusqlite::Error::InvalidQuery)
    }

    /// Messages with `seq > after`, in order, at most `limit`.
    pub fn messages_after(&self, room_id: &str, after: u64, limit: u32) -> Result<Vec<Message>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT m.envelope, c.body, c.deleted FROM messages m
             LEFT JOIN contents c ON c.msg_id = m.id
             WHERE m.room_id=?1 AND m.seq>?2 ORDER BY m.seq LIMIT ?3",
        )?;
        let rows = stmt.query_map(params![room_id, after as i64, limit], |r| {
            self.row_to_message(r)
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Messages with `ts >= since` (RFC 3339), in order.
    pub fn messages_from_ts(&self, room_id: &str, since: &str, limit: u32) -> Result<Vec<Message>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT m.envelope, c.body, c.deleted FROM messages m
             LEFT JOIN contents c ON c.msg_id = m.id
             WHERE m.room_id=?1 AND m.ts>=?2 ORDER BY m.seq LIMIT ?3",
        )?;
        let rows = stmt.query_map(params![room_id, since, limit], |r| self.row_to_message(r))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn message_by_id(&self, id: &str) -> Result<Option<Message>> {
        let conn = self.lock();
        Ok(conn
            .query_row(
                "SELECT m.envelope, c.body, c.deleted FROM messages m
                 LEFT JOIN contents c ON c.msg_id = m.id WHERE m.id=?1",
                params![id],
                |r| self.row_to_message(r),
            )
            .optional()?)
    }

    /// Every message that answers `msg_id`, in order.
    pub fn replies_to(&self, room_id: &str, msg_id: &str) -> Result<Vec<Message>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT m.envelope, c.body, c.deleted FROM messages m
             LEFT JOIN contents c ON c.msg_id = m.id
             WHERE m.room_id=?1 AND m.reply_to=?2 ORDER BY m.seq",
        )?;
        let rows = stmt.query_map(params![room_id, msg_id], |r| self.row_to_message(r))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Who holds a task right now: the last claim not followed by a
    /// release from the same member.
    pub fn claim_holder(&self, room_id: &str, task_id: &str) -> Result<Option<String>> {
        let mut holder: Option<String> = None;
        for m in self.replies_to(room_id, task_id)? {
            match m.kind {
                MessageType::Claim if holder.is_none() => holder = Some(m.from.clone()),
                MessageType::Release if holder.as_deref() == Some(m.from.as_str()) => holder = None,
                _ => {}
            }
        }
        Ok(holder)
    }

    /// Approves in a room for exactly this action hash, in order.
    pub fn approvals_for(&self, room_id: &str, action_hash: &str) -> Result<Vec<Message>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT m.envelope, c.body, c.deleted FROM messages m
             LEFT JOIN contents c ON c.msg_id = m.id
             WHERE m.room_id=?1 AND m.type='approve'
               AND json_extract(m.envelope, '$.action_hash')=?2
               AND m.id NOT IN (SELECT msg_id FROM approvals_used)
             ORDER BY m.seq",
        )?;
        let rows = stmt.query_map(params![room_id, action_hash], |r| self.row_to_message(r))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Spend an approve. Returns false if it was already spent.
    pub fn approval_use(&self, msg_id: &str, action_hash: &str, now: &str) -> Result<bool> {
        let conn = self.lock();
        let n = conn.execute(
            "INSERT OR IGNORE INTO approvals_used (msg_id, action_hash, used_at) VALUES (?1, ?2, ?3)",
            params![msg_id, action_hash, now],
        )?;
        Ok(n == 1)
    }

    /// Retention: drop the content of messages older than `cutoff`
    /// (RFC 3339). Envelopes stay; the chain is unchanged. Returns how
    /// many were tombstoned.
    pub fn tombstone_before(&self, room_id: &str, cutoff: &str) -> Result<u64> {
        let conn = self.lock();
        let n = conn.execute(
            "UPDATE contents SET body='', deleted=1
             WHERE deleted=0 AND msg_id IN (SELECT id FROM messages WHERE room_id=?1 AND ts<?2)",
            params![room_id, cutoff],
        )?;
        Ok(n as u64)
    }

    /// Delete one message's content (a GDPR delete). The envelope stays.
    pub fn tombstone_message(&self, msg_id: &str) -> Result<bool> {
        let conn = self.lock();
        let n = conn.execute(
            "UPDATE contents SET body='', deleted=1 WHERE msg_id=?1 AND deleted=0",
            params![msg_id],
        )?;
        Ok(n == 1)
    }

    pub fn message_count(&self, room_id: &str) -> Result<u64> {
        let conn = self.lock();
        let n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM messages WHERE room_id=?1",
            params![room_id],
            |r| r.get(0),
        )?;
        Ok(n as u64)
    }

    // ---- bookmarks -----------------------------------------------------

    pub fn bookmark(&self, room_id: &str, reader: &str) -> Result<u64> {
        let conn = self.lock();
        let seq: Option<i64> = conn
            .query_row(
                "SELECT seq FROM bookmarks WHERE room_id=?1 AND reader=?2",
                params![room_id, reader],
                |r| r.get(0),
            )
            .optional()?;
        Ok(seq.unwrap_or(0) as u64)
    }

    /// Move a bookmark forward. Never moves it back.
    pub fn set_bookmark(&self, room_id: &str, reader: &str, seq: u64) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "INSERT INTO bookmarks (room_id, reader, seq) VALUES (?1, ?2, ?3)
             ON CONFLICT(room_id, reader) DO UPDATE SET seq=MAX(bookmarks.seq, excluded.seq)",
            params![room_id, reader, seq as i64],
        )?;
        Ok(())
    }

    // ---- deliveries ----------------------------------------------------
    //
    // available -> leased          `next`: a new token, attempt + 1
    // leased -> acked              ack with the current token
    // leased -> leased             renew with the current token
    // leased -> delayed            nack with the current token
    // leased -> available          the lease runs out
    // leased|delayed -> quarantined  at the attempt limit
    // delayed -> available         retry_at passes
    // quarantined -> replay        `deliveries replay`
    //
    // A token is replaced each time a message is handed out, so a worker
    // whose lease ran out cannot settle the newer delivery.

    fn row_to_delivery(row: &Row<'_>) -> rusqlite::Result<Delivery> {
        let state: String = row.get("state")?;
        Ok(Delivery {
            room_id: row.get("room_id")?,
            reader: row.get("reader")?,
            seq: row.get::<_, i64>("seq")? as u64,
            state: DeliveryState::parse(&state),
            token: row.get("token")?,
            lease_until: row.get("lease_until")?,
            retry_at: row.get("retry_at")?,
            attempt: row.get::<_, i64>("attempt")? as u32,
            updated: row.get("updated")?,
        })
    }

    fn bookmark_in(conn: &Connection, room_id: &str, reader: &str) -> Result<u64> {
        let seq: Option<i64> = conn
            .query_row(
                "SELECT seq FROM bookmarks WHERE room_id=?1 AND reader=?2",
                params![room_id, reader],
                |r| r.get(0),
            )
            .optional()?;
        Ok(seq.unwrap_or(0) as u64)
    }

    /// Move the bookmark over every settled message after it: ones the
    /// reader does not want, acked ones and quarantined ones. It stops at
    /// the first message still owed. Returns the new bookmark.
    fn advance_bookmark_in(conn: &Connection, room_id: &str, reader: &str) -> Result<u64> {
        let start = Self::bookmark_in(conn, room_id, reader)?;
        let mut bm = start;
        'pages: loop {
            let mut stmt = conn.prepare_cached(
                "SELECT m.seq, m.from_name, m.type, d.state FROM messages m
                 LEFT JOIN deliveries d ON d.room_id=m.room_id AND d.reader=?2 AND d.seq=m.seq
                 WHERE m.room_id=?1 AND m.seq>?3 ORDER BY m.seq LIMIT 500",
            )?;
            let rows: Vec<(i64, String, String, Option<String>)> = stmt
                .query_map(params![room_id, reader, bm as i64], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
                })?
                .collect::<rusqlite::Result<_>>()?;
            let n = rows.len();
            for (seq, from, kind, state) in rows {
                let settled = !wanted(&from, &kind, reader)
                    || matches!(state.as_deref(), Some("acked") | Some("quarantined"));
                if !settled {
                    break 'pages;
                }
                bm = seq as u64;
            }
            if n < 500 {
                break;
            }
        }
        if bm > start {
            conn.execute(
                "INSERT INTO bookmarks (room_id, reader, seq) VALUES (?1, ?2, ?3)
                 ON CONFLICT(room_id, reader) DO UPDATE SET seq=MAX(bookmarks.seq, excluded.seq)",
                params![room_id, reader, bm as i64],
            )?;
        }
        Ok(bm)
    }

    /// Hand the next message owed to `reader` out, leased until
    /// `lease_until`. The lowest seq first. `None` when nothing is owed.
    pub fn delivery_lease(
        &self,
        room_id: &str,
        reader: &str,
        now: &str,
        lease_until: &str,
        max_attempts: u32,
    ) -> Result<Option<(Message, Delivery)>> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        // Leases that ran out at the attempt limit go to quarantine.
        tx.execute(
            "UPDATE deliveries SET state='quarantined', token=NULL, updated=?3
             WHERE room_id=?1 AND reader=?2 AND state='leased' AND lease_until<=?3 AND attempt>=?4",
            params![room_id, reader, now, max_attempts],
        )?;
        let bm = Self::advance_bookmark_in(&tx, room_id, reader)?;
        // Anything at or below the bookmark first: replays, and replayed
        // messages whose lease ran out or whose delay passed.
        let mut seq: Option<i64> = tx
            .query_row(
                "SELECT seq FROM deliveries WHERE room_id=?1 AND reader=?2 AND seq<=?3 AND (
                   state='replay'
                   OR (state='leased' AND lease_until<=?4)
                   OR (state='delayed' AND retry_at<=?4))
                 ORDER BY seq LIMIT 1",
                params![room_id, reader, bm as i64, now],
                |r| r.get(0),
            )
            .optional()?;
        let mut after = bm as i64;
        while seq.is_none() {
            let rows: Vec<Candidate> = {
                let mut stmt = tx.prepare_cached(
                    "SELECT m.seq, m.from_name, m.type, d.state, d.lease_until, d.retry_at
                     FROM messages m
                     LEFT JOIN deliveries d ON d.room_id=m.room_id AND d.reader=?2 AND d.seq=m.seq
                     WHERE m.room_id=?1 AND m.seq>?3 ORDER BY m.seq LIMIT 500",
                )?;
                let rows = stmt.query_map(params![room_id, reader, after], |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                    ))
                })?;
                rows.collect::<rusqlite::Result<_>>()?
            };
            if rows.is_empty() {
                break;
            }
            for (s, from, kind, state, lease_until, retry_at) in &rows {
                after = *s;
                if !wanted(from, kind, reader) {
                    continue;
                }
                let free = match state.as_deref() {
                    None | Some("replay") => true,
                    Some("leased") => lease_until.as_deref().is_some_and(|t| t <= now),
                    Some("delayed") => retry_at.as_deref().is_some_and(|t| t <= now),
                    _ => false,
                };
                if free {
                    seq = Some(*s);
                    break;
                }
            }
        }
        let Some(seq) = seq else {
            tx.commit()?;
            return Ok(None);
        };
        let token = format!(
            "d_{}",
            data_encoding::HEXLOWER.encode(&rand::random::<[u8; 16]>())
        );
        tx.execute(
            "INSERT INTO deliveries (room_id, reader, seq, state, token, lease_until, attempt, updated)
             VALUES (?1, ?2, ?3, 'leased', ?4, ?5, 1, ?6)
             ON CONFLICT(room_id, reader, seq) DO UPDATE SET
               state='leased', token=excluded.token, lease_until=excluded.lease_until,
               retry_at=NULL, attempt=deliveries.attempt+1, updated=excluded.updated",
            params![room_id, reader, seq, token, lease_until, now],
        )?;
        self.faults.check("delivery.lease")?;
        let msg = tx.query_row(
            "SELECT m.envelope, c.body, c.deleted FROM messages m
             LEFT JOIN contents c ON c.msg_id = m.id WHERE m.room_id=?1 AND m.seq=?2",
            params![room_id, seq],
            |r| self.row_to_message(r),
        )?;
        let d = tx.query_row(
            "SELECT * FROM deliveries WHERE room_id=?1 AND reader=?2 AND seq=?3",
            params![room_id, reader, seq],
            Self::row_to_delivery,
        )?;
        tx.commit()?;
        Ok(Some((msg, d)))
    }

    /// The delivery a token names, if it still names one.
    pub fn delivery_by_token(&self, token: &str) -> Result<Option<Delivery>> {
        let conn = self.lock();
        Ok(conn
            .query_row(
                "SELECT * FROM deliveries WHERE token=?1",
                params![token],
                Self::row_to_delivery,
            )
            .optional()?)
    }

    /// Settle a delivery by its token. Refused if the token no longer names
    /// a message this reader holds: it was settled, or its lease ran out
    /// and it was handed out again under a new token. Acking twice is fine.
    ///
    /// An ack after the lease ran out still counts if nobody was handed the
    /// message since: the token is still the current one.
    pub fn delivery_settle(
        &self,
        token: &str,
        how: &Settle,
        now: &str,
        max_attempts: u32,
    ) -> Result<Delivery> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        let d = tx
            .query_row(
                "SELECT * FROM deliveries WHERE token=?1",
                params![token],
                Self::row_to_delivery,
            )
            .optional()?
            .ok_or_else(|| {
                Error::Denied(
                    "that delivery is no longer yours: it was settled, or its lease ran out and \
                     it was handed out again under a new token"
                        .into(),
                )
            })?;
        let key = params![d.room_id, d.reader, d.seq as i64, now];
        match (d.state, how) {
            (DeliveryState::Acked, Settle::Ack) => {}
            (DeliveryState::Leased, Settle::Ack) => {
                tx.execute(
                    "UPDATE deliveries SET state='acked', lease_until=NULL, updated=?4
                     WHERE room_id=?1 AND reader=?2 AND seq=?3",
                    key,
                )?;
            }
            (DeliveryState::Leased, Settle::Renew { lease_until }) => {
                tx.execute(
                    "UPDATE deliveries SET lease_until=?5, updated=?4
                     WHERE room_id=?1 AND reader=?2 AND seq=?3",
                    params![d.room_id, d.reader, d.seq as i64, now, lease_until],
                )?;
            }
            (DeliveryState::Leased, Settle::Nack { retry_at }) => {
                let quarantine = d.attempt >= max_attempts;
                tx.execute(
                    "UPDATE deliveries SET state=?5, token=NULL, lease_until=NULL, retry_at=?6, updated=?4
                     WHERE room_id=?1 AND reader=?2 AND seq=?3",
                    params![
                        d.room_id,
                        d.reader,
                        d.seq as i64,
                        now,
                        if quarantine { "quarantined" } else { "delayed" },
                        if quarantine { None } else { Some(retry_at) }
                    ],
                )?;
            }
            (state, _) => {
                return Err(Error::Denied(format!(
                    "that delivery is {} and can only be acked again",
                    state.as_str()
                )))
            }
        }
        Self::advance_bookmark_in(&tx, &d.room_id, &d.reader)?;
        // Acked rows the bookmark has passed are kept a day, so a repeated
        // ack still finds them, then pruned.
        let day_ago = chrono::DateTime::parse_from_rfc3339(now)
            .map(|t| {
                (t - chrono::Duration::days(1)).to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
            })
            .unwrap_or_default();
        tx.execute(
            "DELETE FROM deliveries WHERE room_id=?1 AND reader=?2 AND state='acked'
               AND updated<?3 AND seq<=(SELECT seq FROM bookmarks WHERE room_id=?1 AND reader=?2)",
            params![d.room_id, d.reader, day_ago],
        )?;
        let out = tx.query_row(
            "SELECT * FROM deliveries WHERE room_id=?1 AND reader=?2 AND seq=?3",
            params![d.room_id, d.reader, d.seq as i64],
            Self::row_to_delivery,
        )?;
        tx.commit()?;
        Ok(out)
    }

    /// Settle these messages as taken on, whatever their lease: the reader
    /// read them and said so (`read --ack`). Quarantined ones stay as a
    /// record. Returns the new bookmark.
    pub fn delivery_ack_seqs(
        &self,
        room_id: &str,
        reader: &str,
        seqs: &[u64],
        now: &str,
    ) -> Result<u64> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        for seq in seqs {
            tx.execute(
                "INSERT INTO deliveries (room_id, reader, seq, state, attempt, updated)
                 VALUES (?1, ?2, ?3, 'acked', 1, ?4)
                 ON CONFLICT(room_id, reader, seq) DO UPDATE SET
                   state='acked', token=NULL, lease_until=NULL, retry_at=NULL, updated=?4
                 WHERE deliveries.state != 'quarantined'",
                params![room_id, reader, *seq as i64, now],
            )?;
        }
        let bm = Self::advance_bookmark_in(&tx, room_id, reader)?;
        tx.commit()?;
        Ok(bm)
    }

    /// Bring a quarantined message back: handed out again next, as if new.
    pub fn delivery_replay(
        &self,
        room_id: &str,
        reader: &str,
        seq: u64,
        now: &str,
    ) -> Result<bool> {
        let conn = self.lock();
        let n = conn.execute(
            "UPDATE deliveries SET state='replay', token=NULL, lease_until=NULL, retry_at=NULL,
               attempt=0, updated=?4
             WHERE room_id=?1 AND reader=?2 AND seq=?3 AND state='quarantined'",
            params![room_id, reader, seq as i64, now],
        )?;
        Ok(n == 1)
    }

    /// What is owed to `reader`, without handing anything out: the
    /// messages after the bookmark not yet settled, leased ones included,
    /// delayed ones only once due. For a wake-up hook, which shows and
    /// never takes.
    pub fn delivery_peek(
        &self,
        room_id: &str,
        reader: &str,
        now: &str,
        limit: u32,
    ) -> Result<Vec<Message>> {
        let bm = {
            let mut conn = self.lock();
            let tx = conn.transaction()?;
            let bm = Self::advance_bookmark_in(&tx, room_id, reader)?;
            tx.commit()?;
            bm
        };
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT m.envelope, c.body, c.deleted FROM messages m
             LEFT JOIN contents c ON c.msg_id = m.id
             LEFT JOIN deliveries d ON d.room_id=m.room_id AND d.reader=?2 AND d.seq=m.seq
             WHERE m.room_id=?1 AND m.seq>?3
               AND m.from_name != ?2 AND m.type NOT IN ('system', 'control')
               AND (d.state IS NULL OR d.state IN ('leased', 'replay')
                    OR (d.state='delayed' AND d.retry_at<=?4))
             ORDER BY m.seq LIMIT ?5",
        )?;
        let rows = stmt.query_map(params![room_id, reader, bm as i64, now, limit], |r| {
            self.row_to_message(r)
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Deliveries that are not simply done: leased, delayed, quarantined or
    /// replayed. For one reader, or every reader of a room.
    pub fn deliveries(&self, room_id: &str, reader: Option<&str>) -> Result<Vec<Delivery>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT * FROM deliveries WHERE room_id=?1 AND (?2 IS NULL OR reader=?2)
               AND state != 'acked' ORDER BY reader, seq",
        )?;
        let rows = stmt.query_map(params![room_id, reader], Self::row_to_delivery)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// The soonest a lease runs out or a delay passes for `reader`, after
    /// `now`: when a waiting `next` should look again.
    pub fn delivery_next_due(
        &self,
        room_id: &str,
        reader: &str,
        now: &str,
    ) -> Result<Option<String>> {
        let conn = self.lock();
        Ok(conn.query_row(
            "SELECT MIN(t) FROM (
               SELECT lease_until AS t FROM deliveries
                 WHERE room_id=?1 AND reader=?2 AND state='leased' AND lease_until>?3
               UNION ALL
               SELECT retry_at AS t FROM deliveries
                 WHERE room_id=?1 AND reader=?2 AND state='delayed' AND retry_at>?3)",
            params![room_id, reader, now],
            |r| r.get(0),
        )?)
    }

    // ---- outbox --------------------------------------------------------
    //
    // pending -> (sequenced: row deleted with the append)
    // pending|waiting -> waiting      temporary failure, retry_at set
    // pending|waiting -> failed       the home said no for good
    // pending|waiting -> quarantined  the same unknown answer, for long
    // failed|quarantined -> pending   `outbox retry`
    // failed|quarantined -> dropped   `outbox drop`: payload cleared
    //
    // Nothing else removes a row.

    /// Park a signed, unsequenced message until the room's home is
    /// reachable. On disk, sealed, before `send` returns.
    pub fn outbox_add(&self, msg: &Message) -> Result<()> {
        let conn = self.lock();
        let sealed = self.seal(&serde_json::to_string(msg)?)?;
        self.faults.check("outbox.add")?;
        conn.execute(
            "INSERT OR IGNORE INTO outbox (msg_id, room_id, message, created, sender, state, updated, sig)
             VALUES (?1, ?2, ?3, ?4, ?5, 'pending', ?6, ?7)",
            params![
                msg.id,
                msg.room,
                sealed,
                msg.ts,
                msg.from,
                crate::message::now_ts(),
                msg.sig
            ],
        )?;
        Ok(())
    }

    fn row_to_outbox(&self, row: &Row<'_>) -> rusqlite::Result<OutboxEntry> {
        let state: String = row.get("state")?;
        let stored: String = row.get("message")?;
        let message = if stored.is_empty() {
            None
        } else {
            self.unseal(&stored)
                .and_then(|plain| serde_json::from_str::<Message>(&plain).ok())
        };
        Ok(OutboxEntry {
            msg_id: row.get("msg_id")?,
            room_id: row.get("room_id")?,
            sender: row.get("sender")?,
            state: state.parse().unwrap_or(OutboxState::Pending),
            created: row.get("created")?,
            attempts: row.get::<_, i64>("attempts")? as u32,
            retry_at: row.get("retry_at")?,
            reason: row.get("last_error")?,
            reason_class: row.get("reason_class")?,
            reason_code: row.get("reason_code")?,
            updated: row.get("updated")?,
            unreadable: !stored.is_empty() && message.is_none(),
            message,
        })
    }

    /// One entry by message id.
    pub fn outbox_get(&self, msg_id: &str) -> Result<Option<OutboxEntry>> {
        let conn = self.lock();
        Ok(conn
            .query_row(
                "SELECT * FROM outbox WHERE msg_id=?1",
                params![msg_id],
                |r| self.row_to_outbox(r),
            )
            .optional()?)
    }

    /// The messages still to send in a room, as lanes: one per sender, each
    /// in the order it was queued. Only `pending` and `waiting` entries.
    /// Queued order is insertion order (`rowid`): timestamps have whole
    /// seconds and ids are not monotonic within one.
    pub fn outbox_lanes(&self, room_id: &str) -> Result<Vec<(String, Vec<OutboxEntry>)>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT * FROM outbox WHERE room_id=?1 AND state IN ('pending', 'waiting')
             ORDER BY sender, rowid",
        )?;
        let rows = stmt.query_map(params![room_id], |r| self.row_to_outbox(r))?;
        let mut lanes: Vec<(String, Vec<OutboxEntry>)> = Vec::new();
        for e in rows {
            let e = e?;
            match lanes.last_mut() {
                Some((sender, lane)) if *sender == e.sender => lane.push(e),
                _ => lanes.push((e.sender.clone(), vec![e])),
            }
        }
        Ok(lanes)
    }

    /// Every entry, in one room or all, in the order queued. Dropped
    /// entries are listed too, without their content.
    pub fn outbox_entries(&self, room_id: Option<&str>) -> Result<Vec<OutboxEntry>> {
        let conn = self.lock();
        let mut stmt =
            conn.prepare("SELECT * FROM outbox WHERE (?1 IS NULL OR room_id=?1) ORDER BY rowid")?;
        let rows = stmt.query_map(params![room_id], |r| self.row_to_outbox(r))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    fn outbox_set(&self, sql: &str, args: &[&dyn rusqlite::ToSql]) -> Result<bool> {
        let conn = self.lock();
        self.faults.check("outbox.state")?;
        Ok(conn.execute(sql, args)? == 1)
    }

    /// A temporary failure: try again at `retry_at`. `class` says why
    /// (transport, paused, budget, home) so the right event can wake it
    /// early.
    pub fn outbox_wait(
        &self,
        msg_id: &str,
        class: &str,
        reason: &str,
        code: Option<i32>,
        retry_at: &str,
        now: &str,
    ) -> Result<bool> {
        self.outbox_set(
            "UPDATE outbox SET state='waiting', attempts=attempts+1, retry_at=?3,
               reason_class=?4, last_error=?5, reason_code=?6, updated=?2
             WHERE msg_id=?1 AND state IN ('pending', 'waiting')",
            &[&msg_id, &now, &retry_at, &class, &reason, &code],
        )
    }

    /// The home said no for good. Kept, out of its lane, for a person to
    /// retry or drop.
    pub fn outbox_fail(
        &self,
        msg_id: &str,
        reason: &str,
        code: Option<i32>,
        now: &str,
    ) -> Result<bool> {
        self.outbox_set(
            "UPDATE outbox SET state='failed', attempts=attempts+1, retry_at=NULL,
               reason_class='refused', last_error=?3, reason_code=?4, updated=?2
             WHERE msg_id=?1 AND state IN ('pending', 'waiting')",
            &[&msg_id, &now, &reason, &code],
        )
    }

    /// An answer this helper does not understand. Kept and retried at
    /// `retry_at`; quarantined once the same answer (`code`) has come back
    /// at least `min_count` times over at least `min_secs` seconds. Both
    /// must hold, so neither a burst nor a slow trickle decides alone.
    #[allow(clippy::too_many_arguments)]
    pub fn outbox_unknown(
        &self,
        msg_id: &str,
        reason: &str,
        code: Option<i32>,
        retry_at: &str,
        now: &str,
        min_count: u32,
        min_secs: i64,
    ) -> Result<OutboxState> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        let row: Option<(Option<i32>, Option<String>, i64)> = tx
            .query_row(
                "SELECT unknown_code, unknown_since, unknown_count FROM outbox
                 WHERE msg_id=?1 AND state IN ('pending', 'waiting')",
                params![msg_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        let Some((last_code, since, count)) = row else {
            return Err(Error::Invalid(format!(
                "{msg_id} is not waiting to be sent"
            )));
        };
        let (since, count) = match (last_code == code, since) {
            (true, Some(since)) => (since, count as u32 + 1),
            _ => (now.to_string(), 1),
        };
        let held_for = seconds_between(&since, now);
        let state = if count >= min_count && held_for >= min_secs {
            OutboxState::Quarantined
        } else {
            OutboxState::Waiting
        };
        self.faults.check("outbox.state")?;
        tx.execute(
            "UPDATE outbox SET state=?2, attempts=attempts+1, retry_at=?3,
               reason_class='unknown', last_error=?4, reason_code=?5,
               unknown_code=?5, unknown_since=?6, unknown_count=?7, updated=?8
             WHERE msg_id=?1",
            params![
                msg_id,
                state.as_str(),
                if state == OutboxState::Waiting {
                    Some(retry_at)
                } else {
                    None
                },
                reason,
                code,
                since,
                count,
                now
            ],
        )?;
        tx.commit()?;
        Ok(state)
    }

    /// Put a failed or quarantined message back in its lane, as it was:
    /// same id, same signature. Returns false if it was neither.
    pub fn outbox_retry(&self, msg_id: &str, now: &str) -> Result<bool> {
        self.outbox_set(
            "UPDATE outbox SET state='pending', attempts=0, retry_at=NULL,
               unknown_code=NULL, unknown_since=NULL, unknown_count=0, updated=?2
             WHERE msg_id=?1 AND state IN ('failed', 'quarantined') AND message != ''",
            &[&msg_id, &now],
        )
    }

    /// Give up on a failed or quarantined message, on purpose. The row
    /// stays as a record; its content is wiped.
    pub fn outbox_drop(&self, msg_id: &str, now: &str) -> Result<bool> {
        self.outbox_set(
            "UPDATE outbox SET state='dropped', message='', retry_at=NULL, updated=?2
             WHERE msg_id=?1 AND state IN ('failed', 'quarantined')",
            &[&msg_id, &now],
        )
    }

    /// Make every message waiting for this reason due now: the link came
    /// back, or the room was resumed.
    pub fn outbox_wake(&self, room_id: &str, class: &str) -> Result<u64> {
        let conn = self.lock();
        let n = conn.execute(
            "UPDATE outbox SET retry_at=NULL
             WHERE room_id=?1 AND state='waiting' AND reason_class=?2",
            params![room_id, class],
        )?;
        Ok(n as u64)
    }

    /// Take a message out of the outbox because its sender was told, there
    /// and then, that the home refused it.
    pub fn outbox_remove(&self, msg_id: &str) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "DELETE FROM outbox WHERE msg_id=?1 AND state IN ('pending', 'waiting')",
            params![msg_id],
        )?;
        Ok(())
    }

    /// Messages still to send: pending and waiting.
    pub fn outbox_count(&self, room_id: &str) -> Result<u64> {
        Ok(self.outbox_counts(room_id)?.queued())
    }

    pub fn outbox_counts(&self, room_id: &str) -> Result<OutboxCounts> {
        let conn = self.lock();
        let mut stmt =
            conn.prepare("SELECT state, COUNT(*) FROM outbox WHERE room_id=?1 GROUP BY state")?;
        let rows = stmt.query_map(params![room_id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
        })?;
        let mut c = OutboxCounts::default();
        for row in rows {
            let (state, n) = row?;
            let n = n as u64;
            match state.parse().unwrap_or(OutboxState::Pending) {
                OutboxState::Pending => c.pending += n,
                OutboxState::Waiting => c.waiting += n,
                OutboxState::Failed => c.failed += n,
                OutboxState::Quarantined => c.quarantined += n,
                OutboxState::Dropped => c.dropped += n,
            }
        }
        Ok(c)
    }

    // ---- invites -------------------------------------------------------

    pub fn invite_record(&self, inv: &Invite) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "INSERT OR IGNORE INTO invites (nonce, room_id, name, created, expires) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![inv.nonce, inv.room_id, inv.name, inv.created, inv.expires],
        )?;
        Ok(())
    }

    /// Mark an invite used. Returns `Ok(false)` if it was already used or
    /// was never issued here.
    pub fn invite_use(&self, nonce: &str, used_by: &str, now: &str) -> Result<bool> {
        let conn = self.lock();
        let n = conn.execute(
            "UPDATE invites SET used_at=?2, used_by=?3 WHERE nonce=?1 AND used_at IS NULL",
            params![nonce, now, used_by],
        )?;
        Ok(n == 1)
    }

    // ---- limits --------------------------------------------------------

    fn count_since(
        conn: &Connection,
        room_id: &str,
        from: Option<&str>,
        since: &str,
    ) -> Result<u32> {
        let n: i64 = match from {
            Some(f) => conn.query_row(
                "SELECT COUNT(*) FROM messages WHERE room_id=?1 AND from_name=?2 AND received>=?3",
                params![room_id, f, since],
                |r| r.get(0),
            )?,
            None => conn.query_row(
                "SELECT COUNT(*) FROM messages WHERE room_id=?1 AND received>=?2",
                params![room_id, since],
                |r| r.get(0),
            )?,
        };
        Ok(n as u32)
    }

    /// Refuse a message that would break the per-minute or daily limits.
    /// Returns `Ok(alert)`: `alert` is true when the room just crossed its
    /// burst-alert line and the owner should be told.
    pub fn check_limits(&self, room_id: &str, from: &str, limits: &Limits) -> Result<bool> {
        let conn = self.lock();
        let now = chrono::Utc::now();
        let minute_ago =
            (now - chrono::Duration::minutes(1)).to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let day_start = now
            .date_naive()
            .and_hms_opt(0, 0, 0)
            .expect("midnight")
            .and_utc()
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let per_minute = Self::count_since(&conn, room_id, Some(from), &minute_ago)?;
        if per_minute >= limits.per_minute_per_sender {
            // The window frees up when the oldest message in it turns a
            // minute old.
            let oldest: Option<String> = conn.query_row(
                "SELECT MIN(received) FROM messages WHERE room_id=?1 AND from_name=?2 AND received>=?3",
                params![room_id, from, minute_ago],
                |r| r.get(0),
            )?;
            let wait = oldest
                .map(|o| {
                    60 - seconds_between(
                        &o,
                        &now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                    )
                })
                .unwrap_or(60)
                .clamp(1, 60) as u64;
            return Err(Error::OverBudget(
                format!(
                    "{from} sent {per_minute} messages in the last minute; the limit is {}",
                    limits.per_minute_per_sender
                ),
                Some(wait),
            ));
        }
        let today = Self::count_since(&conn, room_id, None, &day_start)?;
        if today >= limits.daily_per_room {
            let midnight = now
                .date_naive()
                .and_hms_opt(0, 0, 0)
                .expect("midnight")
                .and_utc()
                + chrono::Duration::days(1);
            let wait = (midnight - now).num_seconds().max(1) as u64;
            return Err(Error::OverBudget(
                format!(
                    "room has used its daily budget of {} messages",
                    limits.daily_per_room
                ),
                Some(wait),
            ));
        }
        let alert_at = limits.daily_per_room as u64 * limits.burst_alert_percent as u64 / 100;
        Ok(today as u64 + 1 == alert_at)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::{Identity, Kind};
    use crate::message::{Draft, MessageType};

    fn room(owner: &Identity) -> Room {
        Room {
            id: "r_test".into(),
            name: "ops".into(),
            about: "".into(),
            owner: owner.public(),
            created: "2026-01-01T00:00:00Z".into(),
            retention_days: None,
            class: DataClass::Internal,
            paused: false,
            hold: false,
            closed: false,
            home_node: "node".into(),
            home_hints: serde_json::Value::Null,
        }
    }

    fn msg(owner: &Identity, text: &str) -> Message {
        Message::new(
            Draft {
                room: "r_test".into(),
                from: "haris".into(),
                text: text.into(),
                kind: Some(MessageType::Chat),
                ..Default::default()
            },
            owner,
        )
        .unwrap()
    }

    #[test]
    fn rooms_and_members() {
        let owner = Identity::generate("haris", Kind::Human);
        let s = Store::open_memory().unwrap();
        s.create_room(&room(&owner)).unwrap();
        assert!(matches!(
            s.create_room(&room(&owner)),
            Err(Error::NameTaken(_))
        ));
        assert_eq!(s.room("ops").unwrap().id, "r_test");
        assert!(matches!(s.room("nope"), Err(Error::NotInRoom(_))));
        let m = Member {
            room_id: "r_test".into(),
            name: "haris".into(),
            key: owner.public(),
            kind: Kind::Human,
            role: Role::Approver,
            node: None,
            granted_by: owner.public(),
            expires_at: None,
            joined_at: "2026-01-01T00:00:00Z".into(),
            last_seen: None,
            muted: false,
            revoked: false,
            profile: serde_json::Value::Null,
        };
        s.upsert_member(&m, Some("default")).unwrap();
        assert_eq!(s.members("r_test").unwrap().len(), 1);
        assert_eq!(s.local_members("r_test").unwrap()[0].identity, "default");
        assert_eq!(
            s.member_by_key("r_test", &owner.public())
                .unwrap()
                .unwrap()
                .name,
            "haris"
        );
    }

    #[test]
    fn chain_append_dedup_and_bookmarks() {
        let owner = Identity::generate("haris", Kind::Human);
        let s = Store::open_memory().unwrap();
        s.create_room(&room(&owner)).unwrap();
        assert_eq!(
            s.chain_head("r_test").unwrap(),
            (0, GENESIS_PREV.to_string())
        );
        let mut a = msg(&owner, "one");
        s.sequence_and_append(&mut a).unwrap();
        assert_eq!(a.seq, 1);
        assert_eq!(a.prev, GENESIS_PREV);
        let mut b = msg(&owner, "two");
        s.sequence_and_append(&mut b).unwrap();
        assert_eq!(b.seq, 2);
        assert_eq!(b.prev, a.chain_hash());
        // Dedup.
        assert!(!s.append(&b).unwrap());
        // Chain break.
        let mut c = msg(&owner, "three");
        c.sequence(3, "sha256:bad");
        assert!(s.append(&c).is_err());
        c.sequence(3, &b.chain_hash());
        assert!(s.append(&c).unwrap());
        // Read back exactly.
        let all = s.messages_after("r_test", 0, 100).unwrap();
        assert_eq!(all, vec![a.clone(), b.clone(), c.clone()]);
        assert!(all[0].verify(&owner.public()).is_ok());
        // Bookmarks are per reader and never move back.
        assert_eq!(s.bookmark("r_test", "haris").unwrap(), 0);
        s.set_bookmark("r_test", "haris", 2).unwrap();
        s.set_bookmark("r_test", "haris", 1).unwrap();
        assert_eq!(s.bookmark("r_test", "haris").unwrap(), 2);
        assert_eq!(s.bookmark("r_test", "bob").unwrap(), 0);
        assert_eq!(s.messages_after("r_test", 2, 100).unwrap(), vec![c]);
        // Reading never deletes.
        assert_eq!(s.message_count("r_test").unwrap(), 3);
    }

    #[test]
    fn outbox_and_invites() {
        let owner = Identity::generate("haris", Kind::Human);
        let s = Store::open_memory().unwrap();
        s.create_room(&room(&owner)).unwrap();
        let m = msg(&owner, "queued");
        s.outbox_add(&m).unwrap();
        s.outbox_add(&m).unwrap();
        assert_eq!(s.outbox_count("r_test").unwrap(), 1);
        let lanes = s.outbox_lanes("r_test").unwrap();
        assert_eq!(lanes.len(), 1);
        assert_eq!(lanes[0].0, "haris");
        assert_eq!(lanes[0].1[0].message.as_ref(), Some(&m));
        s.outbox_remove(&m.id).unwrap();
        assert_eq!(s.outbox_count("r_test").unwrap(), 0);

        let inv = Invite::create(
            crate::invite::InviteSpec {
                room_id: "r_test".into(),
                room_name: "ops".into(),
                name: "bob".into(),
                kind: Kind::Agent,
                role: Role::TaskGiver,
                home_node: "node".into(),
                home_hints: serde_json::Value::Null,
                for_node: None,
                ttl_hours: None,
            },
            &owner,
        )
        .unwrap();
        s.invite_record(&inv).unwrap();
        assert!(s.invite_use(&inv.nonce, "key", "now").unwrap());
        assert!(!s.invite_use(&inv.nonce, "key2", "now").unwrap());
        assert!(!s.invite_use("i_unknown", "key", "now").unwrap());
    }

    #[test]
    fn limits() {
        let owner = Identity::generate("haris", Kind::Human);
        let s = Store::open_memory().unwrap();
        s.create_room(&room(&owner)).unwrap();
        let limits = Limits {
            per_minute_per_sender: 2,
            daily_per_room: 3,
            burst_alert_percent: 100,
        };
        s.check_limits("r_test", "haris", &limits).unwrap();
        let mut a = msg(&owner, "1");
        s.sequence_and_append(&mut a).unwrap();
        let mut b = msg(&owner, "2");
        s.sequence_and_append(&mut b).unwrap();
        assert!(matches!(
            s.check_limits("r_test", "haris", &limits),
            Err(Error::OverBudget(..))
        ));
        // Another sender is under the per-minute limit but the daily
        // budget is about to be hit: the alert fires on the third message.
        assert!(s.check_limits("r_test", "bob", &limits).unwrap());
    }

    #[test]
    fn appending_the_same_message_twice_stores_it_once() {
        let owner = Identity::generate("haris", Kind::Human);
        let s = Store::open_memory().unwrap();
        s.create_room(&room(&owner)).unwrap();
        let mut a = msg(&owner, "1");
        let mut again = a.clone();
        assert!(s.sequence_and_append(&mut a).unwrap());
        // The copy is refused and left untouched: no made-up seq or prev.
        assert!(!s.sequence_and_append(&mut again).unwrap());
        assert_eq!(again.seq, 0);
        assert_eq!(s.message_count("r_test").unwrap(), 1);
    }

    #[test]
    fn a_backdated_ts_still_counts_against_the_limits() {
        let owner = Identity::generate("haris", Kind::Human);
        let s = Store::open_memory().unwrap();
        s.create_room(&room(&owner)).unwrap();
        let limits = Limits {
            per_minute_per_sender: 2,
            daily_per_room: 100,
            burst_alert_percent: 100,
        };
        for text in ["1", "2"] {
            let mut m = msg(&owner, text);
            m.ts = "2000-01-01T00:00:00Z".into();
            s.sequence_and_append(&mut m).unwrap();
        }
        assert!(matches!(
            s.check_limits("r_test", "haris", &limits),
            Err(Error::OverBudget(..))
        ));
    }

    fn msg_from(who: &Identity, from: &str, text: &str) -> Message {
        Message::new(
            Draft {
                room: "r_test".into(),
                from: from.into(),
                text: text.into(),
                kind: Some(MessageType::Chat),
                ..Default::default()
            },
            who,
        )
        .unwrap()
    }

    fn temp_db(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "diavlos-store-{tag}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("diavlos.db")
    }

    const T0: &str = "2026-01-01T00:00:00Z";

    #[test]
    fn the_outbox_accounts_for_every_message() {
        let owner = Identity::generate("haris", Kind::Human);
        let s = Store::open_memory().unwrap();
        s.create_room(&room(&owner)).unwrap();
        let msgs: Vec<Message> = (0..6)
            .map(|i| {
                msg_from(
                    &owner,
                    if i % 2 == 0 { "haris" } else { "bob" },
                    &format!("m{i}"),
                )
            })
            .collect();
        for m in &msgs {
            s.outbox_add(m).unwrap();
        }
        // Two lanes, each in the order queued.
        let lanes = s.outbox_lanes("r_test").unwrap();
        assert_eq!(lanes.len(), 2);
        assert!(lanes.iter().all(|(_, l)| l.len() == 3));

        // One of each outcome.
        let mut delivered = msgs[0].clone();
        s.sequence_and_append(&mut delivered).unwrap();
        s.outbox_wait(
            &msgs[1].id,
            "transport",
            "offline",
            Some(3),
            "2026-01-01T00:01:00Z",
            T0,
        )
        .unwrap();
        s.outbox_fail(&msgs[2].id, "denied", Some(6), T0).unwrap();
        s.outbox_fail(&msgs[3].id, "denied", Some(6), T0).unwrap();
        s.outbox_drop(&msgs[3].id, T0).unwrap();
        for i in 0..5 {
            let now = format!("2026-01-01T0{i}:00:00Z");
            s.outbox_unknown(&msgs[4].id, "huh", Some(1), &now, &now, 5, 3600)
                .unwrap();
        }
        let c = s.outbox_counts("r_test").unwrap();
        assert_eq!(
            c,
            OutboxCounts {
                pending: 1,
                waiting: 1,
                failed: 1,
                quarantined: 1,
                dropped: 1
            }
        );
        // Accepted = in the chain + everything still in the outbox.
        let in_chain = s.messages_after("r_test", 0, 100).unwrap().len() as u64;
        assert_eq!(
            in_chain + c.pending + c.waiting + c.failed + c.quarantined + c.dropped,
            msgs.len() as u64
        );
        // A dropped entry keeps its record but not its content.
        let dropped = s.outbox_get(&msgs[3].id).unwrap().unwrap();
        assert_eq!(dropped.state, OutboxState::Dropped);
        assert!(dropped.message.is_none());
        // Retry puts a quarantined message back as it was.
        assert!(s.outbox_retry(&msgs[4].id, T0).unwrap());
        let back = s.outbox_get(&msgs[4].id).unwrap().unwrap();
        assert_eq!(back.state, OutboxState::Pending);
        assert_eq!(back.message.as_ref(), Some(&msgs[4]));
        // Only failed or quarantined entries can be retried or dropped.
        assert!(!s.outbox_retry(&msgs[5].id, T0).unwrap());
        assert!(!s.outbox_drop(&msgs[5].id, T0).unwrap());
        assert!(!s.outbox_retry(&msgs[3].id, T0).unwrap());
    }

    #[test]
    fn an_unknown_answer_is_quarantined_only_after_both_count_and_time() {
        let owner = Identity::generate("haris", Kind::Human);
        let s = Store::open_memory().unwrap();
        s.create_room(&room(&owner)).unwrap();
        let m = msg(&owner, "x");
        s.outbox_add(&m).unwrap();
        let at = |mins: u32| format!("2026-01-01T{:02}:{:02}:00Z", mins / 60, mins % 60);
        // A fast burst: many answers, little time.
        for i in 0..20 {
            let st = s
                .outbox_unknown(&m.id, "huh", Some(1), &at(i), &at(i), 5, 3600)
                .unwrap();
            assert_eq!(st, OutboxState::Waiting);
        }
        // A different answer starts the count again.
        let st = s
            .outbox_unknown(&m.id, "other", Some(9), &at(61), &at(61), 5, 3600)
            .unwrap();
        assert_eq!(st, OutboxState::Waiting);
        // Slow: an hour passes but only a few answers came.
        for i in [90, 120, 180] {
            let st = s
                .outbox_unknown(&m.id, "other", Some(9), &at(i), &at(i), 5, 3600)
                .unwrap();
            assert_eq!(st, OutboxState::Waiting);
        }
        // The fifth same answer, more than an hour after the first.
        let st = s
            .outbox_unknown(&m.id, "other", Some(9), &at(200), &at(200), 5, 3600)
            .unwrap();
        assert_eq!(st, OutboxState::Quarantined);
        assert_eq!(s.outbox_count("r_test").unwrap(), 0);
        assert_eq!(s.outbox_counts("r_test").unwrap().quarantined, 1);
    }

    #[test]
    fn queued_messages_are_sealed_and_old_plain_ones_get_sealed_too() {
        let owner = Identity::generate("haris", Kind::Human);
        let key = [7u8; 32];
        let canary = "CANARY-4f1b9e-queued-text";
        let path = temp_db("seal");

        // An older helper, no key: three plain rows.
        let ids: Vec<String> = {
            let s = Store::open_with_key(&path, None).unwrap();
            s.create_room(&room(&owner)).unwrap();
            (0..3)
                .map(|i| {
                    let m = msg(&owner, &format!("{canary} {i}"));
                    s.outbox_add(&m).unwrap();
                    m.id
                })
                .collect()
        };

        // Sealing stops after two rows: the first batch of two committed,
        // the second rolled back.
        {
            let s = Store::open_unsealed(&path, Some(key)).unwrap();
            s.faults.arm_after("migration.row", 2);
            assert!(s.seal_legacy_outbox(2).is_err());
            let entries = s.outbox_entries(None).unwrap();
            assert_eq!(entries.len(), 3);
            assert!(
                entries.iter().all(|e| e.message.is_some()),
                "plain and sealed both read"
            );
        }

        // The next open carries on and finishes.
        {
            let s = Store::open_with_key(&path, Some(key)).unwrap();
            let entries = s.outbox_entries(None).unwrap();
            assert_eq!(entries.len(), 3);
            for (e, id) in entries.iter().zip(&ids) {
                assert_eq!(&e.msg_id, id);
                assert!(e.message.as_ref().unwrap().text.starts_with(canary));
            }
            // A new row is sealed before it is written.
            s.outbox_add(&msg(&owner, &format!("{canary} new")))
                .unwrap();
            // Failed and dropped rows stay sealed or empty.
            s.outbox_fail(&ids[0], "no", Some(6), T0).unwrap();
            s.outbox_fail(&ids[1], "no", Some(6), T0).unwrap();
            s.outbox_drop(&ids[1], T0).unwrap();
            s.checkpoint().unwrap();
        }
        let mut bytes = std::fs::read(&path).unwrap();
        bytes.extend(std::fs::read(path.with_extension("db-wal")).unwrap_or_default());
        let text = String::from_utf8_lossy(&bytes);
        assert!(
            !text.contains(canary),
            "plain queued text left in the database files"
        );

        // Without the key the rows are there but unreadable, never garbage.
        let s = Store::open_with_key(&path, None).unwrap();
        let e = s.outbox_get(&ids[2]).unwrap().unwrap();
        assert!(e.unreadable && e.message.is_none());
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn a_full_disk_refuses_the_message_and_leaves_nothing_half_written() {
        let owner = Identity::generate("haris", Kind::Human);
        let s = Store::open_memory_with_key([3u8; 32]).unwrap();
        s.create_room(&room(&owner)).unwrap();
        s.cap_size_for_test(4).unwrap();
        let big = "x".repeat(3000);
        let mut kept = Vec::new();
        let err = loop {
            let m = msg(&owner, &big);
            match s.outbox_add(&m) {
                Ok(()) => kept.push(m.id),
                Err(e) => break (e, m.id),
            }
            assert!(kept.len() < 1000, "never filled up");
        };
        match &err.0 {
            Error::Db(rusqlite::Error::SqliteFailure(f, _)) => {
                assert_eq!(f.code, rusqlite::ErrorCode::DiskFull)
            }
            other => panic!("expected a full disk, got {other}"),
        }
        // The refused one is not there; every earlier one is, whole.
        assert!(s.outbox_get(&err.1).unwrap().is_none());
        for id in &kept {
            assert!(s.outbox_get(id).unwrap().unwrap().message.is_some());
        }
        // The same when the chain itself is full: no half message.
        let before = s.message_count("r_test").unwrap();
        let mut m = msg(&owner, &big);
        assert!(s.sequence_and_append(&mut m).is_err());
        assert_eq!(s.message_count("r_test").unwrap(), before);
    }

    #[test]
    fn a_queued_copy_of_a_message_already_in_the_chain_is_cleared() {
        let owner = Identity::generate("haris", Kind::Human);
        let s = Store::open_memory().unwrap();
        s.create_room(&room(&owner)).unwrap();
        let mut m = msg(&owner, "stored at the home, answer lost");
        s.sequence_and_append(&mut m).unwrap();
        // Queued again afterwards (a retry of something the home had).
        s.outbox_add(&m).unwrap();
        assert!(!s.append(&m).unwrap(), "not stored twice");
        assert!(s.outbox_get(&m.id).unwrap().is_none());
        // A different message under the same id is not mistaken for it.
        let mut other = msg(&owner, "something else");
        other.id = m.id.clone();
        s.outbox_add(&other).unwrap();
        let _ = s.append(&m);
        assert!(s.outbox_get(&m.id).unwrap().is_some());
    }

    fn at(secs: i64) -> String {
        (chrono::DateTime::parse_from_rfc3339(T0).unwrap() + chrono::Duration::seconds(secs))
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
    }

    /// A room with `n` chat messages from bob, read by haris. Returns their seqs.
    fn owed(n: usize) -> (Store, Vec<u64>) {
        let owner = Identity::generate("haris", Kind::Human);
        let s = Store::open_memory().unwrap();
        s.create_room(&room(&owner)).unwrap();
        let mut seqs = Vec::new();
        for i in 0..n {
            let mut m = msg_from(&owner, "bob", &format!("m{i}"));
            s.sequence_and_append(&mut m).unwrap();
            seqs.push(m.seq);
        }
        (s, seqs)
    }

    fn lease(s: &Store, now: i64) -> Option<Delivery> {
        s.delivery_lease("r_test", "haris", &at(now), &at(now + 600), 5)
            .unwrap()
            .map(|(_, d)| d)
    }

    fn ack(s: &Store, d: &Delivery, now: i64) -> Result<Delivery> {
        s.delivery_settle(d.token.as_deref().unwrap(), &Settle::Ack, &at(now), 5)
    }

    #[test]
    fn the_bookmark_never_passes_a_message_still_owed() {
        let (s, seqs) = owed(3);
        let a = lease(&s, 0).unwrap();
        let b = lease(&s, 0).unwrap();
        let c = lease(&s, 0).unwrap();
        assert_eq!((a.seq, b.seq, c.seq), (seqs[0], seqs[1], seqs[2]));
        assert!(lease(&s, 0).is_none(), "all three are out");
        // Acks out of order: the bookmark waits for the gap.
        ack(&s, &c, 1).unwrap();
        assert_eq!(s.bookmark("r_test", "haris").unwrap(), 0);
        ack(&s, &a, 1).unwrap();
        assert_eq!(s.bookmark("r_test", "haris").unwrap(), seqs[0]);
        ack(&s, &b, 1).unwrap();
        assert_eq!(s.bookmark("r_test", "haris").unwrap(), seqs[2]);
        // Acking again is fine.
        ack(&s, &b, 2).unwrap();
    }

    #[test]
    fn a_lease_that_runs_out_is_handed_out_again_and_the_old_token_is_refused() {
        let (s, seqs) = owed(1);
        let first = lease(&s, 0).unwrap();
        assert_eq!(first.attempt, 1);
        assert!(lease(&s, 599).is_none(), "still leased");
        let second = lease(&s, 600).unwrap();
        assert_eq!(second.seq, seqs[0]);
        assert_eq!(second.attempt, 2);
        assert_ne!(first.token, second.token);
        // The first worker finishes late: it cannot settle the newer one.
        assert!(matches!(ack(&s, &first, 700), Err(Error::Denied(_))));
        let renew = Settle::Renew {
            lease_until: at(5000),
        };
        assert!(s
            .delivery_settle(first.token.as_deref().unwrap(), &renew, &at(700), 5)
            .is_err());
        assert_eq!(s.bookmark("r_test", "haris").unwrap(), 0);
        ack(&s, &second, 700).unwrap();
        assert_eq!(s.bookmark("r_test", "haris").unwrap(), seqs[0]);
    }

    #[test]
    fn a_late_ack_counts_if_nobody_was_handed_it_since() {
        let (s, seqs) = owed(1);
        let d = lease(&s, 0).unwrap();
        ack(&s, &d, 5000).unwrap();
        assert_eq!(s.bookmark("r_test", "haris").unwrap(), seqs[0]);
    }

    #[test]
    fn renew_holds_it_and_nack_hands_it_back_later() {
        let (s, _) = owed(1);
        let d = lease(&s, 0).unwrap();
        let t = d.token.clone().unwrap();
        s.delivery_settle(
            &t,
            &Settle::Renew {
                lease_until: at(2000),
            },
            &at(500),
            5,
        )
        .unwrap();
        assert!(lease(&s, 1500).is_none(), "renewed past the first lease");
        s.delivery_settle(&t, &Settle::Nack { retry_at: at(3000) }, &at(1600), 5)
            .unwrap();
        assert!(lease(&s, 2999).is_none(), "not before retry_at");
        let again = lease(&s, 3000).unwrap();
        assert_eq!(again.attempt, 2);
    }

    #[test]
    fn five_strikes_quarantine_it_the_lane_moves_on_and_replay_brings_it_back() {
        let (s, seqs) = owed(2);
        let mut now = 0;
        for i in 1..=5 {
            let d = lease(&s, now).unwrap();
            assert_eq!((d.seq, d.attempt), (seqs[0], i));
            now += 600;
        }
        // The fifth lease ran out: quarantined, and the next one is handed out.
        let next = lease(&s, now).unwrap();
        assert_eq!(next.seq, seqs[1]);
        let q = s.deliveries("r_test", Some("haris")).unwrap();
        assert!(q
            .iter()
            .any(|d| d.seq == seqs[0] && d.state == DeliveryState::Quarantined));
        ack(&s, &next, now).unwrap();
        assert_eq!(
            s.bookmark("r_test", "haris").unwrap(),
            seqs[1],
            "quarantine counts as settled"
        );
        // Replay: handed out again first, as new.
        assert!(s
            .delivery_replay("r_test", "haris", seqs[0], &at(now))
            .unwrap());
        let back = lease(&s, now).unwrap();
        assert_eq!((back.seq, back.attempt), (seqs[0], 1));
        ack(&s, &back, now).unwrap();
        assert!(lease(&s, now).is_none());
    }

    #[test]
    fn own_and_housekeeping_messages_settle_by_themselves() {
        let owner = Identity::generate("haris", Kind::Human);
        let s = Store::open_memory().unwrap();
        s.create_room(&room(&owner)).unwrap();
        let mut mine = msg(&owner, "from me");
        s.sequence_and_append(&mut mine).unwrap();
        let mut sys = msg(&owner, "joined");
        sys.kind = MessageType::System;
        s.sequence_and_append(&mut sys).unwrap();
        let mut theirs = msg_from(&owner, "bob", "for you");
        s.sequence_and_append(&mut theirs).unwrap();
        let d = lease(&s, 0).unwrap();
        assert_eq!(d.seq, theirs.seq);
        // The two before it were never owed.
        assert_eq!(s.bookmark("r_test", "haris").unwrap(), sys.seq);
    }

    #[test]
    fn peek_shows_what_is_owed_without_taking_it() {
        let (s, seqs) = owed(3);
        let d = lease(&s, 0).unwrap();
        s.delivery_settle(
            d.token.as_deref().unwrap(),
            &Settle::Nack { retry_at: at(100) },
            &at(1),
            5,
        )
        .unwrap();
        let peeked: Vec<u64> = s
            .delivery_peek("r_test", "haris", &at(2), 10)
            .unwrap()
            .iter()
            .map(|m| m.seq)
            .collect();
        assert_eq!(peeked, vec![seqs[1], seqs[2]], "the delayed one is not due");
        let later: Vec<u64> = s
            .delivery_peek("r_test", "haris", &at(100), 10)
            .unwrap()
            .iter()
            .map(|m| m.seq)
            .collect();
        assert_eq!(later, seqs);
        // Peeking took nothing.
        assert_eq!(lease(&s, 100).unwrap().seq, seqs[0]);
    }

    #[test]
    fn read_and_ack_settles_exactly_what_was_read() {
        let (s, seqs) = owed(3);
        s.delivery_ack_seqs("r_test", "haris", &[seqs[1]], &at(0))
            .unwrap();
        assert_eq!(s.bookmark("r_test", "haris").unwrap(), 0);
        s.delivery_ack_seqs("r_test", "haris", &[seqs[0]], &at(0))
            .unwrap();
        assert_eq!(s.bookmark("r_test", "haris").unwrap(), seqs[1]);
        assert_eq!(lease(&s, 0).unwrap().seq, seqs[2]);
    }

    #[test]
    fn a_crash_after_the_lease_is_written_hands_it_out_again_later() {
        let owner = Identity::generate("haris", Kind::Human);
        let path = temp_db("lease");
        let seq = {
            let s = Store::open_with_key(&path, None).unwrap();
            s.create_room(&room(&owner)).unwrap();
            let mut m = msg_from(&owner, "bob", "work");
            s.sequence_and_append(&mut m).unwrap();
            // Leased and committed; the answer never reached the reader.
            lease(&s, 0).unwrap();
            m.seq
        };
        let s = Store::open_with_key(&path, None).unwrap();
        assert!(lease(&s, 10).is_none(), "the lease survived the restart");
        let again = lease(&s, 600).unwrap();
        assert_eq!((again.seq, again.attempt), (seq, 2));
        // A lease write that fails leaves nothing behind.
        s.faults.arm("delivery.lease");
        assert!(s
            .delivery_lease("r_test", "haris", &at(1300), &at(1900), 5)
            .is_err());
        let d = s.deliveries("r_test", Some("haris")).unwrap();
        assert_eq!(d[0].attempt, 2);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
}
