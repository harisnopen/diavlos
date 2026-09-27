//! Files on messages, helper side. See docs/FILES.md.
//!
//! The sender's helper copies each file into its blob folder and puts a
//! reference in the message. Before the message goes to the room's home,
//! the bytes go there, a signed piece at a time, over the same link. The
//! home checks the whole file against its fingerprint, then takes the
//! message. A reader's `get` fetches the bytes from the home and checks
//! them again. Nothing here opens or runs a file.

use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use data_encoding::BASE64;
use diavlos_client::proto::SavedFile;
use diavlos_core::files::{self, FileRef, Kind, HARD_MAX_FILES, HARD_MAX_FILE_BYTES};
use diavlos_core::store::{FileRefRow, ORPHAN_FILE_SECS};
use diavlos_core::{Error, FileMode, LocalMember, Message, Policy, Result, Room, StoredFile};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tracing::{info, warn};

use super::outbox::Outcome;
use super::Helper;
use crate::net::{file_signing_bytes, Link, Wire};

/// Bytes per piece on the wire.
pub const CHUNK: usize = 1024 * 1024;
/// What a helper that can carry files says in its hello.
pub const FEATURE: &str = "files";
/// Text files up to this size get the secret scan before they are sent.
const SCAN_MAX_BYTES: u64 = 10 * 1024 * 1024;
/// How old a signed file request may be.
const MAX_REQUEST_AGE_SECS: i64 = 300;

fn mb(bytes: u64) -> String {
    let m = bytes as f64 / (1024.0 * 1024.0);
    let n = if m >= 10.0 {
        format!("{m:.0}")
    } else {
        let n = format!("{m:.2}");
        n.trim_end_matches('0').trim_end_matches('.').to_string()
    };
    format!("{n} MB")
}

fn blob_path(helper: &Helper, room_id: &str, id: &str) -> PathBuf {
    helper
        .paths
        .blobs(room_id)
        .join(id.strip_prefix("sha256:").unwrap_or(id))
}

fn part_path(helper: &Helper, room_id: &str, id: &str) -> PathBuf {
    let mut p = blob_path(helper, room_id, id).into_os_string();
    p.push(".part");
    PathBuf::from(p)
}

