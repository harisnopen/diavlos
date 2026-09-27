//! Files on a message: the reference a message carries, and the checks
//! that never trust the sender. See docs/FILES.md.
//!
//! A message points at a file with a small reference in `data.files`:
//! fingerprint, cleaned name, size and the type the sender says it is. The
//! bytes travel separately, helper to helper, and are checked against the
//! fingerprint at each end. Nothing here opens or runs a file; it only
//! looks at bytes.

use std::io::Read;
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::error::{Error, Result};

/// No room may let a single file be bigger than this.
pub const HARD_MAX_FILE_BYTES: u64 = 1024 * 1024 * 1024;
/// No message may carry more files than this, whatever the policy says.
pub const HARD_MAX_FILES: usize = 20;
/// Longest cleaned file name, in bytes.
pub const MAX_NAME_BYTES: usize = 100;

/// What a message carries about one file. Nothing else about the file is
/// in the message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileRef {
    /// `sha256:<hex>` of the bytes. Also the file's id.
    pub id: String,
    /// The cleaned name. Never a path.
    pub name: String,
    pub size: u64,
    /// The type the sender says it is. Advice only; the reader checks the
    /// bytes.
    #[serde(rename = "type", default)]
    pub mime: String,
}

impl FileRef {
    /// The hex part of the id: the name of the stored bytes on disk.
    pub fn hex(&self) -> &str {
        self.id.strip_prefix("sha256:").unwrap_or(&self.id)
    }

    fn check(&self) -> Result<()> {
        check_id(&self.id)?;
        if clean_name(&self.name) != self.name {
            return Err(Error::Invalid(format!(
                "file name {:?} is not a clean name",
                self.name
            )));
        }
        if self.size > HARD_MAX_FILE_BYTES {
            return Err(Error::Invalid(format!(
                "file {} is {} bytes; no file may be over {HARD_MAX_FILE_BYTES}",
                self.name, self.size
            )));
        }
        if self.mime.len() > 100 || self.mime.chars().any(|c| c.is_control()) {
            return Err(Error::Invalid("file type is not a plain type name".into()));
        }
        Ok(())
    }
}

/// `sha256:` and 64 lowercase hex digits.
pub fn check_id(id: &str) -> Result<()> {
    let ok = id.strip_prefix("sha256:").is_some_and(|h| {
        h.len() == 64 && h.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    });
    if ok {
        Ok(())
    } else {
        Err(Error::Invalid(format!("{id:?} is not a file id")))
    }
}

/// The file references on a message, checked. None is an empty list. A
/// `files` key that is there but not a list of good references is an
/// error, so nothing half-formed goes into the chain.
pub fn refs(data: &Value) -> Result<Vec<FileRef>> {
    let Some(files) = data.get("files") else {
        return Ok(Vec::new());
    };
    let refs: Vec<FileRef> = serde_json::from_value(files.clone())
        .map_err(|e| Error::Invalid(format!("data.files is not a list of files: {e}")))?;
    if refs.len() > HARD_MAX_FILES {
        return Err(Error::Invalid(format!(
            "a message may carry at most {HARD_MAX_FILES} files"
        )));
    }
    for r in &refs {
        r.check()?;
    }
    Ok(refs)
}

/// Fingerprint and size of a file on disk, read in pieces.
pub fn hash_file(path: &Path) -> Result<(String, u64)> {
    let mut f = std::fs::File::open(path)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 256 * 1024];
    let mut size = 0u64;
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
        size += n as u64;
    }
    Ok((
        format!("sha256:{}", data_encoding::HEXLOWER.encode(&h.finalize())),
        size,
    ))
}

// ---- names -----------------------------------------------------------------

const RESERVED: &[&str] = &[
    "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8",
    "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
];

