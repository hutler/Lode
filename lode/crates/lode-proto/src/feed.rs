//! Event feed module (SPEC §6.7): the events of one depot, read from a cursor.

use serde::{Deserialize, Serialize};

/// Longest a `GET …/events` request may be held waiting for an event.
pub const MAX_WAIT_SECONDS: u64 = 55;

/// Most events in one answer.
pub const MAX_EVENTS: usize = 500;

/// Event names the feed carries: the same as webhooks (ARCHITECTURE §8.1).
pub const EVENTS: &[&str] = crate::webhooks::EVENTS;

/// Query of `GET /depots/{d}/events`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventsQuery {
    /// Only events after this id. Absent: no events, and `next` is the newest id, so a client
    /// starts from now.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<i64>,
    /// Only these event names, comma-separated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kinds: Option<String>,
    /// Seconds to hold the request until an event arrives (at most [`MAX_WAIT_SECONDS`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wait: Option<u64>,
}

/// Answer of `GET /depots/{d}/events`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventList {
    /// Events, oldest first, each the envelope `{ id, event, depot, created_at, data }` of
    /// ARCHITECTURE §8.1: identifiers only.
    pub events: Vec<serde_json::Value>,
    /// The cursor to pass as `after` next time.
    pub next: i64,
    /// Events after `after` were dropped by retention: read the revision list again, then
    /// continue from `next`.
    pub truncated: bool,
}