/// Make a folder only this user can open.
fn private_dir(dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// Readable and writable by this user only, runnable by nobody.
fn private_file(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> Result<T> + Send + 'static) -> Result<T> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| Error::Other(format!("file task failed: {e}")))?
}

// ---- the home's rules ----------------------------------------------------

/// May `member` put a file of `size` bytes into this room? Run by the home
/// before it takes any bytes, and again when the last piece is in.
fn admit(
    helper: &Helper,
    room: &Room,
    policy: &Policy,
    member: &str,
    id: &str,
    size: u64,
) -> Result<()> {
    if policy.file_mode(room.class) == FileMode::Off {
        return Err(Error::Denied(format!(
            "room {} takes no files (set files = \"any\" or \"safe\" in its policy to allow them)",
            room.name
        )));
    }
    if size > policy.max_file_bytes() {
        return Err(Error::Denied(format!(
            "that file is {}; room {} takes files up to {} (max_file_mb in its policy)",
            mb(size),
            room.name,
            mb(policy.max_file_bytes())
        )));
    }
    if helper.store.file_stored(&room.id, id)?.is_some() {
        return Ok(());
    }
    let since = (helper.now() - chrono::Duration::hours(24))
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let (kept, mine) = helper.store.file_bytes(&room.id, member, &since)?;
    // Unfinished uploads take room too, so pieces cannot fill the disk.
    let in_room = kept + unfinished_bytes(helper, room, id);
    if in_room + size > policy.room_file_bytes() {
        return Err(Error::Denied(format!(
            "room {}'s file store is full ({} of {}); nothing was deleted to make room",
            room.name,
            mb(in_room),
            mb(policy.room_file_bytes())
        )));
    }
    if mine + size > policy.daily_file_bytes() {
        return Err(Error::OverBudget(
            format!(
                "{member} has sent {} of files in the last 24 hours; room {} allows {} a day",
                mb(mine),
                room.name,
                mb(policy.daily_file_bytes())
            ),
            Some(3600),
        ));
    }
    Ok(())
}

/// Bytes in unfinished uploads for a room, other than `id`'s own.
fn unfinished_bytes(helper: &Helper, room: &Room, id: &str) -> u64 {
    let own = part_path(helper, &room.id, id);
    std::fs::read_dir(helper.paths.blobs(&room.id))
        .map(|dir| {
            dir.flatten()
                .filter(|e| e.path().extension().is_some_and(|x| x == "part") && e.path() != own)
                .filter_map(|e| e.metadata().ok())
                .map(|m| m.len())
                .sum()
        })
        .unwrap_or(0)
}

/// In a `safe` room, only what is on the safe list, by its bytes.
fn check_kind(room: &Room, policy: &Policy, name: &str, kind: Kind) -> Result<()> {
    if policy.file_mode(room.class) == FileMode::Safe && !kind.is_safe() {
        return Err(Error::Denied(format!(
            "room {} takes only plain text, pictures, PDF and ZIP (files = \"safe\"); {name} looks like {}",
            room.name,
            kind.as_str()
        )));
    }
    Ok(())
}

/// Home side: a message may go in only if every file it points at is here.
pub fn check_message(helper: &Helper, room: &Room, policy: &Policy, msg: &Message) -> Result<()> {
    let refs = files::refs(&msg.data)?;
    if refs.is_empty() {
        return Ok(());
    }
    if policy.file_mode(room.class) == FileMode::Off {
        return Err(Error::Denied(format!("room {} takes no files", room.name)));
    }
    if refs.len() > policy.max_files() {
        return Err(Error::Denied(format!(
            "room {} takes at most {} files on one message (max_files_per_message in its policy)",
            room.name,
            policy.max_files()
        )));
    }
    for r in &refs {
        match helper.store.file_stored(&room.id, &r.id)? {
            Some(f) if f.size == r.size => {}
            Some(_) => {
                return Err(Error::Invalid(format!(
                    "{} does not have the size the room's home has for it",
                    r.name
                )))
            }
            None => {
                return Err(Error::Invalid(format!(
                    "{} is not on the room's home; send it again",
                    r.name
                )))
            }
        }
    }
    Ok(())
}

// ---- sending ---------------------------------------------------------------

/// Copy a file into the blob folder, hashing it on the way. Returns its id
/// and size. The copy is what gets sent, so changing the original after
/// `send` changes nothing.
fn copy_in(src: &Path, dir: &Path, max: u64) -> Result<(String, u64)> {
    let tmp = dir.join(format!("{}.tmp", rand::random::<u64>()));
    let result = (|| {
        let mut from = std::fs::File::open(src)?;
        let mut to = std::fs::File::create(&tmp)?;
        let mut h = Sha256::new();
        let mut buf = vec![0u8; 256 * 1024];
        let mut size = 0u64;
        loop {
            let n = from.read(&mut buf)?;
            if n == 0 {
                break;
            }
            size += n as u64;
            if size > max {
                return Err(Error::Denied(format!(
                    "{} is over {}",
                    src.display(),
                    mb(max)
                )));
            }
            h.update(&buf[..n]);
            to.write_all(&buf[..n])?;
        }
        to.sync_all()?;
        let id = format!("sha256:{}", data_encoding::HEXLOWER.encode(&h.finalize()));
        Ok((id, size))
    })();
    match result {
        Ok((id, size)) => {
            let dest = dir.join(id.strip_prefix("sha256:").unwrap_or(&id));
            if dest.exists() {
                std::fs::remove_file(&tmp)?;
            } else {
                std::fs::rename(&tmp, &dest)?;
                private_file(&dest)?;
            }
            Ok((id, size))
        }
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e)
        }
    }
}