/// A name that is safe to save under on any system: the last part of the
/// path only, letters, digits, `.`, `_` and `-`, no leading dot, not a
/// reserved Windows name, at most [`MAX_NAME_BYTES`]. Never empty.
pub fn clean_name(raw: &str) -> String {
    let base = raw.rsplit(['/', '\\']).next().unwrap_or("");
    let mut s: String = base
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect();
    while s.contains("..") {
        s = s.replace("..", ".");
    }
    let s = s.trim_matches(|c| c == '.' || c == ' ');
    let mut s = if s.is_empty() {
        "file".to_string()
    } else {
        s.to_string()
    };
    if s.len() > MAX_NAME_BYTES {
        // Keep the extension; cut the stem.
        let ext = s
            .rfind('.')
            .map(|i| s[i..].to_string())
            .filter(|e| e.len() <= 16)
            .unwrap_or_default();
        let stem_len = MAX_NAME_BYTES - ext.len();
        s = format!("{}{ext}", &s[..stem_len]);
    }
    let stem = s.split('.').next().unwrap_or("").to_ascii_lowercase();
    if RESERVED.contains(&stem.as_str()) {
        s = format!("_{s}");
        s.truncate(MAX_NAME_BYTES);
    }
    s
}

fn ext_of(name: &str) -> String {
    match name.rfind('.') {
        Some(i) if i > 0 => name[i + 1..].to_ascii_lowercase(),
        _ => String::new(),
    }
}

/// Endings that run on a double click or from a shell.
const RUNNABLE: &[&str] = &[
    "exe", "com", "bat", "cmd", "ps1", "psm1", "vbs", "vbe", "js", "jse", "wsf", "wsh", "hta",
    "msi", "msp", "scr", "pif", "cpl", "jar", "sh", "bash", "zsh", "command", "app", "dmg", "pkg",
    "run", "appimage", "deb", "rpm", "lnk", "reg", "desktop",
];

/// Would a file with this name run when someone double-clicks it?
pub fn runnable_name(name: &str) -> bool {
    RUNNABLE.contains(&ext_of(name).as_str())
}

/// The name to save under: the cleaned name, with `.unsafe` added when the
/// name would run on a double click.
pub fn saved_name(name: &str) -> String {
    let clean = clean_name(name);
    if runnable_name(&clean) {
        format!("{clean}.unsafe")
    } else {
        clean
    }
}

/// The type a name suggests. `application/octet-stream` when unknown.
pub fn type_for_name(name: &str) -> &'static str {
    match ext_of(name).as_str() {
        "txt" | "log" | "md" | "csv" | "tsv" | "json" | "jsonl" | "yaml" | "yml" | "toml"
        | "ini" | "xml" | "html" | "htm" | "css" | "rs" | "py" | "go" | "ts" | "java" | "c"
        | "h" | "cpp" | "rb" | "sql" | "diff" | "patch" => "text/plain",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "pdf" => "application/pdf",
        "zip" => "application/zip",
        "gz" | "tgz" => "application/gzip",
        _ => "application/octet-stream",
    }
}

// ---- what the bytes are ----------------------------------------------------

/// What the bytes of a file look like, from the bytes alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Text,
    Png,
    Jpeg,
    Gif,
    Webp,
    Pdf,
    Zip,
    Gzip,
    /// A Windows, Linux or macOS program, or a script with `#!`.
    Program,
    Other,
}

impl Kind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Kind::Text => "text",
            Kind::Png => "png",
            Kind::Jpeg => "jpeg",
            Kind::Gif => "gif",
            Kind::Webp => "webp",
            Kind::Pdf => "pdf",
            Kind::Zip => "zip",
            Kind::Gzip => "gzip",
            Kind::Program => "program",
            Kind::Other => "other",
        }
    }

    pub fn mime(&self) -> &'static str {
        match self {
            Kind::Text => "text/plain",
            Kind::Png => "image/png",
            Kind::Jpeg => "image/jpeg",
            Kind::Gif => "image/gif",
            Kind::Webp => "image/webp",
            Kind::Pdf => "application/pdf",
            Kind::Zip => "application/zip",
            Kind::Gzip => "application/gzip",
            Kind::Program | Kind::Other => "application/octet-stream",
        }
    }

    /// On the safe list a room can switch to.
    pub fn is_safe(&self) -> bool {
        matches!(
            self,
            Kind::Text | Kind::Png | Kind::Jpeg | Kind::Gif | Kind::Webp | Kind::Pdf | Kind::Zip
        )
    }
}

