//! Request and response types of the Lode HTTP API (ARCHITECTURE §5.1, §6, §9.1–9.2),
//! shared by server and client so both sides agree on one definition.
//!
//! Wire conventions: field names in `snake_case`; object ids as 64 hex characters; times as
//! RFC 3339 in UTC; unknown fields are ignored on input; optional fields are left out of the
//! output when empty. Snapshot tests in `tests/` pin the JSON form of every type.

#![forbid(unsafe_code)]

pub mod admin;
pub mod auth;
pub mod commit;
pub mod consts;
pub mod depot;
pub mod error;
pub mod locks;
pub mod read;
pub mod shelves;
pub mod webhooks;

pub use lode_core::{Change, ChangeAction, CopyFrom, DepotPath, EntryKind, ErrorCode, ObjectId};

/// A point in time, serialized as RFC 3339 UTC.
pub type Timestamp = time::OffsetDateTime;

pub(crate) mod rfc3339 {
    //! RFC 3339 with a `Z` offset, so the same instant always has the same text.
    use serde::{Deserialize, Deserializer, Serializer};
    use time::format_description::well_known::Rfc3339;
    use time::{OffsetDateTime, UtcOffset};

    pub fn serialize<S: Serializer>(t: &OffsetDateTime, s: S) -> Result<S::Ok, S::Error> {
        let text = t
            .to_offset(UtcOffset::UTC)
            .format(&Rfc3339)
            .map_err(serde::ser::Error::custom)?;
        s.serialize_str(&text)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<OffsetDateTime, D::Error> {
        let s = <std::borrow::Cow<'de, str>>::deserialize(d)?;
        OffsetDateTime::parse(&s, &Rfc3339)
            .map(|t| t.to_offset(UtcOffset::UTC))
            .map_err(serde::de::Error::custom)
    }

    pub mod option {
        use super::*;

        pub fn serialize<S: Serializer>(
            t: &Option<OffsetDateTime>,
            s: S,
        ) -> Result<S::Ok, S::Error> {
            match t {
                Some(t) => super::serialize(t, s),
                None => s.serialize_none(),
            }
        }

        pub fn deserialize<'de, D: Deserializer<'de>>(
            d: D,
        ) -> Result<Option<OffsetDateTime>, D::Error> {
            #[derive(Deserialize)]
            struct W(#[serde(with = "super")] OffsetDateTime);
            Ok(Option::<W>::deserialize(d)?.map(|w| w.0))
        }
    }
}

/// Human or agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PrincipalKind {
    /// A person.
    User,
    /// A non-human principal owned by a user.
    Agent,
}

/// A principal as other objects refer to it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PrincipalRef {
    /// Login name.
    pub name: String,
    /// User or agent.
    pub kind: PrincipalKind,
    /// For an agent, the user who owns it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
}

pub(crate) fn is_false(b: &bool) -> bool {
    !*b
}