/// Sender side: turn the paths on a `send` into references on the
/// message. On the room's home the files are checked and kept right away;
/// anywhere else the home checks them when they arrive.
pub async fn attach(
    helper: &Helper,
    room: &Room,
    from: &str,
    paths: &[String],
    data: &mut Value,
) -> Result<()> {
    if data.get("files").is_some() {
        return Err(Error::Invalid(
            "data.files is filled in by the helper; attach files with --file (MCP: files)".into(),
        ));
    }
    if paths.is_empty() {
        return Ok(());
    }
    if !(data.is_null() || data.is_object()) {
        return Err(Error::Invalid(
            "a message with files needs its data to be an object".into(),
        ));
    }
    let at_home = helper.is_home(room);
    let policy = helper.policy(room);
    let most = if at_home {
        policy.max_files()
    } else {
        HARD_MAX_FILES
    };
    if at_home && policy.file_mode(room.class) == FileMode::Off {
        return Err(Error::Denied(format!(
            "room {} takes no files (set files = \"any\" or \"safe\" in its policy to allow them)",
            room.name
        )));
    }
    if paths.len() > most {
        return Err(Error::Denied(format!(
            "at most {most} files on one message in room {}",
            room.name
        )));
    }
    let dir = helper.paths.blobs(&room.id);
    private_dir(&dir)?;
    let max = if at_home {
        policy.max_file_bytes()
    } else {
        HARD_MAX_FILE_BYTES
    };
    let mut refs = Vec::new();
    for p in paths {
        let src = PathBuf::from(p);
        let meta =
            std::fs::metadata(&src).map_err(|e| Error::Invalid(format!("cannot read {p}: {e}")))?;
        if meta.is_dir() {
            return Err(Error::Invalid(format!("{p} is a folder; zip it first")));
        }
        if !meta.is_file() {
            return Err(Error::Invalid(format!("{p} is not a plain file")));
        }
        if meta.len() > max {
            return Err(Error::Denied(format!(
                "{p} is {}; room {} takes files up to {}",
                mb(meta.len()),
                room.name,
                mb(max)
            )));
        }
        let name = files::clean_name(
            &src.file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default(),
        );
        let (id, size) = {
            let (src, dir) = (src.clone(), dir.clone());
            blocking(move || copy_in(&src, &dir, max)).await?
        };
        let stored = blob_path(helper, &room.id, &id);
        let kind = {
            let stored = stored.clone();
            blocking(move || files::sniff_file(&stored)).await?
        };
        // The same secret scan a message gets, over a text file.
        if helper.config.helper.secret_scan && kind == Kind::Text && size <= SCAN_MAX_BYTES {
            let text = std::fs::read_to_string(&stored)?;
            if let Some(hit) = diavlos_core::secrets::find_secret(&text) {
                helper.emit(
                    "secret_refused",
                    Some(&room.id),
                    serde_json::json!({"kind": hit, "file": true}),
                );
                return Err(Error::Denied(format!(
                    "refused: {name} looks like it holds a {hit}, so it was not sent. \
                     (secret_scan = false in config turns the scan off)"
                )));
            }
        }
        let mime = match kind {
            Kind::Other | Kind::Program => files::type_for_name(&name).to_string(),
            k => k.mime().to_string(),
        };
        if at_home {
            admit(helper, room, &policy, from, &id, size)?;
            check_kind(room, &policy, &name, kind)?;
            helper.store.file_add(&StoredFile {
                room_id: room.id.clone(),
                hash: id.clone(),
                size,
                kind: kind.as_str().into(),
                uploader: from.into(),
                created: helper.now_ts(),
            })?;
        }
        refs.push(FileRef {
            id,
            name,
            size,
            mime,
        });
    }
    if data.is_null() {
        *data = Value::Object(Default::default());
    }
    data["files"] = serde_json::to_value(&refs)?;
    Ok(())
}

/// Does the room's home speak files?
fn home_takes_files(helper: &Helper, room: &Room) -> bool {
    helper.node_has(&room.home_node, FEATURE)
}