fn magic(head: &[u8]) -> Option<Kind> {
    let starts = |m: &[u8]| head.starts_with(m);
    if starts(b"\x89PNG\r\n\x1a\n") {
        Some(Kind::Png)
    } else if starts(b"\xff\xd8\xff") {
        Some(Kind::Jpeg)
    } else if starts(b"GIF87a") || starts(b"GIF89a") {
        Some(Kind::Gif)
    } else if head.len() >= 12 && &head[..4] == b"RIFF" && &head[8..12] == b"WEBP" {
        Some(Kind::Webp)
    } else if starts(b"%PDF-") {
        Some(Kind::Pdf)
    } else if starts(b"PK\x03\x04") || starts(b"PK\x05\x06") {
        Some(Kind::Zip)
    } else if starts(b"\x1f\x8b") {
        Some(Kind::Gzip)
    } else if starts(b"\x7fELF")
        || starts(b"MZ")
        || starts(b"#!")
        || starts(b"\xfe\xed\xfa\xce")
        || starts(b"\xfe\xed\xfa\xcf")
        || starts(b"\xce\xfa\xed\xfe")
        || starts(b"\xcf\xfa\xed\xfe")
        || starts(b"\xca\xfe\xba\xbe")
    {
        Some(Kind::Program)
    } else {
        None
    }
}

/// What a file on disk is, from its bytes. Text means valid UTF-8 with no
/// NUL byte anywhere in it, so the whole file is read.
pub fn sniff_file(path: &Path) -> Result<Kind> {
    let mut f = std::fs::File::open(path)?;
    let mut buf = vec![0u8; 256 * 1024];
    let mut carry: Vec<u8> = Vec::new();
    let mut first = true;
    let mut text = true;
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        if first {
            first = false;
            if let Some(k) = magic(&buf[..n]) {
                return Ok(k);
            }
        }
        if !text {
            continue;
        }
        let mut chunk = std::mem::take(&mut carry);
        chunk.extend_from_slice(&buf[..n]);
        if chunk.contains(&0) {
            text = false;
            continue;
        }
        match std::str::from_utf8(&chunk) {
            Ok(_) => {}
            // Cut in the middle of a character: keep the tail for next time.
            Err(e) if e.error_len().is_none() => carry = chunk[e.valid_up_to()..].to_vec(),
            Err(_) => text = false,
        }
    }
    Ok(if text && carry.is_empty() {
        Kind::Text
    } else {
        Kind::Other
    })
}

