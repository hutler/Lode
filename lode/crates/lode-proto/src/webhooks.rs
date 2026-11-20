//! Webhooks module (ARCHITECTURE §9.4).

use serde::{Deserialize, Serialize};

use crate::{rfc3339, PrincipalRef, Timestamp};

/// Event names a hook may subscribe to (ARCHITECTURE §8.1).
pub const EVENTS: &[&str] = &[
    "revision.created",
    "branch.created",
    "branch.archived",
    "lock.acquired",
    "lock.released",
    "shelf.created",
    "shelf.updated",
    "shelf.deleted",
    "shelf.submitted",
    "check.updated",
];

/// The event name of a test delivery sent by `POST …/webhooks/{id}/ping`.
pub const PING_EVENT: &str = "ping";

/// Header naming the event of a delivery.
pub const HEADER_EVENT: &str = "X-Lode-Event";
/// Header carrying the delivery id, the same on every attempt.
pub const HEADER_DELIVERY: &str = "X-Lode-Delivery";
/// Header carrying `sha256=<hex HMAC-SHA256 of the body>`, keyed with the hook's secret.
pub const HEADER_SIGNATURE: &str = "X-Lode-Signature";

/// What a hook's deliveries look like (T175).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WebhookFormat {
    /// The signed JSON event (the default, and the only format before T175).
    #[default]
    Lode,
    /// A Slack incoming webhook: `{"text": …}`.
    Slack,
    /// A Discord webhook: `{"content": …}`.
    Discord,
    /// The Telegram Bot API's `sendMessage`: `{"chat_id": …, "text": …}`, the chat taken
    /// from the hook URL's `chat_id` query parameter.
    Telegram,
}

impl WebhookFormat {
    /// Every format, in the order pickers show them.
    pub const ALL: [WebhookFormat; 4] = [Self::Lode, Self::Slack, Self::Discord, Self::Telegram];

    /// The name in requests, configuration and the CLI.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Lode => "lode",
            Self::Slack => "slack",
            Self::Discord => "discord",
            Self::Telegram => "telegram",
        }
    }

    /// The format named `s`.
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|f| f.as_str() == s)
    }

    /// Whether this is the default format, which is left out of JSON so that older peers
    /// see the documents they know.
    pub fn is_lode(&self) -> bool {
        *self == Self::Lode
    }
}

/// `POST /depots/{d}/webhooks`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateWebhookRequest {
    /// Where to send events: `https://`, or `http://` when the server allows it.
    pub url: String,
    /// Event names, from [`EVENTS`]; at least one.
    pub events: Vec<String>,
    /// Only events on branches matching this glob. Events without a branch are not
    /// filtered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// The deliveries' format; `lode` when absent.
    #[serde(default, skip_serializing_if = "WebhookFormat::is_lode")]
    pub format: WebhookFormat,
}

/// A hook, without its secret.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebhookView {
    /// Id.
    pub id: i64,
    /// Depot.
    pub depot: String,
    /// Receiver URL.
    pub url: String,
    /// Subscribed events.
    pub events: Vec<String>,
    /// Branch glob, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// The deliveries' format; `lode` when absent.
    #[serde(default, skip_serializing_if = "WebhookFormat::is_lode")]
    pub format: WebhookFormat,
    /// Whether the hook is sending.
    pub active: bool,
    /// Who created it.
    pub created_by: PrincipalRef,
    /// When.
    #[serde(with = "rfc3339")]
    pub created_at: Timestamp,
}

/// Answer to a create: the hook and its secret, shown this once.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreatedWebhook {
    /// The hook.
    pub hook: WebhookView,
    /// The signing secret. The HMAC key is this text as UTF-8 bytes.
    pub secret: String,
}

/// `GET /depots/{d}/webhooks`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebhookList {
    /// Hooks, oldest first.
    pub webhooks: Vec<WebhookView>,
}

/// State of a delivery.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DeliveryState {
    /// Not yet accepted; another attempt is due.
    Pending,
    /// The receiver answered 2xx.
    Ok,
    /// Gave up after 24 hours of attempts.
    Failed,
}

impl DeliveryState {
    /// The wire and database form.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Ok => "ok",
            Self::Failed => "failed",
        }
    }

    /// Parses the wire form.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "pending" => Some(Self::Pending),
            "ok" => Some(Self::Ok),
            "failed" => Some(Self::Failed),
            _ => None,
        }
    }
}

/// One delivery of one event to one hook.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeliveryView {
    /// Delivery id, sent as `X-Lode-Delivery`.
    pub id: i64,
    /// Hook id.
    pub hook_id: i64,
    /// Event name.
    pub event: String,
    /// State.
    pub state: DeliveryState,
    /// Attempts made so far.
    pub attempts: u32,
    /// When the next attempt is due, while pending.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "rfc3339::option"
    )]
    pub next_attempt_at: Option<Timestamp>,
    /// HTTP status of the last attempt, if the receiver answered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_status: Option<u16>,
    /// Why the last attempt failed, if it did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    /// When the event was queued for this hook.
    #[serde(with = "rfc3339")]
    pub created_at: Timestamp,
}

/// `GET /depots/{d}/webhooks/{id}/deliveries`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeliveryList {
    /// Recent deliveries, newest first.
    pub deliveries: Vec<DeliveryView>,
}
