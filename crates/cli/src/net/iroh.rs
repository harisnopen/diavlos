//! The iroh transport: direct peer link when possible, relay when the
//! network won't allow it, encrypted end to end either way (QUIC + TLS).
//! The relay only sees encrypted bytes.

use std::net::{Ipv4Addr, SocketAddr};
use std::path::Path;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use diavlos_core::{Error, Result};
use iroh::endpoint::{presets, Connection, SendStream};
use iroh::protocol::{AcceptError, ProtocolHandler, Router};
use iroh::{Endpoint, EndpointAddr, EndpointId, RelayMode, RelayUrl, SecretKey, Watcher};
use serde_json::Value;
use tokio::sync::{mpsc, Mutex};

use super::{read_frame, write_frame, Link, Reply, Transport, Wire, ALPN};

/// How long a dial may take before we say "reached nobody".
const DIAL_TIMEOUT: Duration = Duration::from_secs(20);
/// How long one request may take end to end.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

pub struct IrohTransport {
    endpoint: Endpoint,
    router: Router,
    incoming: Mutex<mpsc::Receiver<Arc<dyn Link>>>,
}

impl IrohTransport {
    /// Bind the endpoint. `port` 0 means pick one.
    pub async fn bind(
        secret: SecretKey,
        port: u16,
        public_relays: bool,
        relay_urls: &[String],
    ) -> anyhow::Result<Self> {
        let mut builder = if public_relays {
            Endpoint::builder(presets::N0)
        } else {
            Endpoint::builder(presets::Minimal)
        };
        if !relay_urls.is_empty() {
            let mut urls = Vec::new();
            for u in relay_urls {
                urls.push(
                    RelayUrl::from_str(u).map_err(|e| anyhow::anyhow!("relay url {u}: {e}"))?,
                );
            }
            builder = builder.relay_mode(RelayMode::custom(urls));
        } else if !public_relays {
            builder = builder.relay_mode(RelayMode::Disabled);
        }
        builder = builder.secret_key(secret).proxy_from_env();
        if port != 0 {
            // Pin the IPv4 port so direct addresses stay stable across
            // restarts. IPv6 keeps iroh's default bind, which may fail on
            // hosts without IPv6.
            builder = builder
                .bind_addr(SocketAddr::new(Ipv4Addr::UNSPECIFIED.into(), port))
                .map_err(|e| anyhow::anyhow!("bind port {port}: {e}"))?;
        }
        let endpoint = builder.bind().await?;
        let (tx, rx) = mpsc::channel(64);
        let router = Router::builder(endpoint.clone())
            .accept(ALPN, Handler { tx })
            .spawn();
        // Give the endpoint a moment to learn its own addresses so the
        // first invite carries something useful.
        for _ in 0..30 {
            if endpoint.addr().ip_addrs().next().is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        Ok(IrohTransport {
            endpoint,
            router,
            incoming: Mutex::new(rx),
        })
    }

    /// The UDP port this endpoint bound.
    pub fn bound_port(&self) -> Option<u16> {
        self.endpoint.bound_sockets().first().map(|a| a.port())
    }

    /// Load the node key, or make one.
    pub fn load_or_create_key(path: &Path) -> anyhow::Result<SecretKey> {
        if path.exists() {
            let text = std::fs::read_to_string(path)?;
            let key = SecretKey::from_str(text.trim())
                .map_err(|e| anyhow::anyhow!("node key {}: {e}", path.display()))?;
            return Ok(key);
        }
        let key = SecretKey::generate();
        let hex = data_encoding::HEXLOWER.encode(&key.to_bytes());
        diavlos_core::keys::write_private(path, hex.as_bytes())?;
        Ok(key)
    }
}

#[derive(Debug, Clone)]
struct Handler {
    tx: mpsc::Sender<Arc<dyn Link>>,
}

impl ProtocolHandler for Handler {
    async fn accept(&self, connection: Connection) -> std::result::Result<(), AcceptError> {
        let link: Arc<dyn Link> = Arc::new(IrohLink {
            conn: connection.clone(),
        });
        if self.tx.send(link).await.is_err() {
            connection.close(1u32.into(), b"shutting down");
            return Ok(());
        }
        // Keep this task alive until the connection ends.
        connection.closed().await;
        Ok(())
    }
}

#[async_trait]
impl Transport for IrohTransport {
    fn node_id(&self) -> String {
        self.endpoint.id().to_string()
    }