fn old_home(room: &Room) -> Error {
    Error::Denied(format!(
        "room {}'s home runs an older diavlos that cannot carry files; ask its owner to update",
        room.name
    ))
}

/// Answer to a file request, as the outbox sorts it.
async fn file_request(
    link: &Arc<dyn Link>,
    req: &Wire,
) -> std::result::Result<(u64, bool), Outcome> {
    match link.request(req).await {
        Err(e) => Err(Outcome::local(e)),
        Ok(Wire::FileStored { have, done }) => Ok((have, done)),
        Ok(Wire::Err {
            code,
            msg,
            fate,
            retry_after,
        }) => Err(Outcome::from_reply(code, &msg, fate, retry_after)),
        Ok(other) => Err(Outcome::Unknown(Error::Invalid(format!(
            "unexpected {}",
            other.label()
        )))),
    }
}

/// Member side: make sure the home holds every file `msg` points at,
/// sending what it is missing. Runs before the message itself is sent.
pub async fn upload(
    helper: &Helper,
    room: &Room,
    link: &Arc<dyn Link>,
    msg: &Message,
) -> std::result::Result<(), Outcome> {
    let refs = files::refs(&msg.data).map_err(Outcome::Definitive)?;
    if refs.is_empty() {
        return Ok(());
    }
    if !home_takes_files(helper, room) {
        return Err(Outcome::Definitive(old_home(room)));
    }
    let lm = helper
        .store
        .local_members(&room.id)
        .map_err(Outcome::local)?
        .into_iter()
        .find(|lm| lm.member.name == msg.from)
        .ok_or_else(|| Outcome::Definitive(Error::NotInRoom(room.name.clone())))?;
    let id = helper
        .identity(&lm.identity)
        .await
        .map_err(Outcome::local)?;
    let me = helper.net.node_id();
    let sign = |op: &str, file: &str, at: u64, chunk: &str, ts: &str| {
        diavlos_core::Signer::sign(
            id.as_ref(),
            &file_signing_bytes(op, &room.id, &msg.from, file, at, chunk, ts, &me),
        )
    };
    for f in &refs {
        let ts = helper.now_ts();
        let ask = Wire::FileAsk {
            room_id: room.id.clone(),
            name: msg.from.clone(),
            file: f.id.clone(),
            size: f.size,
            sig: sign("ask", &f.id, f.size, "", &ts),
            ts,
        };
        let (mut have, mut done) = file_request(link, &ask).await?;
        let path = blob_path(helper, &room.id, &f.id);
        while !done {
            let (p, at) = (path.clone(), have);
            let piece = blocking(move || {
                let mut file = std::fs::File::open(&p)?;
                file.seek(SeekFrom::Start(at))?;
                let mut buf = Vec::with_capacity(CHUNK);
                file.take(CHUNK as u64).read_to_end(&mut buf)?;
                Ok(buf)
            })
            .await
            .map_err(|e| {
                Outcome::Definitive(Error::Invalid(format!(
                    "{} is no longer on this machine to send ({e}); send it again",
                    f.name
                )))
            })?;
            if piece.is_empty() {
                return Err(Outcome::Definitive(Error::Invalid(format!(
                    "{} on this machine is shorter than it was; send it again",
                    f.name
                ))));
            }
            let chunk = data_encoding::HEXLOWER.encode(&Sha256::digest(&piece));
            let ts = helper.now_ts();
            let put = Wire::FilePut {
                room_id: room.id.clone(),
                name: msg.from.clone(),
                file: f.id.clone(),
                size: f.size,
                offset: have,
                sig: sign("put", &f.id, have, &chunk, &ts),
                data: BASE64.encode(&piece),
                ts,
            };
            (have, done) = file_request(link, &put).await?;
        }
    }
    Ok(())
}

// ---- the home serving file requests ---------------------------------------

