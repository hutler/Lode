//! Chat formats (T175): one line of text per event, in the body Slack, Discord or Telegram
//! expects, so a team hears in its chat when a shelf waits for review or locks change.
//!
//! Events carry identifiers only, never paths or content, and so do the messages: who, what,
//! which depot, branch, revision or shelf. When the server knows its public address
//! ([`lode_server::ServerConfig::public_base`]) a message ends with a link to the web page of
//! the revision or shelf.

use lode_proto::webhooks::{WebhookFormat, PING_EVENT};
use lode_server::module::Event;
use serde_json::{json, Value};

/// The chat named by a Telegram hook URL's `chat_id` query parameter.
pub fn telegram_chat(url: &str) -> Option<String> {
    let parsed = reqwest::Url::parse(url).ok()?;
    parsed
        .query_pairs()
        .find(|(k, _)| k == "chat_id")
        .map(|(_, v)| v.into_owned())
        .filter(|v| !v.is_empty())
}

/// The body of a delivery of `event` in chat format `format` to `url`.
pub fn body(format: WebhookFormat, url: &str, event: &Event, base: Option<&str>) -> String {
    let text = text(event, base);
    let v = match format {
        WebhookFormat::Discord => json!({ "content": text }),
        WebhookFormat::Telegram => json!({
            "chat_id": telegram_chat(url).unwrap_or_default(),
            "text": text,
            "disable_web_page_preview": true,
        }),
        WebhookFormat::Slack | WebhookFormat::Lode => json!({ "text": text }),
    };
    v.to_string()
}

fn s<'a>(data: &'a Value, key: &str) -> &'a str {
    data.get(key).and_then(Value::as_str).unwrap_or("?")
}

fn n(data: &Value, key: &str) -> String {
    data.get(key)
        .and_then(Value::as_u64)
        .map_or_else(|| "?".to_owned(), |v| v.to_string())
}

fn agent(data: &Value, kind_key: &str) -> &'static str {
    if data.get(kind_key).and_then(Value::as_str) == Some("agent") {
        " (agent)"
    } else {
        ""
    }
}

fn encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for b in text.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// The one-line message for `event`, with a link to its web page when `base` is known.
pub fn text(event: &Event, base: Option<&str>) -> String {
    let depot = event.depot.as_deref().unwrap_or("?");
    let d = &event.data;
    let web = |tail: String| base.map(|b| format!("{b}/web/depots/{}/{tail}", encode(depot)));
    let revision = |key: &str| {
        d.get(key)
            .and_then(Value::as_u64)
            .and_then(|r| web(format!("r/{r}")))
    };
    let shelf = || {
        d.get("shelf_id")
            .and_then(Value::as_u64)
            .and_then(|id| web(format!("shelves/{id}")))
    };
    let (line, link) = match event.event.as_str() {
        "revision.created" => {
            let by = if s(d, "submitter") != s(d, "author") {
                format!(", submitted by {}", s(d, "submitter"))
            } else {
                String::new()
            };
            (
                format!(
                    "{}{} committed r{} to {}{}",
                    s(d, "author"),
                    agent(d, "author_kind"),
                    n(d, "rev"),
                    s(d, "branch"),
                    by
                ),
                revision("rev"),
            )
        }
        "branch.created" => (
            format!(
                "branch {} created from {} at r{}",
                s(d, "branch"),
                s(d, "from_branch"),
                n(d, "from_rev")
            ),
            None,
        ),
        "branch.archived" => (format!("branch {} archived", s(d, "branch")), None),
        "lock.acquired" => (
            format!(
                "{} locked {} on {}",
                s(d, "owner"),
                count(d, "file"),
                s(d, "branch")
            ),
            None,
        ),
        "lock.released" => (
            format!(
                "{} released {} on {}",
                s(d, "owner"),
                count(d, "lock"),
                s(d, "branch")
            ),
            None,
        ),
        "shelf.created" => (
            format!(
                "{}{} shelved #{} on {} for review",
                s(d, "owner"),
                agent(d, "owner_kind"),
                n(d, "shelf_id"),
                s(d, "branch")
            ),
            shelf(),
        ),
        "shelf.updated" => (
            format!(
                "{}{} updated shelf #{} on {} (version {})",
                s(d, "owner"),
                agent(d, "owner_kind"),
                n(d, "shelf_id"),
                s(d, "branch"),
                n(d, "version")
            ),
            shelf(),
        ),
        "shelf.deleted" => (
            format!(
                "shelf #{} of {} on {} was deleted",
                n(d, "shelf_id"),
                s(d, "owner"),
                s(d, "branch")
            ),
            None,
        ),
        "shelf.submitted" => (
            format!(
                "shelf #{} of {} was submitted as r{} on {}",
                n(d, "shelf_id"),
                s(d, "owner"),
                n(d, "rev"),
                s(d, "branch")
            ),
            revision("rev"),
        ),
        "check.updated" => {
            let target = match s(d, "target_kind") {
                "shelf" => format!(
                    "shelf #{} version {}",
                    n(d, "target_id"),
                    n(d, "target_version")
                ),
                _ => format!("r{}", n(d, "target_id")),
            };
            let link = match s(d, "target_kind") {
                "shelf" => d
                    .get("target_id")
                    .and_then(Value::as_u64)
                    .and_then(|id| web(format!("shelves/{id}"))),
                _ => revision("target_id"),
            };
            (
                format!(
                    "check {} is {} on {} (reported by {})",
                    s(d, "name"),
                    s(d, "state"),
                    target,
                    s(d, "reporter")
                ),
                link,
            )
        }
        PING_EVENT => (
            format!(
                "test message: webhook {} of this depot reaches this chat",
                n(d, "hook_id")
            ),
            None,
        ),
        other => (other.to_owned(), None),
    };
    match link {
        Some(l) => format!("Lode {depot}: {line}. {l}"),
        None => format!("Lode {depot}: {line}."),
    }
}

