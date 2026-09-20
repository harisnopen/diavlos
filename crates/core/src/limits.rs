//! Floods and loops: rate limit per sender and a daily budget per room.
//! Both on by default. Two agents in a loop stop before your API bill
//! notices.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Limits {
    /// Messages one sender may post in a room per minute.
    #[serde(default = "default_per_minute")]
    pub per_minute_per_sender: u32,
    /// Messages a room may hold per UTC day, all senders together.
    #[serde(default = "default_daily")]
    pub daily_per_room: u32,
    /// Warn the owner once a room passes this share of its daily budget.
    #[serde(default = "default_burst_alert_percent")]
    pub burst_alert_percent: u32,
}

fn default_per_minute() -> u32 {
    60
}
fn default_daily() -> u32 {
    2000
}
fn default_burst_alert_percent() -> u32 {
    80
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            per_minute_per_sender: default_per_minute(),
            daily_per_room: default_daily(),
            burst_alert_percent: default_burst_alert_percent(),
        }
    }
}
