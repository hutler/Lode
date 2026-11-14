//! Authentication: tokens, passwords, scopes, the [`Caller`] extractor and the auth and
//! administration endpoints for principals, groups and tokens.

mod caller;
mod handlers;
mod password;
mod ratelimit;
mod scopes;
mod sso;
mod token;

pub(crate) use caller::authenticate;
pub use caller::{Caller, ADMIN_USER};
pub use handlers::routes;
pub(crate) use handlers::{check_password_sign_in, issue_token, would_be_server_admin, ClientAddr};
pub use password::{hash_password, verify_password, MIN_PASSWORD_LEN};
pub use ratelimit::{LoginLimiter, LoginPolicy};
pub use scopes::{ScopeRegistry, CORE_SCOPES, SCOPE_ADMIN, SCOPE_READ, SCOPE_WRITE};
pub use sso::Sso;
pub(crate) use sso::{finish_admin, random_verifier, start_admin, ADMIN_SSO_DONE};
pub(crate) use token::ct_eq;
pub use token::{generate_token, hash_token, TOKEN_PREFIX};

/// Converts stored milliseconds to a timestamp.
pub fn ms_to_time(ms: i64) -> time::OffsetDateTime {
    time::OffsetDateTime::from_unix_timestamp_nanos(i128::from(ms) * 1_000_000)
        .unwrap_or(time::OffsetDateTime::UNIX_EPOCH)
}

/// Converts a timestamp to stored milliseconds.
pub fn time_to_ms(t: time::OffsetDateTime) -> i64 {
    i64::try_from(t.unix_timestamp_nanos() / 1_000_000).unwrap_or(i64::MAX)
}