/// Check a signed file request: this helper is the room's home, the
/// signer is a member, the signature covers this node and is fresh.
#[allow(clippy::too_many_arguments)]
async fn authorize(
    helper: &Helper,
    link: &Arc<dyn Link>,
    room_id: &str,
    name: &str,
    op: &str,
    file: &str,
    at: u64,
    chunk: &str,
    ts: &str,
    sig: &str,
) -> Result<Room> {
    let room = helper
        .store
        .room_by_id(room_id)?
        .ok_or_else(|| Error::NotInRoom(room_id.to_string()))?;
    if !helper.is_home(&room) {
        return Err(Error::Denied("not the home of that room".into()));
    }
    let member = helper
        .store
        .member_by_name(&room.id, name)?
        .filter(|m| !m.revoked)
        .ok_or_else(|| Error::Denied("not a member of that room".into()))?;
    let remote = link.remote_node();
    member.key.verify(
        &file_signing_bytes(op, room_id, name, file, at, chunk, ts, &remote),
        sig,
    )?;
    let age = chrono::DateTime::parse_from_rfc3339(ts)
        .map(|t| {
            (helper.now() - t.with_timezone(&chrono::Utc))
                .num_seconds()
                .abs()
        })
        .unwrap_or(i64::MAX);
    if age > MAX_REQUEST_AGE_SECS {
        return Err(Error::Denied("file request is too old".into()));
    }
    helper.node_check(&room, &member, &remote).await?;
    files::check_id(file)?;
    Ok(room)
}

/// The home's answer to one file request.
pub async fn serve(helper: &Helper, link: &Arc<dyn Link>, req: Wire) -> Result<Wire> {
    match req {
        Wire::FileAsk {
            room_id,
            name,
            file,
            size,
            ts,
            sig,
        } => {
            let room = authorize(
                helper, link, &room_id, &name, "ask", &file, size, "", &ts, &sig,
            )
            .await?;
            if room.paused {
                return Err(Error::RoomPaused(room.name.clone()));
            }
            let policy = helper.policy(&room);
            admit(helper, &room, &policy, &name, &file, size)?;
            if helper.store.file_stored(&room.id, &file)?.is_some() {
                return Ok(Wire::FileStored {
                    have: size,
                    done: true,
                });
            }
            let part = part_path(helper, &room.id, &file);
            let mut have = std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
            if have > size {
                std::fs::remove_file(&part)?;
                have = 0;
            }
            Ok(Wire::FileStored { have, done: false })
        }
        Wire::FilePut {
            room_id,
            name,
            file,
            size,
            offset,
            data,
            ts,
            sig,
        } => {
            let bytes = BASE64
                .decode(data.as_bytes())
                .map_err(|_| Error::Invalid("a file piece is not base64".into()))?;
            let chunk = data_encoding::HEXLOWER.encode(&Sha256::digest(&bytes));
            let room = authorize(
                helper, link, &room_id, &name, "put", &file, offset, &chunk, &ts, &sig,
            )
            .await?;
            if room.paused {
                return Err(Error::RoomPaused(room.name.clone()));
            }
            if bytes.len() > CHUNK {
                return Err(Error::Invalid("a file piece is too big".into()));
            }
            let policy = helper.policy(&room);
            admit(helper, &room, &policy, &name, &file, size)?;
            if helper.store.file_stored(&room.id, &file)?.is_some() {
                return Ok(Wire::FileStored {
                    have: size,
                    done: true,
                });
            }
            let dir = helper.paths.blobs(&room.id);
            private_dir(&dir)?;
            let part = part_path(helper, &room.id, &file);
            let have = std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
            if offset != have {
                // Out of step (a lost answer): say where to go on from.
                return Ok(Wire::FileStored { have, done: false });
            }
            if offset + bytes.len() as u64 > size {
                return Err(Error::Invalid("more bytes than the file's size".into()));
            }
            {
                let mut f = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&part)?;
                f.write_all(&bytes)?;
                f.sync_data()?;
            }
            let have = have + bytes.len() as u64;
            if have < size {
                return Ok(Wire::FileStored { have, done: false });
            }
            // All in: check it, then keep it.
            let p = part.clone();
            let (got, got_size, kind) = blocking(move || {
                let (h, s) = files::hash_file(&p)?;
                Ok((h, s, files::sniff_file(&p)?))
            })
            .await?;
            let keep = (|| {
                if got != file || got_size != size {
                    return Err(Error::Invalid(
                        "the file did not match its fingerprint; it was thrown away".into(),
                    ));
                }
                check_kind(&room, &policy, "that file", kind)?;
                admit(helper, &room, &policy, &name, &file, size)?;
                Ok(())
            })();
            if let Err(e) = keep {
                let _ = std::fs::remove_file(&part);
                return Err(e);
            }
            let dest = blob_path(helper, &room.id, &file);
            std::fs::rename(&part, &dest)?;
            private_file(&dest)?;
            helper.store.file_add(&StoredFile {
                room_id: room.id.clone(),
                hash: file.clone(),
                size,
                kind: kind.as_str().into(),
                uploader: name.clone(),
                created: helper.now_ts(),
            })?;
            info!(room = %room.id, from = %name, size, "file stored");
            helper.emit(
                "file_stored",
                Some(&room.id),
                serde_json::json!({"id": file, "size": size, "from": name, "kind": kind.as_str()}),
            );
            Ok(Wire::FileStored { have, done: true })
        }
        Wire::FileGet {
            room_id,
            name,
            file,
            offset,
            ts,
            sig,
        } => {
            let room = authorize(
                helper, link, &room_id, &name, "get", &file, offset, "", &ts, &sig,
            )
            .await?;
            if helper.store.file_refs(&room.id, &file)?.is_empty() {
                return Err(Error::Denied(
                    "no message in that room points at that file".into(),
                ));
            }
            let stored = helper
                .store
                .file_stored(&room.id, &file)?
                .ok_or_else(|| Error::Invalid("the room's home no longer has that file".into()))?;
            if offset > stored.size {
                return Err(Error::Invalid(
                    "that offset is past the end of the file".into(),
                ));
            }
            let p = blob_path(helper, &room.id, &file);
            let piece = blocking(move || {
                let mut f = std::fs::File::open(&p)?;
                f.seek(SeekFrom::Start(offset))?;
                let mut buf = Vec::with_capacity(CHUNK);
                f.take(CHUNK as u64).read_to_end(&mut buf)?;
                Ok(buf)
            })
            .await?;
            if offset + piece.len() as u64 >= stored.size {
                helper
                    .store
                    .file_fetched(&room.id, &file, &name, &helper.now_ts())?;
            }
            Ok(Wire::FileChunk {
                data: BASE64.encode(&piece),
                size: stored.size,
            })
        }
        other => Err(Error::Invalid(format!("unexpected {}", other.label()))),
    }
}

