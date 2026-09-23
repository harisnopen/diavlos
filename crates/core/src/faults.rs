//! Named places where a test can make an operation fail on purpose.
//!
//! Every [`Store`](crate::Store) and every helper carries one of these.
//! Nothing arms them outside tests: there is no config key, flag or
//! environment variable for it, only this API. Unarmed, a check is one
//! atomic load.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use crate::error::{Error, Result};

#[derive(Debug, Default)]
pub struct Faults {
    any: AtomicBool,
    /// name -> how many more hits to let through before firing once.
    armed: Mutex<HashMap<String, u32>>,
}

impl Faults {
    /// Fire once, on the next hit of `point`.
    pub fn arm(&self, point: &str) {
        self.arm_after(point, 0);
    }

    /// Let `skip` hits of `point` through, then fire once.
    pub fn arm_after(&self, point: &str, skip: u32) {
        if let Ok(mut a) = self.armed.lock() {
            a.insert(point.to_string(), skip);
            self.any.store(true, Ordering::SeqCst);
        }
    }

    /// True if `point` fires now. A fired point is disarmed.
    pub fn hit(&self, point: &str) -> bool {
        if !self.any.load(Ordering::SeqCst) {
            return false;
        }
        let Ok(mut a) = self.armed.lock() else {
            return false;
        };
        match a.get_mut(point) {
            Some(0) => {
                a.remove(point);
                if a.is_empty() {
                    self.any.store(false, Ordering::SeqCst);
                }
                true
            }
            Some(n) => {
                *n -= 1;
                false
            }
            None => false,
        }
    }

    /// `Err` with a storage I/O error if `point` fires now: what a failed
    /// disk write looks like to the caller.
    pub fn check(&self, point: &str) -> Result<()> {
        if self.hit(point) {
            return Err(Error::Db(rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_IOERR),
                Some(format!("injected fault at {point}")),
            )));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fires_once_after_the_skipped_hits() {
        let f = Faults::default();
        assert!(!f.hit("x"));
        f.arm_after("x", 2);
        assert!(!f.hit("x"));
        assert!(!f.hit("x"));
        assert!(f.hit("x"));
        assert!(!f.hit("x"));
        f.arm("y");
        assert!(f.check("y").is_err());
        assert!(f.check("y").is_ok());
    }
}
