//! Where things live: `~/.diavlos` unless `DIAVLOS_HOME` says otherwise.

use std::path::{Path, PathBuf};

use interprocess::local_socket::{
    GenericFilePath, GenericNamespaced, Name, NameType, ToFsName, ToNsName,
};

#[derive(Debug, Clone)]
pub struct Paths {
    pub home: PathBuf,
}

impl Paths {
    /// `--home`, else `$DIAVLOS_HOME`, else `~/.diavlos`.
    pub fn resolve(explicit: Option<PathBuf>) -> anyhow::Result<Paths> {
        if let Some(p) = explicit {
            return Ok(Paths { home: p });
        }
        if let Some(p) = std::env::var_os("DIAVLOS_HOME") {
            if !p.is_empty() {
                return Ok(Paths {
                    home: PathBuf::from(p),
                });
            }
        }
        let base = directories::BaseDirs::new()
            .ok_or_else(|| anyhow::anyhow!("cannot find your home directory"))?;
        Ok(Paths {
            home: base.home_dir().join(".diavlos"),
        })
    }

    /// Make the home directory, private to this user.
    pub fn ensure(&self) -> anyhow::Result<()> {
        std::fs::create_dir_all(&self.home)?;
        std::fs::create_dir_all(self.keys_dir())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&self.home, std::fs::Permissions::from_mode(0o700));
            let _ =
                std::fs::set_permissions(self.keys_dir(), std::fs::Permissions::from_mode(0o700));
        }
        Ok(())
    }

    pub fn config(&self) -> PathBuf {
        self.home.join("config.toml")
    }
    pub fn db(&self) -> PathBuf {
        self.home.join("diavlos.db")
    }
    pub fn keys_dir(&self) -> PathBuf {
        self.home.join("keys")
    }
    pub fn key(&self, identity: &str) -> PathBuf {
        self.keys_dir().join(format!("{identity}.json"))
    }
    pub fn node_key(&self) -> PathBuf {
        self.home.join("node.key")
    }
    pub fn log(&self) -> PathBuf {
        self.home.join("helper.log")
    }
    pub fn socket_file(&self) -> PathBuf {
        self.home.join("helper.sock")
    }
    pub fn policy(&self, room: &str) -> PathBuf {
        self.home.join("rooms").join(room).join("policy.toml")
    }

    /// The local socket name. A file under the home dir on Unix (so it can
    /// be 0600); a namespaced pipe on Windows.
    pub fn socket_name(&self) -> std::io::Result<Name<'static>> {
        if cfg!(windows) && GenericNamespaced::is_supported() {
            let tag = short_hash(&self.home);
            format!("diavlos-{tag}").to_ns_name::<GenericNamespaced>()
        } else {
            #[cfg(unix)]
            if let Some(problem) = self.socket_path_too_long() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    problem.to_string(),
                ));
            }
            self.socket_file().to_fs_name::<GenericFilePath>()
        }
    }

    /// Whether the socket path is too long for the kernel to accept, and by
    /// how much.
    ///
    /// The path to a Unix socket lives in `sun_path`, a fixed-size array
    /// inside `sockaddr_un`: 108 bytes on Linux, 104 on macOS and the BSDs,
    /// including the NUL the kernel wants at the end. Nothing can raise it.
    /// Go over and `bind()` fails, but what comes back says nothing useful:
    /// the OS calls it "invalid argument", and the socket crate calls it
    /// "local socket name length exceeds capacity of sun_path of
    /// sockaddr_un". Neither tells a person the one thing that matters,
    /// which is that their home directory is too deep.
    ///
    /// A normal home (`/home/you/.diavlos`, `/Users/you/.diavlos`) is
    /// nowhere near the limit. `--home` and `$DIAVLOS_HOME` take any path
    /// at all, though, so this is reachable.
    #[cfg(unix)]
    pub fn socket_path_too_long(&self) -> Option<SocketPathTooLong> {
        use std::os::unix::ffi::OsStrExt;
        let len = self.socket_file().as_os_str().as_bytes().len();
        let max = SUN_PATH_CAPACITY - 1; // the kernel needs the last byte
        (len > max).then(|| SocketPathTooLong {
            len,
            max,
            home: self.home.clone(),
        })
    }
}

/// `sun_path` is this many bytes, the trailing NUL included.
#[cfg(unix)]
const SUN_PATH_CAPACITY: usize = if cfg!(any(target_os = "linux", target_os = "android")) {
    108
} else {
    104
};