// ---- getting ---------------------------------------------------------------

/// Which files a `get` means: every file on a message, or one file by its
/// id or the start of it.
fn targets(helper: &Helper, room: &Room, id: &str) -> Result<Vec<FileRefRow>> {
    if id.starts_with("m_") {
        let refs: Vec<FileRefRow> = helper
            .store
            .file_refs_of(id)?
            .into_iter()
            .filter(|r| r.room_id == room.id)
            .collect();
        if refs.is_empty() {
            return Err(Error::Invalid(format!(
                "no message {id} with files in room {}",
                room.name
            )));
        }
        return Ok(refs);
    }
    let hex = id
        .strip_prefix("sha256:")
        .unwrap_or(id)
        .to_ascii_lowercase();
    if hex.len() < 12 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(Error::Invalid(format!(
            "{id:?} is not a file id (sha256:…, or at least 12 hex digits of it) or a message id"
        )));
    }
    let found = helper
        .store
        .file_refs_like(&room.id, &format!("sha256:{hex}"))?;
    let mut ids: Vec<&str> = found.iter().map(|r| r.file.id.as_str()).collect();
    ids.dedup();
    match ids.len() {
        0 => Err(Error::Invalid(format!(
            "no file {id} on any message in room {}",
            room.name
        ))),
        1 => Ok(found.into_iter().take(1).collect()),
        _ => Err(Error::Invalid(format!(
            "{id} matches more than one file; give more of it"
        ))),
    }
}

