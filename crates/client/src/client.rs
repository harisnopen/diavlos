//! Talk to the local helper. Start it if it is not running.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use diavlos_core::{Error, Result};
use interprocess::local_socket::tokio::{prelude::*, Stream};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::paths::Paths;
use crate::proto::{Request, Response};

/// A stream of JSON lines from a streaming request (events, watch).
pub struct LineStream {
    reader: BufReader<Stream>,
}

impl LineStream {
    /// The next line, or `None` when the helper closed the stream.
    pub async fn next(&mut self) -> Result<Option<Value>> {
        let mut buf = String::new();
        let n = self.reader.read_line(&mut buf).await?;
        if n == 0 {
            return Ok(None);
        }
        let resp: Response = serde_json::from_str(buf.trim_end())?;
        if resp.ok {
            Ok(Some(resp.result))
        } else {
            Err(Error::from_code(
                resp.code.unwrap_or(1),
                resp.error.as_deref().unwrap_or("unknown error"),
            ))
        }
    }
}

/// How long to wait for a freshly started helper to answer.
const START_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone)]
pub struct Client {
    paths: Paths,
}

impl Client {
    pub fn new(paths: Paths) -> Self {
        Client { paths }
    }

    async fn connect_once(&self) -> std::io::Result<Stream> {
        let name = self.paths.socket_name()?;
        Stream::connect(name).await
    }

    /// Connect, starting the helper first if nothing answers.
    pub async fn connect(&self) -> Result<Stream> {
        // Waiting is for a helper that has not finished starting. A socket
        // path the kernel will never accept is not going to fix itself, so
        // say so now rather than after the timeout.
        #[cfg(unix)]
        if let Some(problem) = self.paths.socket_path_too_long() {
            return Err(Error::Other(problem.to_string()));
        }
        if let Ok(s) = self.connect_once().await {
            return Ok(s);
        }
        self.spawn_helper()?;
        let deadline = Instant::now() + START_TIMEOUT;
        loop {
            tokio::time::sleep(Duration::from_millis(100)).await;
            match self.connect_once().await {
                Ok(s) => return Ok(s),
                Err(e) if Instant::now() > deadline => {
                    return Err(Error::Other(format!(
                        "helper did not start ({e}). See {}",
                        self.paths.log().display()
                    )))
                }
                Err(_) => continue,
            }
        }
    }

    /// Is a helper answering right now? Does not start one.
    pub async fn is_running(&self) -> bool {
        self.connect_once().await.is_ok()
    }

    /// The `diavlos` binary: this process if it is one, else `DIAVLOS_BIN`,
    /// else `diavlos` on the PATH.
    fn helper_binary() -> Result<PathBuf> {
        if let Ok(exe) = std::env::current_exe() {
            if exe
                .file_stem()
                .map(|s| s.to_string_lossy().starts_with("diavlos"))
                .unwrap_or(false)
            {
                return Ok(exe);
            }
        }
        if let Some(p) = std::env::var_os("DIAVLOS_BIN") {
            return Ok(PathBuf::from(p));
        }
        let name = if cfg!(windows) {
            "diavlos.exe"
        } else {
            "diavlos"
        };
        if let Some(path) = std::env::var_os("PATH") {
            for dir in std::env::split_paths(&path) {
                let candidate = dir.join(name);
                if candidate.is_file() {
                    return Ok(candidate);
                }
            }
        }
        Err(Error::Other(
            "cannot find the diavlos binary; set DIAVLOS_BIN or put diavlos on your PATH".into(),
        ))
    }

    fn spawn_helper(&self) -> Result<()> {
        let exe = Self::helper_binary()?;
        let mut cmd = std::process::Command::new(exe);
        cmd.arg("--home")
            .arg(&self.paths.home)
            .arg("helper")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            cmd.process_group(0);
        }
        #[cfg(windows)]
        {
            use std::os::windows::io::AsRawHandle;
            use std::os::windows::process::CommandExt;
            use windows_sys::Win32::Foundation::{
                SetHandleInformation, HANDLE, HANDLE_FLAG_INHERIT,
            };
            // Windows children inherit every inheritable handle, not just
            // the stdio we hand them. If whoever ran us is reading our
            // output through a pipe, the long-lived helper would keep that
            // pipe open and the reader would wait until the helper exits.
            // So make our own stdio non-inheritable before spawning.
            for h in [
                std::io::stdin().as_raw_handle(),
                std::io::stdout().as_raw_handle(),
                std::io::stderr().as_raw_handle(),
            ] {
                if !h.is_null() {
                    // SAFETY: a plain Win32 call on a handle this process owns;
                    // failure (an invalid handle) is harmless and ignored.
                    unsafe {
                        SetHandleInformation(h as HANDLE, HANDLE_FLAG_INHERIT, 0);
                    }
                }
            }
            const DETACHED_PROCESS: u32 = 0x0000_0008;
            const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
            cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
        }
        cmd.spawn()
            .map_err(|e| Error::Other(format!("could not start the helper: {e}")))?;
        Ok(())
    }

    /// One request, one response.
    pub async fn call(&self, req: &Request) -> Result<Value> {
        let stream = self.connect().await?;
        Self::call_on(stream, req).await
    }

    /// Like `call`, but never starts a helper.
    pub async fn call_if_running(&self, req: &Request) -> Result<Option<Value>> {
        match self.connect_once().await {
            Ok(stream) => Ok(Some(Self::call_on(stream, req).await?)),
            Err(_) => Ok(None),
        }
    }

    /// Open a streaming request. Lines keep coming until the helper or the
    /// caller closes the connection.
    pub async fn stream(&self, req: &Request) -> Result<LineStream> {
        let stream = self.connect().await?;
        let mut line = serde_json::to_string(req)?;
        line.push('\n');
        let mut writer = &stream;
        writer.write_all(line.as_bytes()).await?;
        writer.flush().await?;
        Ok(LineStream {
            reader: BufReader::new(stream),
        })
    }

    async fn call_on(stream: Stream, req: &Request) -> Result<Value> {
        let mut line = serde_json::to_string(req)?;
        line.push('\n');
        let mut reader = BufReader::new(&stream);
        let mut writer = &stream;
        writer.write_all(line.as_bytes()).await?;
        writer.flush().await?;
        let mut buf = String::new();
        let n = reader.read_line(&mut buf).await?;
        if n == 0 {
            return Err(Error::Other("helper closed the connection".into()));
        }
        let resp: Response = serde_json::from_str(buf.trim_end())?;
        if resp.ok {
            Ok(resp.result)
        } else {
            Err(Error::from_code(
                resp.code.unwrap_or(1),
                resp.error.as_deref().unwrap_or("unknown error"),
            ))
        }
    }
}