fn count(d: &Value, noun: &str) -> String {
    match d.get("count").and_then(Value::as_u64) {
        Some(1) => format!("1 {noun}"),
        Some(c) => format!("{c} {noun}s"),
        None => format!("{noun}s"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(event: &str, data: Value) -> Event {
        Event {
            id: 7,
            event: event.to_owned(),
            depot: Some("game".to_owned()),
            created_at: time::OffsetDateTime::UNIX_EPOCH,
            data,
        }
    }

    #[test]
    fn messages_name_who_what_and_where() {
        let base = Some("https://lode.example.com");
        let shelf = ev(
            "shelf.created",
            json!({ "shelf_id": 4, "version": 1, "branch": "main", "base_rev": 9,
                    "owner": "bot", "owner_kind": "agent" }),
        );
        assert_eq!(
            text(&shelf, base),
            "Lode game: bot (agent) shelved #4 on main for review. \
             https://lode.example.com/web/depots/game/shelves/4"
        );
        assert_eq!(
            text(&shelf, None),
            "Lode game: bot (agent) shelved #4 on main for review."
        );
        let rev = ev(
            "revision.created",
            json!({ "rev": 12, "branch": "main", "author": "bot", "author_kind": "agent",
                    "submitter": "ana", "origin": "shelves" }),
        );
        assert_eq!(
            text(&rev, base),
            "Lode game: bot (agent) committed r12 to main, submitted by ana. \
             https://lode.example.com/web/depots/game/r/12"
        );
        let lock = ev(
            "lock.acquired",
            json!({ "branch": "main", "owner": "ana", "count": 3 }),
        );
        assert_eq!(text(&lock, base), "Lode game: ana locked 3 files on main.");
        let check = ev(
            "check.updated",
            json!({ "target_kind": "shelf", "target_id": 4, "target_version": 2,
                    "name": "ci/build", "state": "failure", "reporter": "ci" }),
        );
        assert_eq!(
            text(&check, None),
            "Lode game: check ci/build is failure on shelf #4 version 2 (reported by ci)."
        );
    }

    #[test]
    fn bodies_per_service() {
        let e = ev("branch.archived", json!({ "branch": "old" }));
        let url = "https://api.telegram.org/bot1:abc/sendMessage?chat_id=-100123";
        let v: Value = serde_json::from_str(&body(WebhookFormat::Telegram, url, &e, None)).unwrap();
        assert_eq!(v["chat_id"], "-100123");
        assert_eq!(v["text"], "Lode game: branch old archived.");
        let v: Value =
            serde_json::from_str(&body(WebhookFormat::Discord, "https://x", &e, None)).unwrap();
        assert_eq!(v, json!({ "content": "Lode game: branch old archived." }));
        let v: Value =
            serde_json::from_str(&body(WebhookFormat::Slack, "https://x", &e, None)).unwrap();
        assert_eq!(v, json!({ "text": "Lode game: branch old archived." }));
        assert_eq!(
            telegram_chat("https://api.telegram.org/bot1/sendMessage"),
            None
        );
    }
}
