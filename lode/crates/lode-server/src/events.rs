//! The outbox dispatcher (ARCHITECTURE §8): delivers committed events to every enabled
//! module in order, at least once, and prunes what all of them have seen.
//!
//! Each module is a subscriber with a cursor in `outbox_cursor`. The cursor moves only after
//! `on_event` succeeds, so an event delivered just before a crash is delivered again after
//! the restart. A failing subscriber backs off without holding up the others.

use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use lode_meta::{OutboxRow, Reads};
use tokio::task::JoinHandle;

use crate::auth::ms_to_time;
use crate::module::{Event, Module};
use crate::state::AppState;

/// Events read per subscriber per round.
const BATCH: u32 = 256;
/// First retry delay after a failure; doubles up to [`MAX_BACKOFF`].
const FIRST_BACKOFF: Duration = Duration::from_secs(1);
/// Longest retry delay.
const MAX_BACKOFF: Duration = Duration::from_secs(300);
/// How often the dispatcher looks even without a notification.
const IDLE_POLL: Duration = Duration::from_secs(30);

/// Retry state of one subscriber.
#[derive(Debug, Clone, Default)]
struct Backoff {
    until: Option<tokio::time::Instant>,
    delay: Option<Duration>,
}

/// The dispatcher's memory between passes.
#[derive(Debug, Default)]
pub struct Dispatcher {
    backoff: HashMap<&'static str, Backoff>,
}

/// What one pass did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct PassReport {
    /// Events handed to subscribers successfully.
    pub delivered: usize,
    /// Subscribers that failed and are backing off.
    pub failed: usize,
    /// Outbox rows deleted.
    pub pruned: usize,
}

fn to_event(row: OutboxRow, depot_names: &HashMap<i64, String>) -> Event {
    Event {
        id: row.id,
        event: row.kind,
        depot: row.depot_id.and_then(|d| depot_names.get(&d.0).cloned()),
        created_at: ms_to_time(row.created_at),
        data: serde_json::from_str(&row.payload).unwrap_or(serde_json::Value::Null),
    }
}

/// Delivers everything pending to one subscriber, stopping at its first failure.
async fn catch_up(
    state: &AppState,
    module: &Arc<dyn Module>,
    depot_names: &HashMap<i64, String>,
) -> Result<usize, ()> {
    let name = module.name();
    let ctx = state.module_ctx(name);
    let mut delivered = 0;
    loop {
        let rows = state
            .meta()
            .read(move |tx| {
                let cursor = tx
                    .outbox_cursors()?
                    .into_iter()
                    .find(|(s, _)| s == name)
                    .map_or(0, |(_, c)| c);
                tx.outbox_after(cursor, BATCH)
            })
            .await
            .map_err(
                |e| tracing::error!(error = %e, subscriber = name, "cannot read the outbox"),
            )?;
        if rows.is_empty() {
            return Ok(delivered);
        }
        for row in rows {
            let id = row.id;
            let event = to_event(row, depot_names);
            if let Err(e) = module.on_event(&ctx, &event).await {
                tracing::warn!(error = %e, subscriber = name, event = id, "event handler failed; will retry");
                return Err(());
            }
            state
                .meta()
                .write(move |tx| tx.set_outbox_cursor(name, id))
                .await
                .map_err(
                    |e| tracing::error!(error = %e, subscriber = name, "cannot save the cursor"),
                )?;
            delivered += 1;
        }
    }
}

impl Dispatcher {
    /// One pass: every subscriber not backing off catches up, concurrently; then rows every
    /// registered subscriber has passed are deleted, and cursors of subscribers that are no
    /// longer registered are dropped.
    pub async fn pass(&mut self, state: &AppState) -> PassReport {
        let now = tokio::time::Instant::now();
        let depot_names: HashMap<i64, String> = state
            .meta()
            .read(|tx| tx.depots())
            .await
            .map(|ds| ds.into_iter().map(|d| (d.id.0, d.name)).collect())
            .unwrap_or_default();
        let due: Vec<Arc<dyn Module>> = state
            .modules()
            .iter()
            .filter(|m| {
                self.backoff
                    .get(m.name())
                    .and_then(|b| b.until)
                    .is_none_or(|t| t <= now)
            })
            .cloned()
            .collect();
        let results =
            futures::future::join_all(due.iter().map(|m| catch_up(state, m, &depot_names))).await;
        let mut report = PassReport::default();
        for (m, r) in due.iter().zip(results) {
            match r {
                Ok(n) => {
                    report.delivered += n;
                    self.backoff.remove(m.name());
                }
                Err(()) => {
                    report.failed += 1;
                    let b = self.backoff.entry(m.name()).or_default();
                    let delay = b.delay.map_or(FIRST_BACKOFF, |d| (d * 2).min(MAX_BACKOFF));
                    b.delay = Some(delay);
                    b.until = Some(tokio::time::Instant::now() + delay);
                }
            }
        }
        report.pruned = prune(state).await;
        report
    }

    /// When the next retry is due, if any subscriber is backing off.
    fn next_retry(&self) -> Option<tokio::time::Instant> {
        self.backoff.values().filter_map(|b| b.until).min()
    }

    /// Runs passes until `shutdown` completes: after every commit notification, when a retry
    /// is due, and at least every 30 seconds. The first pass runs at once, which delivers
    /// whatever a previous run left pending.
    pub fn spawn<F>(state: AppState, shutdown: F) -> JoinHandle<()>
    where
        F: Future<Output = ()> + Send + 'static,
    {
        tokio::spawn(async move {
            let mut d = Dispatcher::default();
            let notify = state.outbox_notify();
            tokio::pin!(shutdown);
            loop {
                d.pass(&state).await;
                let wake = d
                    .next_retry()
                    .unwrap_or_else(|| tokio::time::Instant::now() + IDLE_POLL);
                tokio::select! {
                    () = &mut shutdown => return,
                    () = notify.notified() => {}
                    () = tokio::time::sleep_until(wake) => {}
                }
            }
        })
    }
}

/// Deletes outbox rows every registered subscriber has passed and drops stale cursors.
async fn prune(state: &AppState) -> usize {
    let names: Vec<String> = state.module_names().to_vec();
    let r = state
        .meta()
        .write(move |tx| {
            let cursors = tx.outbox_cursors()?;
            for (s, _) in &cursors {
                if !names.contains(s) {
                    tx.delete_outbox_cursor(s)?;
                }
            }
            let upto = if names.is_empty() {
                // Nobody listens: every event is done.
                i64::MAX
            } else {
                names
                    .iter()
                    .map(|n| cursors.iter().find(|(s, _)| s == n).map_or(0, |(_, c)| *c))
                    .min()
                    .unwrap_or(0)
            };
            if upto > 0 {
                tx.prune_outbox(upto)
            } else {
                Ok(0)
            }
        })
        .await;
    r.unwrap_or_else(|e| {
        tracing::error!(error = %e, "cannot prune the outbox");
        0
    })
}
