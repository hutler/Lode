//! Named failpoints, so tests can make the commit path fail at a chosen step and check that
//! nothing is left inconsistent (SPEC §10). Nothing arms them in a running server; an unarmed
//! check is one atomic load.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use crate::error::ApiError;

/// Before the new trees are stored: nothing durable has happened yet.
pub const COMMIT_BEFORE_TREES: &str = "commit.before_trees";
/// Inside the metadata transaction, after the revision rows are written: rolls them back,
/// leaving only unreferenced trees for GC.
pub const COMMIT_IN_TRANSACTION: &str = "commit.in_transaction";
/// After the transaction committed, before the response: the revision exists but the client
/// sees an error, like a timeout after the server committed.
pub const COMMIT_AFTER_TRANSACTION: &str = "commit.after_transaction";

/// Every failpoint, for tests that try them all.
pub const ALL: &[&str] = &[
    COMMIT_BEFORE_TREES,
    COMMIT_IN_TRANSACTION,
    COMMIT_AFTER_TRANSACTION,
];

/// The armed failpoints of one server.
#[derive(Debug, Default)]
pub struct Failpoints {
    any: AtomicBool,
    /// Remaining hits per armed point; `u32::MAX` means every time.
    armed: Mutex<HashMap<String, u32>>,
}

impl Failpoints {
    /// Makes `name` fail the next `times` times it is reached.
    pub fn arm(&self, name: &str, times: u32) {
        if let Ok(mut m) = self.armed.lock() {
            m.insert(name.to_owned(), times);
            self.any.store(true, Ordering::SeqCst);
        }
    }

    /// Makes `name` fail every time until disarmed.
    pub fn arm_always(&self, name: &str) {
        self.arm(name, u32::MAX);
    }

    /// Disarms `name`.
    pub fn disarm(&self, name: &str) {
        if let Ok(mut m) = self.armed.lock() {
            m.remove(name);
            self.any.store(!m.is_empty(), Ordering::SeqCst);
        }
    }

    /// Whether `name` fires now; counts the hit.
    pub fn hit(&self, name: &str) -> bool {
        if !self.any.load(Ordering::SeqCst) {
            return false;
        }
        let Ok(mut m) = self.armed.lock() else {
            return false;
        };
        let Some(left) = m.get_mut(name) else {
            return false;
        };
        if *left != u32::MAX {
            *left -= 1;
            if *left == 0 {
                m.remove(name);
                self.any.store(!m.is_empty(), Ordering::SeqCst);
            }
        }
        tracing::warn!(failpoint = name, "failpoint fired");
        true
    }

    /// `Err(internal)` when `name` fires.
    pub fn check(&self, name: &str) -> Result<(), ApiError> {
        if self.hit(name) {
            Err(ApiError::internal())
        } else {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counted_and_permanent() {
        let f = Failpoints::default();
        assert!(!f.hit("x"));
        f.arm("x", 2);
        assert!(f.hit("x"));
        assert!(f.hit("x"));
        assert!(!f.hit("x"));
        f.arm_always("y");
        for _ in 0..5 {
            assert!(f.hit("y"));
        }
        f.disarm("y");
        assert!(!f.hit("y"));
        assert!(f.check("y").is_ok());
    }
}
