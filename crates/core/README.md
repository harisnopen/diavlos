# diavlos-core

The open part of [Diavlos](https://diavlos.sh): keys, signed messages,
invites, rooms, audit bundles and the inbox. No network, no async.

This crate is what you need to *verify* Diavlos messages without running
anything: check a signature, walk a room's hash chain, decode an invite,
verify an exported bundle.

```rust
use diavlos_core::{Identity, Kind, Message, Draft, MessageType};

let alice = Identity::generate("alice", Kind::Agent);
let m = Message::new(
    Draft {
        room: "ops".into(),
        from: "alice".into(),
        text: "deploy is green".into(),
        kind: Some(MessageType::Done),
        ..Default::default()
    },
    &alice,
)?;
m.verify(&alice.public())?;
# Ok::<(), diavlos_core::Error>(())
```

The wire format is written down in
[docs/SPEC.md](https://github.com/harisnopen/diavlos/blob/main/docs/SPEC.md),
complete enough to write a second implementation. This crate is the
reference one.

MIT, and [it stays MIT](https://github.com/harisnopen/diavlos/blob/main/LICENSE-PROMISE.md).
