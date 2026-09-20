//! Talk to the local helper. Start it if it is not running.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use diavlos_core::{Error, Result};
use interprocess::local_socket::tokio::{prelude::*, Stream};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::paths::Paths;
use crate::proto::{Request, Response};

/// How long to wait for a freshly started helper to answer.
const START_TIMEOUT: Duration = Duration::from_secs(10);

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

    fn spawn_helper(&self) -> Result<()> {
        let exe: PathBuf = std::env::current_exe()?;
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
            use std::os::windows::process::CommandExt;
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