/// A free name in `dir`: `name`, else `name-1`, `name-2`, … before the
/// extension. A file already there with the same bytes is reused.
fn free_path(dir: &Path, name: &str, id: &str) -> Result<(PathBuf, bool)> {
    let (stem, ext) = match name.find('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    };
    for n in 0..1000 {
        let candidate = if n == 0 {
            dir.join(name)
        } else {
            dir.join(format!("{stem}-{n}{ext}"))
        };
        if !candidate.exists() {
            return Ok((candidate, false));
        }
        if files::hash_file(&candidate)
            .map(|(h, _)| h == id)
            .unwrap_or(false)
        {
            return Ok((candidate, true));
        }
    }
    Err(Error::Other(format!("no free name for {name}")))
}

/// Fetch one file from the room's home into `tmp`.
async fn fetch_remote(
    helper: &Helper,
    room: &Room,
    lm: &LocalMember,
    r: &FileRefRow,
    tmp: &Path,
) -> Result<()> {
    if !home_takes_files(helper, room) && helper.home_link(&room.id).await.is_some() {
        return Err(old_home(room));
    }
    let link = super::local::wait_for_home_link(helper, room)
        .await
        .ok_or_else(|| {
            Error::ReachedNobody(format!(
                "room {}'s home is not reachable; try again later",
                room.name
            ))
        })?;
    if !home_takes_files(helper, room) {
        return Err(old_home(room));
    }
    let id = helper.identity(&lm.identity).await?;
    let me = helper.net.node_id();
    let mut out = std::fs::File::create(tmp)?;
    let mut offset = 0u64;
    loop {
        let ts = helper.now_ts();
        let sig = diavlos_core::Signer::sign(
            id.as_ref(),
            &file_signing_bytes(
                "get",
                &room.id,
                &lm.member.name,
                &r.file.id,
                offset,
                "",
                &ts,
                &me,
            ),
        );
        let req = Wire::FileGet {
            room_id: room.id.clone(),
            name: lm.member.name.clone(),
            file: r.file.id.clone(),
            offset,
            ts,
            sig,
        };
        match link.request(&req).await?.into_result()? {
            Wire::FileChunk { data, size } => {
                if size != r.file.size {
                    return Err(Error::Invalid(format!(
                        "the home has {} at another size than the message says",
                        r.file.name
                    )));
                }
                let bytes = BASE64
                    .decode(data.as_bytes())
                    .map_err(|_| Error::Invalid("a file piece is not base64".into()))?;
                if bytes.is_empty() && offset < size {
                    return Err(Error::Invalid(format!("{} came back short", r.file.name)));
                }
                if offset + bytes.len() as u64 > size {
                    return Err(Error::Invalid(format!(
                        "{} came back too long",
                        r.file.name
                    )));
                }
                out.write_all(&bytes)?;
                offset += bytes.len() as u64;
                if offset == size {
                    out.sync_all()?;
                    return Ok(());
                }
            }
            other => return Err(Error::Invalid(format!("unexpected {}", other.label()))),
        }
    }
}

async fn get_one(
    helper: &Helper,
    room: &Room,
    lm: &LocalMember,
    r: &FileRefRow,
) -> Result<SavedFile> {
    let dir = helper.paths.files(&room.name);
    private_dir(&dir)?;
    let tmp = dir.join(format!(".{}.part", rand::random::<u64>()));
    let local = blob_path(helper, &room.id, &r.file.id);
    let fetched = if local.exists() {
        std::fs::copy(&local, &tmp).map(|_| ()).map_err(Error::from)
    } else if helper.is_home(room) {
        Err(Error::Invalid(format!(
            "the room's home no longer has {}",
            r.file.name
        )))
    } else {
        fetch_remote(helper, room, lm, r, &tmp).await
    };
    let checked = match fetched {
        Ok(()) => {
            let p = tmp.clone();
            blocking(move || {
                let (h, s) = files::hash_file(&p)?;
                Ok((h, s, files::sniff_file(&p)?))
            })
            .await
        }
        Err(e) => Err(e),
    };
    let kind = match checked {
        Ok((h, s, kind)) if h == r.file.id && s == r.file.size => kind,
        Ok(_) => {
            let _ = std::fs::remove_file(&tmp);
            return Err(Error::Invalid(format!(
                "{} did not match its fingerprint; it was thrown away",
                r.file.name
            )));
        }
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            return Err(e);
        }
    };
    if helper.is_home(room) {
        helper
            .store
            .file_fetched(&room.id, &r.file.id, &lm.member.name, &helper.now_ts())?;
    }
    let (path, already) = free_path(&dir, &files::saved_name(&r.file.name), &r.file.id)?;
    if already {
        std::fs::remove_file(&tmp)?;
    } else {
        std::fs::rename(&tmp, &path)?;
        private_file(&path)?;
    }
    let warnings = files::warnings(&r.file.name, &r.file.mime, kind);
    helper.emit(
        "file_saved",
        Some(&room.id),
        serde_json::json!({"id": r.file.id, "size": r.file.size, "kind": kind.as_str(), "warnings": warnings.len()}),
    );
    Ok(SavedFile {
        id: r.file.id.clone(),
        name: r.file.name.clone(),
        path: path.to_string_lossy().to_string(),
        size: r.file.size,
        kind: kind.as_str().into(),
        from: r.from.clone(),
        msg_id: r.msg_id.clone(),
        warnings,
    })
}

