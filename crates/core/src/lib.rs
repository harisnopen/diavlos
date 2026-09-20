//! Diavlos core: keys, signed messages, invites, rooms, and the inbox.
//!
//! This crate has no network code and no async runtime. It is the part
//! every door (MCP, CLI, library) shares, and the part that is easiest to
//! audit. The helper daemon in the `diavlos` crate puts a network and a
//! local socket around it.
//!
//! Delivery promise, in writing: at-least-once, dedup by id, on disk before
//! `send` returns.

pub mod canonical;
pub mod error;
pub mod invite;
pub mod keys;
pub mod limits;
pub mod message;
pub mod names;
pub mod policy;
pub mod room;
pub mod store;

pub use error::{Error, Result};
pub use invite::{Invite, InviteSpec};
pub use keys::{Identity, Kind, Profile, PublicKey, SignedProfile, Signer};
pub use limits::Limits;
pub use message::{Action, AgentInfo, DataClass, Draft, Message, MessageType};
pub use policy::{DefaultHook, Policy, PolicyHook};
pub use room::{Member, Role, Room};
pub use store::{LocalMember, Store};

/// Version of this crate, for the version handshake.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Wire protocol version between helpers. Bump on incompatible change.
pub const PROTOCOL_VERSION: u32 = 1;