    fn hints(&self) -> Value {
        serde_json::to_value(self.endpoint.addr()).unwrap_or(Value::Null)
    }

    async fn dial(&self, node: &str, hints: &Value) -> Result<Arc<dyn Link>> {
        let id = EndpointId::from_str(node)
            .map_err(|e| Error::Invalid(format!("bad node id {node}: {e}")))?;
        let addr = match serde_json::from_value::<EndpointAddr>(hints.clone()) {
            Ok(a) if a.id == id => a,
            _ => EndpointAddr::new(id),
        };
        let conn = tokio::time::timeout(DIAL_TIMEOUT, self.endpoint.connect(addr, ALPN))
            .await
            .map_err(|_| Error::ReachedNobody(format!("no answer from {} in time", short(node))))?
            .map_err(|e| Error::ReachedNobody(format!("cannot reach {}: {e}", short(node))))?;
        Ok(Arc::new(IrohLink { conn }))
    }

    async fn accept(&self) -> Option<Arc<dyn Link>> {
        self.incoming.lock().await.recv().await
    }

    fn network_info(&self) -> Value {
        let relays: Vec<Value> = self
            .endpoint
            .home_relay_status()
            .get()
            .iter()
            .map(|r| {
                serde_json::json!({
                    "url": r.url().to_string(),
                    "connected": r.is_connected(),
                    "last_error": r.last_error().map(|e| e.to_string()),
                })
            })
            .collect();
        serde_json::json!({
            "node": self.endpoint.id().to_string(),
            "addr": self.endpoint.addr(),
            "relays": relays,
            "bound": self.endpoint.bound_sockets().iter().map(|a| a.to_string()).collect::<Vec<_>>(),
        })
    }

    async fn shutdown(&self) {
        let _ = self.router.shutdown().await;
        self.endpoint.close().await;
    }
}

fn short(node: &str) -> String {
    node.chars().take(10).collect()
}

struct IrohLink {
    conn: Connection,
}

#[async_trait]
impl Link for IrohLink {
    fn remote_node(&self) -> String {
        self.conn.remote_id().to_string()
    }

    async fn request(&self, req: &Wire) -> Result<Wire> {
        let fut = async {
            let (mut send, mut recv) = self
                .conn
                .open_bi()
                .await
                .map_err(|e| Error::ReachedNobody(format!("link: {e}")))?;
            write_frame(&mut send, req).await?;
            send.finish()
                .map_err(|e| Error::ReachedNobody(format!("link: {e}")))?;
            read_frame(&mut recv).await
        };
        tokio::time::timeout(REQUEST_TIMEOUT, fut)
            .await
            .map_err(|_| Error::ReachedNobody("request timed out".into()))?
    }

    async fn next_request(&self) -> Result<(Wire, Box<dyn Reply>)> {
        let (send, mut recv) = self
            .conn
            .accept_bi()
            .await
            .map_err(|e| Error::ReachedNobody(format!("link closed: {e}")))?;
        let req = read_frame(&mut recv).await?;
        Ok((req, Box::new(IrohReply { send })))
    }

    fn is_closed(&self) -> bool {
        self.conn.close_reason().is_some()
    }

    fn close(&self) {
        self.conn.close(0u32.into(), b"bye");
    }
}

struct IrohReply {
    send: SendStream,
}

#[async_trait]
impl Reply for IrohReply {
    async fn send(mut self: Box<Self>, resp: &Wire) -> Result<()> {
        write_frame(&mut self.send, resp).await?;
        self.send
            .finish()
            .map_err(|e| Error::ReachedNobody(format!("link: {e}")))?;
        // Wait (briefly) for the peer to take the answer before the stream
        // handle goes away.
        let _ = tokio::time::timeout(REQUEST_TIMEOUT, self.send.stopped()).await;
        Ok(())
    }
}
