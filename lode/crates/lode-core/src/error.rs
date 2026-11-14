//! Stable error codes of the HTTP API (ARCHITECTURE §6).

use std::fmt;
use std::str::FromStr;

/// An error code with a stable string form and HTTP status. Clients match on these.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ErrorCode {
    /// No or bad credentials (401).
    Unauthenticated,
    /// Authenticated but not allowed (403).
    PermissionDenied,
    /// The thing does not exist or is not visible (404).
    NotFound,
    /// Malformed or semantically invalid request (400).
    InvalidRequest,
    /// A path breaks SPEC §5.7 (400).
    InvalidPath,
    /// A change is based on content that is no longer the branch head (409).
    OutOfDate,
    /// The request conflicts with current state, such as a stale shelf version (409).
    Conflict,
    /// A path is locked by someone else (409).
    Locked,
    /// A path needs a lock the author does not hold (409).
    LockRequired,
    /// Referenced blobs are not on the server (409).
    MissingObjects,
    /// The branch is archived and read-only (409).
    BranchArchived,
    /// Uploaded content does not match its id (422).
    HashMismatch,
    /// Over a configured size limit (413).
    TooLarge,
    /// Required checks have not passed (409). Defined by the checks module.
    ChecksRequired,
    /// The content was obliterated from the depot and cannot be read or stored again (410).
    Obliterated,
    /// Server fault (500).
    Internal,
}

impl ErrorCode {
    /// Every code, for exhaustive tests and documentation.
    pub const ALL: [ErrorCode; 16] = [
        ErrorCode::Unauthenticated,
        ErrorCode::PermissionDenied,
        ErrorCode::NotFound,
        ErrorCode::InvalidRequest,
        ErrorCode::InvalidPath,
        ErrorCode::OutOfDate,
        ErrorCode::Conflict,
        ErrorCode::Locked,
        ErrorCode::LockRequired,
        ErrorCode::MissingObjects,
        ErrorCode::BranchArchived,
        ErrorCode::HashMismatch,
        ErrorCode::TooLarge,
        ErrorCode::ChecksRequired,
        ErrorCode::Obliterated,
        ErrorCode::Internal,
    ];

    /// The wire form, such as `out_of_date`.
    pub fn as_str(self) -> &'static str {
        match self {
            ErrorCode::Unauthenticated => "unauthenticated",
            ErrorCode::PermissionDenied => "permission_denied",
            ErrorCode::NotFound => "not_found",
            ErrorCode::InvalidRequest => "invalid_request",
            ErrorCode::InvalidPath => "invalid_path",
            ErrorCode::OutOfDate => "out_of_date",
            ErrorCode::Conflict => "conflict",
            ErrorCode::Locked => "locked",
            ErrorCode::LockRequired => "lock_required",
            ErrorCode::MissingObjects => "missing_objects",
            ErrorCode::BranchArchived => "branch_archived",
            ErrorCode::HashMismatch => "hash_mismatch",
            ErrorCode::TooLarge => "too_large",
            ErrorCode::ChecksRequired => "checks_required",
            ErrorCode::Obliterated => "obliterated",
            ErrorCode::Internal => "internal",
        }
    }

    /// The HTTP status the server answers with.
    pub fn http_status(self) -> u16 {
        match self {
            ErrorCode::Unauthenticated => 401,
            ErrorCode::PermissionDenied => 403,
            ErrorCode::NotFound => 404,
            ErrorCode::InvalidRequest | ErrorCode::InvalidPath => 400,
            ErrorCode::OutOfDate
            | ErrorCode::Conflict
            | ErrorCode::Locked
            | ErrorCode::LockRequired
            | ErrorCode::MissingObjects
            | ErrorCode::BranchArchived
            | ErrorCode::ChecksRequired => 409,
            ErrorCode::HashMismatch => 422,
            ErrorCode::TooLarge => 413,
            ErrorCode::Obliterated => 410,
            ErrorCode::Internal => 500,
        }
    }
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The string is not a known error code. Clients should keep unknown codes as strings,
/// since modules may add their own.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown error code {0:?}")]
pub struct UnknownErrorCode(pub String);

impl FromStr for ErrorCode {
    type Err = UnknownErrorCode;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        ErrorCode::ALL
            .into_iter()
            .find(|c| c.as_str() == s)
            .ok_or_else(|| UnknownErrorCode(s.to_owned()))
    }
}

impl serde::Serialize for ErrorCode {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

impl<'de> serde::Deserialize<'de> for ErrorCode {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = <std::borrow::Cow<'de, str>>::deserialize(d)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn string_forms_round_trip_and_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for c in ErrorCode::ALL {
            assert!(seen.insert(c.as_str()));
            assert_eq!(c.as_str().parse::<ErrorCode>(), Ok(c));
        }
        assert!("nope".parse::<ErrorCode>().is_err());
    }

    #[test]
    fn statuses_match_architecture() {
        assert_eq!(ErrorCode::Unauthenticated.http_status(), 401);
        assert_eq!(ErrorCode::InvalidPath.http_status(), 400);
        assert_eq!(ErrorCode::BranchArchived.http_status(), 409);
        assert_eq!(ErrorCode::HashMismatch.http_status(), 422);
        assert_eq!(ErrorCode::TooLarge.http_status(), 413);
        assert_eq!(ErrorCode::Obliterated.http_status(), 410);
    }
}
