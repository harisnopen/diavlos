//! The inbox: one SQLite file per helper.
//!
//! Envelopes and content live in separate tables so a delete leaves a
//! tombstone and the chain still proves nothing else changed. Each reader
//! keeps its own bookmark. Reading never deletes. A message is on disk
//! before `send` returns. Delivery is at-least-once, dedup by id.

use std::path::Path;
use std::sync::Mutex;

use rusqlite::{params, Connection, OptionalExtension, Row};
use rusqlite_migration::{Migrations, M};

use crate::error::{Error, Result};
use crate::invite::Invite;
use crate::keys::{Kind, PublicKey};
use crate::limits::Limits;
use crate::message::{Content, DataClass, Message, GENESIS_PREV};
use crate::room::{Member, Role, Room};

const MIGRATIONS: &[M<'static>] = &[M::up(
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
)];

/// The store. Safe to share between threads; one connection, one lock.
pub struct Store {
    conn: Mutex<Connection>,
}

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

impl Store {
    /// Open (or create) the database file and bring the schema up to date.
    /// Mixed versions are normal; migrations run from v0.1 on.
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path)?;
        Self::init(conn)
    }

    /// An in-memory store for tests.
    pub fn open_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(mut conn: Connection) -> Result<Self> {
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "FULL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        Migrations::from_slice(MIGRATIONS).to_latest(&mut conn)?;
        Ok(Store {
            conn: Mutex::new(conn),
        })
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
            "UPDATE rooms SET about=?2, retention_days=?3, class=?4, paused=?5, hold=?6, home_node=?7, home_hints=?8 WHERE id=?1",
            params![
                room.id,
                room.about,
                room.retention_days,
                room.class.as_str(),
                room.paused as i32,
                room.hold as i32,
                room.home_node,
                serde_json::to_string(&room.home_hints)?,
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
        let written = Self::append_in(&tx, msg)?;
        tx.commit()?;
        Ok(written)
    }

    fn append_in(conn: &Connection, msg: &Message) -> Result<bool> {
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
            "INSERT INTO messages (room_id, seq, id, from_name, type, ts, prev, chain_hash, envelope)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
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
            ],
        )?;
        conn.execute(
            "INSERT INTO contents (msg_id, body, deleted) VALUES (?1, ?2, 0)",
            params![msg.id, serde_json::to_string(&msg.content())?],
        )?;
        Ok(true)
    }

    /// Give a message the next place in the chain and store it. This is
    /// what the room's home helper does. Atomic: no two messages get the
    /// same seq.
    pub fn sequence_and_append(&self, msg: &mut Message) -> Result<()> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        let (last_seq, last_hash) = Self::chain_head_in(&tx, &msg.room)?;
        msg.sequence(last_seq + 1, &last_hash);
        Self::append_in(&tx, msg)?;
        tx.commit()?;
        Ok(())
    }

    fn row_to_message(row: &Row<'_>) -> rusqlite::Result<Message> {
        let envelope: String = row.get("envelope")?;
        let body: Option<String> = row.get("body")?;
        let deleted: Option<i32> = row.get("deleted")?;
        let mut v: serde_json::Value =
            serde_json::from_str(&envelope).map_err(|_| rusqlite::Error::InvalidQuery)?;
        let content: Content = match (body, deleted) {
            (Some(b), Some(0)) => serde_json::from_str(&b).unwrap_or_default(),
            _ => Content::default(),
        };
        if let Some(obj) = v.as_object_mut() {
            obj.remove("content_hash");
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
        let rows = stmt.query_map(params![room_id, after as i64, limit], Self::row_to_message)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn message_by_id(&self, id: &str) -> Result<Option<Message>> {
        let conn = self.lock();
        Ok(conn
            .query_row(
                "SELECT m.envelope, c.body, c.deleted FROM messages m
                 LEFT JOIN contents c ON c.msg_id = m.id WHERE m.id=?1",
                params![id],
                Self::row_to_message,
            )
            .optional()?)
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

    // ---- outbox --------------------------------------------------------

    /// Park a signed, unsequenced message until the room's home is
    /// reachable. On disk before `send` returns.
    pub fn outbox_add(&self, msg: &Message) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "INSERT OR IGNORE INTO outbox (msg_id, room_id, message, created) VALUES (?1, ?2, ?3, ?4)",
            params![msg.id, msg.room, serde_json::to_string(msg)?, msg.ts],
        )?;
        Ok(())
    }

    pub fn outbox_list(&self, room_id: &str) -> Result<Vec<Message>> {
        let conn = self.lock();
        let mut stmt =
            conn.prepare("SELECT message FROM outbox WHERE room_id=?1 ORDER BY created, msg_id")?;
        let rows = stmt.query_map(params![room_id], |r| {
            let s: String = r.get(0)?;
            serde_json::from_str::<Message>(&s).map_err(|_| rusqlite::Error::InvalidQuery)
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn outbox_remove(&self, msg_id: &str) -> Result<()> {
        let conn = self.lock();
        conn.execute("DELETE FROM outbox WHERE msg_id=?1", params![msg_id])?;
        Ok(())
    }

    pub fn outbox_note_failure(&self, msg_id: &str, err: &str) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "UPDATE outbox SET attempts=attempts+1, last_error=?2 WHERE msg_id=?1",
            params![msg_id, err],
        )?;
        Ok(())
    }

    pub fn outbox_count(&self, room_id: &str) -> Result<u64> {
        let conn = self.lock();
        let n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM outbox WHERE room_id=?1",
            params![room_id],
            |r| r.get(0),
        )?;
        Ok(n as u64)
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
                "SELECT COUNT(*) FROM messages WHERE room_id=?1 AND from_name=?2 AND ts>=?3",
                params![room_id, f, since],
                |r| r.get(0),
            )?,
            None => conn.query_row(
                "SELECT COUNT(*) FROM messages WHERE room_id=?1 AND ts>=?2",
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
            return Err(Error::OverBudget(format!(
                "{from} sent {per_minute} messages in the last minute; the limit is {}",
                limits.per_minute_per_sender
            )));
        }
        let today = Self::count_since(&conn, room_id, None, &day_start)?;
        if today >= limits.daily_per_room {
            return Err(Error::OverBudget(format!(
                "room has used its daily budget of {} messages",
                limits.daily_per_room
            )));
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
        assert_eq!(s.outbox_list("r_test").unwrap(), vec![m.clone()]);
        s.outbox_note_failure(&m.id, "offline").unwrap();
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
            Err(Error::OverBudget(_))
        ));
        // Another sender is under the per-minute limit but the daily
        // budget is about to be hit: the alert fires on the third message.
        assert!(s.check_limits("r_test", "bob", &limits).unwrap());
    }
}
