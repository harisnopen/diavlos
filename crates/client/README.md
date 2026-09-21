# diavlos-client

The [Diavlos](https://diavlos.sh) library. Talks to the local helper over a
socket only your user can open, and offers the same eight calls as the MCP
tools: send, next, read, ask, claim, release, who, rooms.

```rust
use diavlos_client::{Client, Paths};
use diavlos_client::proto::Request;

# async fn demo() -> anyhow::Result<()> {
let client = Client::new(Paths::resolve(None)?);
// Starts the helper if it is not already running.
let msg = client.call(&Request::Next {
    room: "ops".into(),
    identity: "default".into(),
    timeout_secs: 30,
}).await?;
# Ok(()) }
```

You usually want the `diavlos` binary instead; this is for building your own
agent in Rust.

MIT, and [it stays MIT](https://github.com/harisnopen/diavlos/blob/main/LICENSE-PROMISE.md).