/// `get`: fetch files and save them under `files/<room>/`.
pub async fn get(helper: &Helper, room: &str, identity: &str, id: &str) -> Result<Vec<SavedFile>> {
    let room = helper.store.room(room)?;
    let lm = helper
        .store
        .local_member(&room.id, identity)?
        .ok_or_else(|| Error::NotInRoom(room.name.clone()))?;
    let mut out = Vec::new();
    for r in targets(helper, &room, id)? {
        out.push(get_one(helper, &room, &lm, &r).await?);
    }
    Ok(out)
}

// ---- cleaning up -------------------------------------------------------------

fn modified_before(path: &Path, secs: i64) -> bool {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .is_some_and(|age| age.as_secs() as i64 > secs)
}

/// Remove what a room no longer needs. On the home: files the policy says
/// are due, and pieces of uploads that never finished. Elsewhere: copies
/// waiting to be sent whose message is no longer queued.
pub fn sweep(helper: &Helper, room: &Room) -> Result<()> {
    let dir = helper.paths.blobs(&room.id);
    if helper.is_home(room) {
        let policy = helper.policy(room);
        let due = helper.store.file_sweep(
            &room.id,
            helper.now(),
            policy.file_keep_days,
            policy.file_max_days,
        )?;
        for id in &due {
            let _ = std::fs::remove_file(blob_path(helper, &room.id, id));
        }
        if !due.is_empty() {
            info!(room = %room.id, removed = due.len(), "files swept");
            helper.emit(
                "files_swept",
                Some(&room.id),
                serde_json::json!({"removed": due.len()}),
            );
        }
    }
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Ok(());
    };
    let mut queued: Vec<String> = Vec::new();
    if !helper.is_home(room) {
        for e in helper.store.outbox_entries(Some(&room.id))? {
            if let Some(m) = e.message {
                for f in files::refs(&m.data).unwrap_or_default() {
                    queued.push(f.hex().to_string());
                }
            }
        }
    }
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if !modified_before(&path, ORPHAN_FILE_SECS) {
            continue;
        }
        let keep = if helper.is_home(room) {
            name.len() == 64
                && helper
                    .store
                    .file_stored(&room.id, &format!("sha256:{name}"))?
                    .is_some()
        } else {
            queued.contains(&name)
        };
        if !keep {
            if let Err(e) = std::fs::remove_file(&path) {
                warn!(file = %path.display(), error = %e, "could not remove");
            }
        }
    }
    Ok(())
}
