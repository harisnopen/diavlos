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
            self.socket_file().to_fs_name::<GenericFilePath>()
        }
    }
}

fn short_hash(p: &Path) -> String {
    let h = diavlos_core::canonical::sha256_bytes(p.to_string_lossy().as_bytes());
    h.trim_start_matches("sha256:")[..12].to_string()
}