/// What the reader is told about a file before anyone opens it. Built from
/// the bytes and the name, never from what the sender says alone.
pub fn warnings(name: &str, said: &str, kind: Kind) -> Vec<String> {
    let mut out = Vec::new();
    if kind == Kind::Program || runnable_name(name) {
        out.push(
            "this file can run as a program; do not run it unless you trust who sent it".into(),
        );
    }
    let by_name = type_for_name(name);
    for (what, claimed) in [("its name", by_name), ("the sender", said)] {
        if claimed.is_empty() || claimed == "application/octet-stream" {
            continue;
        }
        let family = |m: &str| m.split('/').next().unwrap_or("").to_string();
        let fits = match kind {
            Kind::Text => family(claimed) == "text" || claimed.ends_with("json"),
            Kind::Other | Kind::Program => false,
            k => k.mime() == claimed,
        };
        if !fits {
            out.push(format!(
                "the bytes look like {}, not {claimed} as {what} says",
                kind.as_str()
            ));
        }
    }
    out.dedup();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(bytes: &[u8]) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!("diavlos-files-{}", ulid::Ulid::generate()));
        std::fs::write(&p, bytes).unwrap();
        p
    }

    #[test]
    fn names_are_cleaned_for_any_system() {
        assert_eq!(clean_name("build.log"), "build.log");
        assert_eq!(clean_name("../../etc/passwd"), "passwd");
        assert_eq!(clean_name("C:\\Users\\x\\evil.exe"), "evil.exe");
        assert_eq!(clean_name(".bashrc"), "bashrc");
        assert_eq!(clean_name("a..b"), "a.b");
        assert_eq!(clean_name("my report (v2).pdf"), "my_report__v2_.pdf");
        assert_eq!(clean_name(""), "file");
        assert_eq!(clean_name(".."), "file");
        assert_eq!(clean_name("dir/"), "file");
        assert_eq!(clean_name("CON.txt"), "_CON.txt");
        assert_eq!(clean_name("nul"), "_nul");
        let long = format!("{}.txt", "a".repeat(300));
        let c = clean_name(&long);
        assert!(c.len() <= MAX_NAME_BYTES && c.ends_with(".txt"), "{c}");
        // Cleaning a clean name changes nothing.
        for n in ["a.txt", "_CON.txt", "file", &c] {
            assert_eq!(clean_name(n), n);
        }
    }

    #[test]
    fn runnable_names_are_saved_so_they_do_not_run() {
        assert_eq!(saved_name("setup.exe"), "setup.exe.unsafe");
        assert_eq!(saved_name("x.SH"), "x.SH.unsafe");
        assert_eq!(saved_name("notes.txt"), "notes.txt");
        assert!(!runnable_name("exe"));
    }

    #[test]
    fn the_bytes_decide_the_kind() {
        let cases: &[(&[u8], Kind)] = &[
            (b"hello\nworld", Kind::Text),
            ("καλημέρα".as_bytes(), Kind::Text),
            (b"\x89PNG\r\n\x1a\nrest", Kind::Png),
            (b"%PDF-1.7", Kind::Pdf),
            (b"PK\x03\x04zip", Kind::Zip),
            (b"\x7fELF\x02", Kind::Program),
            (b"MZ\x90\x00", Kind::Program),
            (b"#!/bin/sh\nrm -rf /", Kind::Program),
            (b"text\0with nul", Kind::Other),
            (b"\xff\xfe bad utf8", Kind::Other),
            (b"", Kind::Text),
        ];
        for (bytes, want) in cases {
            let p = tmp(bytes);
            assert_eq!(sniff_file(&p).unwrap(), *want, "{bytes:?}");
            std::fs::remove_file(p).unwrap();
        }
        assert!(Kind::Pdf.is_safe() && !Kind::Program.is_safe() && !Kind::Gzip.is_safe());
    }

    #[test]
    fn a_character_split_across_reads_is_still_text() {
        // 256 KiB of 'a' then a two-byte character straddling the boundary.
        let mut bytes = vec![b'a'; 256 * 1024 - 1];
        bytes.extend_from_slice("é and more".as_bytes());
        let p = tmp(&bytes);
        assert_eq!(sniff_file(&p).unwrap(), Kind::Text);
        std::fs::remove_file(p).unwrap();
    }

    #[test]
    fn warnings_come_from_the_bytes() {
        assert!(warnings("notes.txt", "text/plain", Kind::Text).is_empty());
        assert!(warnings("data.json", "application/json", Kind::Text).is_empty());
        assert!(warnings("blob", "", Kind::Other).is_empty());
        let w = warnings("photo.png", "image/png", Kind::Program);
        assert!(w.iter().any(|w| w.contains("run as a program")), "{w:?}");
        assert!(w.iter().any(|w| w.contains("not image/png")), "{w:?}");
        let w = warnings("tool.exe", "", Kind::Other);
        assert!(w.iter().any(|w| w.contains("run as a program")), "{w:?}");
        let w = warnings("a.bin", "image/png", Kind::Pdf);
        assert_eq!(w.len(), 1, "{w:?}");
    }

    #[test]
    fn hashing_and_references() {
        let p = tmp(b"test");
        let (id, size) = hash_file(&p).unwrap();
        assert_eq!(
            id,
            "sha256:9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08"
        );
        assert_eq!(size, 4);
        std::fs::remove_file(p).unwrap();

        let good = serde_json::json!({"files": [{"id": id, "name": "t.txt", "size": 4, "type": "text/plain"}]});
        assert_eq!(refs(&good).unwrap()[0].hex().len(), 64);
        assert!(refs(&serde_json::json!({})).unwrap().is_empty());
        assert!(refs(&Value::Null).unwrap().is_empty());
        for bad in [
            serde_json::json!({"files": "x"}),
            serde_json::json!({"files": [{"id": "sha256:zz", "name": "a", "size": 1}]}),
            serde_json::json!({"files": [{"id": id, "name": "../a", "size": 1}]}),
            serde_json::json!({"files": [{"id": id, "name": "a", "size": HARD_MAX_FILE_BYTES + 1}]}),
        ] {
            assert!(refs(&bad).is_err(), "{bad}");
        }
    }
}