/// The home directory is too deep for a Unix socket to live in it.
///
/// Carries the numbers so the message can name them, rather than telling
/// someone their path is "too long" and leaving them to guess by how much.
#[cfg(unix)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SocketPathTooLong {
    /// How many bytes the socket path actually is.
    pub len: usize,
    /// The most it may be on this platform.
    pub max: usize,
    /// The home directory it was built from.
    pub home: PathBuf,
}

#[cfg(unix)]
impl std::fmt::Display for SocketPathTooLong {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "the helper's socket path is {} bytes and this system allows at \
             most {}. That limit is in the kernel; nothing can raise it. \
             Move the Diavlos home somewhere shorter, with DIAVLOS_HOME or \
             --home: about {} fewer bytes would do it. It is {} now.",
            self.len,
            self.max,
            self.len - self.max,
            self.home.display()
        )
    }
}

#[cfg(unix)]
impl std::error::Error for SocketPathTooLong {}

fn short_hash(p: &Path) -> String {
    let h = diavlos_core::canonical::sha256_bytes(p.to_string_lossy().as_bytes());
    h.trim_start_matches("sha256:")[..12].to_string()
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn home_of_len(n: usize) -> Paths {
        // A home directory exactly `n` bytes long: "/" plus n-1 padding.
        let pad = n.saturating_sub(1);
        Paths {
            home: PathBuf::from(format!("/{}", "x".repeat(pad))),
        }
    }

    #[test]
    fn a_normal_home_is_nowhere_near_the_limit() {
        for home in [
            "/home/you/.diavlos",
            "/Users/you/.diavlos",
            "/root/.diavlos",
        ] {
            let p = Paths {
                home: PathBuf::from(home),
            };
            assert_eq!(p.socket_path_too_long(), None, "{home} should be fine");
            assert!(p.socket_name().is_ok());
        }
    }

    #[test]
    fn the_limit_is_where_sun_path_ends() {
        let max = SUN_PATH_CAPACITY - 1;
        // "<home>/helper.sock" is the home plus 12 bytes.
        let suffix = "/helper.sock".len();

        let ok = home_of_len(max - suffix);
        assert_eq!(ok.socket_file().as_os_str().len(), max);
        assert_eq!(
            ok.socket_path_too_long(),
            None,
            "exactly at the limit is ok"
        );

        let over = home_of_len(max - suffix + 1);
        assert_eq!(over.socket_file().as_os_str().len(), max + 1);
        let problem = over.socket_path_too_long().expect("one over must fail");
        assert_eq!(problem.len, max + 1);
        assert_eq!(problem.max, max);
    }

    #[test]
    fn the_message_says_what_to_do_about_it() {
        let p = home_of_len(400);
        let msg = p.socket_path_too_long().unwrap().to_string();
        // The old error said "length exceeds capacity of sun_path of
        // sockaddr_un", which told nobody anything. This one has to name
        // the fix and the numbers.
        assert!(msg.contains("DIAVLOS_HOME"), "{msg}");
        assert!(msg.contains("--home"), "{msg}");
        assert!(msg.contains(&(SUN_PATH_CAPACITY - 1).to_string()), "{msg}");
        assert!(!msg.contains("sockaddr_un"), "{msg}");
    }

    #[test]
    fn socket_name_refuses_rather_than_handing_back_a_doomed_name() {
        let e = home_of_len(400).socket_name().unwrap_err();
        assert_eq!(e.kind(), std::io::ErrorKind::InvalidInput);
        assert!(e.to_string().contains("DIAVLOS_HOME"), "{e}");
    }

    /// The numbers above are only worth anything if the kernel agrees with
    /// them, so bind a real socket exactly at the limit and one byte over.
    /// If this ever fails, `SUN_PATH_CAPACITY` is wrong for this platform
    /// and the check is lying to people.
    #[test]
    fn the_kernel_really_does_draw_the_line_there() {
        use std::os::unix::net::UnixListener;

        let base = std::env::temp_dir().join(format!("dv-sun-{}", std::process::id()));
        std::fs::create_dir_all(&base).unwrap();
        let _cleanup = Cleanup(base.clone());

        let base = base.to_str().unwrap();
        let max = SUN_PATH_CAPACITY - 1;
        if base.len() + 2 > max {
            return; // the temp dir is itself too deep; nothing to prove here
        }

        let at_limit = format!("{base}/{}", "x".repeat(max - base.len() - 1));
        assert_eq!(at_limit.len(), max);
        UnixListener::bind(&at_limit).expect("a path exactly at the limit must bind");

        let one_over = format!("{at_limit}x");
        assert_eq!(one_over.len(), max + 1);
        assert!(
            UnixListener::bind(&one_over).is_err(),
            "one byte over the limit must not bind"
        );
    }

    struct Cleanup(PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}
