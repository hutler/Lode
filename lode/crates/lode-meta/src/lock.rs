//! One active server per Postgres database (T153).
//!
//! `loded serve` holds a session-level advisory lock on a connection of its own. Postgres
//! frees the lock when that connection ends, so a standby waiting with `--wait-for-lock`
//! takes over when the active server stops or dies. A thread checks the connection every
//! few seconds; if it is lost, [`ServerLock::lost`] completes and the server must stop,
//! because another one may already have taken over.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crate::pg::{wait, wait_timeout, PgConn};
use crate::{Backend, MetaError, MetaStore, Result};

/// How often the lock connection is checked.
const PING_EVERY: Duration = Duration::from_secs(3);
/// How long a check may take before the connection counts as lost.
const PING_TIMEOUT: Duration = Duration::from_secs(10);

/// The advisory lock key: the database and schema, so servers in different schemas of one
/// database do not block each other.
const KEY_SQL: &str =
    "SELECT hashtextextended('lode-server:' || current_database() || '.' || current_schema(), 0)";

/// The answer to [`MetaStore::try_server_lock`].
#[derive(Debug)]
pub enum LockAttempt {
    /// This process is now the active server.
    Held(ServerLock),
    /// Another server holds the lock; the text names it as well as Postgres can.
    InUse(String),
}

/// Held while this process is the active server. Dropping it releases the lock.
#[derive(Debug)]
pub struct ServerLock {
    stop: Arc<AtomicBool>,
    lost: tokio::sync::watch::Receiver<bool>,
}

impl ServerLock {
    /// A lock for a database that needs none (SQLite: the file is opened by one server).
    fn none() -> Self {
        let (_tx, rx) = tokio::sync::watch::channel(false);
        Self {
            stop: Arc::new(AtomicBool::new(false)),
            lost: rx,
        }
    }

    /// Completes if the lock connection fails. Never completes for SQLite.
    pub async fn lost(&self) {
        let mut rx = self.lost.clone();
        loop {
            if *rx.borrow() {
                return;
            }
            if rx.changed().await.is_err() {
                // The checking thread ended without losing the lock (it was released).
                std::future::pending::<()>().await;
            }
        }
    }
}

impl Drop for ServerLock {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

fn holder(conn: &mut PgConn, key: i64) -> String {
    let hi = key >> 32;
    let lo = key & 0xffff_ffff;
    let Ok(client) = conn.client() else {
        return "another server".to_owned();
    };
    let row = wait(client.query_opt(
        "SELECT a.application_name, coalesce(host(a.client_addr), 'local'), a.backend_start::text
         FROM pg_locks l JOIN pg_stat_activity a ON a.pid = l.pid
         WHERE l.locktype = 'advisory' AND l.granted AND l.objsubid = 1
           AND l.classid::bigint = ($1::bigint & 4294967295) AND l.objid::bigint = $2
         LIMIT 1",
        &[&hi, &lo],
    ));
    match row {
        Ok(Some(r)) => {
            let app: String = r.try_get(0).unwrap_or_default();
            let addr: String = r.try_get(1).unwrap_or_default();
            let since: String = r.try_get(2).unwrap_or_default();
            format!("{app} from {addr}, connected since {since}")
        }
        _ => "another server".to_owned(),
    }
}

impl MetaStore {
    /// Tries once to become the active server. `holder` names this process to others (it is
    /// the lock connection's `application_name`). Always succeeds for SQLite.
    pub async fn try_server_lock(&self, holder_name: &str) -> Result<LockAttempt> {
        let Backend::Postgres(b) = &*self.backend else {
            return Ok(LockAttempt::Held(ServerLock::none()));
        };
        let target = b.target.with_application_name(holder_name);
        tokio::task::spawn_blocking(move || {
            let mut conn = target.connect()?;
            let key: i64 = wait(conn.client()?.query_one(KEY_SQL, &[]))?.try_get(0)?;
            let got: bool = wait(
                conn.client()?
                    .query_one("SELECT pg_try_advisory_lock($1)", &[&key]),
            )?
            .try_get(0)?;
            if !got {
                return Ok(LockAttempt::InUse(holder(&mut conn, key)));
            }
            let stop = Arc::new(AtomicBool::new(false));
            let (tx, rx) = tokio::sync::watch::channel(false);
            let flag = stop.clone();
            std::thread::Builder::new()
                .name("lode-server-lock".into())
                .spawn(move || {
                    let step = Duration::from_millis(100);
                    let mut waited = Duration::ZERO;
                    while !flag.load(Ordering::SeqCst) {
                        std::thread::sleep(step);
                        waited += step;
                        if waited < PING_EVERY {
                            continue;
                        }
                        waited = Duration::ZERO;
                        let alive = !conn.is_closed()
                            && conn.client().is_ok_and(|c| {
                                matches!(
                                    wait_timeout(c.simple_query("SELECT 1"), PING_TIMEOUT),
                                    Ok(Some(Ok(_)))
                                )
                            });
                        if !alive {
                            let _ = tx.send(true);
                            return;
                        }
                    }
                    // Dropping the connection releases the lock.
                    drop(conn);
                })
                .map_err(|e| MetaError::Setup(format!("cannot start the lock thread: {e}")))?;
            Ok(LockAttempt::Held(ServerLock { stop, lost: rx }))
        })
        .await
        .map_err(|e| MetaError::Setup(format!("lock task failed: {e}")))?
    }
}
