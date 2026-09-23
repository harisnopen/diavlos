//! Bridges to chat services. Humans keep their chat; agents keep the room.

pub mod buzz;
pub mod slack;
pub mod teams;

use diavlos_client::proto::Request;
use diavlos_client::Client;
use serde_json::Value;

/// Settle one watched message once the other service has it: ack if it was
/// posted, nack (try again in 30 s) if not. A crash between posting and
/// this ack means the message is posted again later: at-least-once, so a
/// duplicate post is possible, a lost one is not.
pub async fn settle(client: &Client, identity: &str, line: &Value, posted: bool) {
    let Some(token) = line["delivery"]["token"].as_str() else {
        return;
    };
    let req = if posted {
        Request::Ack {
            identity: identity.to_string(),
            token: token.to_string(),
        }
    } else {
        Request::Nack {
            identity: identity.to_string(),
            token: token.to_string(),
            retry_in_secs: Some(30),
        }
    };
    if let Err(e) = client.call(&req).await {
        eprintln!("could not settle a room message: {e}");
    }
}
