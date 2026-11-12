//! The registry of scope names a token may carry.

use std::collections::BTreeSet;

/// All reads.
pub const SCOPE_READ: &str = "read";
/// Commits and branch operations.
pub const SCOPE_WRITE: &str = "write";
/// Administration.
pub const SCOPE_ADMIN: &str = "admin";
/// The scopes the core defines.
pub const CORE_SCOPES: [&str; 3] = [SCOPE_READ, SCOPE_WRITE, SCOPE_ADMIN];

/// Scopes known to this server: the core set plus those declared by enabled modules.
///
/// A token may only be created with registered scopes. A stored scope that is no longer
/// registered (its module was disabled) is kept on the token and ignored.
#[derive(Debug, Clone)]
pub struct ScopeRegistry {
    names: BTreeSet<String>,
}

impl Default for ScopeRegistry {
    fn default() -> Self {
        Self::core()
    }
}

impl ScopeRegistry {
    /// Only the core scopes.
    pub fn core() -> Self {
        Self {
            names: CORE_SCOPES.iter().map(|s| (*s).to_owned()).collect(),
        }
    }

    /// Adds scopes declared by a module.
    pub fn extend<I, S>(&mut self, names: I)
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.names.extend(names.into_iter().map(Into::into));
    }

    /// Whether `name` is registered.
    pub fn contains(&self, name: &str) -> bool {
        self.names.contains(name)
    }

    /// Every registered scope, sorted.
    pub fn all(&self) -> Vec<String> {
        self.names.iter().cloned().collect()
    }

    /// The registered subset of `stored`, preserving order.
    pub fn effective(&self, stored: &[String]) -> Vec<String> {
        stored
            .iter()
            .filter(|s| self.contains(s))
            .cloned()
            .collect()
    }
}
