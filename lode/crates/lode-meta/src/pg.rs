//! Postgres backend (T153): connections, TLS, transactions and the [`Sql`] implementation.
//!
//! The store's methods run on blocking threads, as for SQLite. Postgres connections are
//! driven by a small Tokio runtime of this module's own, and the blocking threads wait for
//! the client's futures with `futures::executor::block_on`, which works on any thread,
//! inside a runtime or not. Statements are prepared once per connection and cached.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use bytes::BytesMut;
use std::future::Future;
use std::sync::OnceLock;

use tokio_postgres::error::SqlState;
use tokio_postgres::types::{IsNull, ToSql, Type};
use tokio_postgres::{Client, IsolationLevel, Statement, Transaction};

use crate::sql::{to_postgres_params, Cell, Dialect, Row, Sql, Val};
use crate::{MetaError, Result};

/// Most statements kept prepared on one connection; the cache is emptied when it grows past.
const STATEMENT_CACHE: usize = 512;

/// The runtime that drives every Postgres connection.
fn runtime() -> Result<&'static tokio::runtime::Runtime> {
    static RT: OnceLock<std::result::Result<tokio::runtime::Runtime, String>> = OnceLock::new();
    RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("lode-pg")
            .enable_all()
            .build()
            .map_err(|e| e.to_string())
    })
    .as_ref()
    .map_err(|e| MetaError::Setup(format!("cannot start the Postgres runtime: {e}")))
}

/// Waits for a client future on the calling (blocking) thread.
pub(crate) fn wait<F: Future>(f: F) -> F::Output {
    futures::executor::block_on(f)
}

/// [`wait`] with a time limit; `None` when it ran out.
pub(crate) fn wait_timeout<F: Future>(f: F, limit: Duration) -> Result<Option<F::Output>> {
    let _guard = runtime()?.enter();
    Ok(wait(tokio::time::timeout(limit, f)).ok())
}

/// How Lode treats `sslmode` (libpq's names). `verify-ca` is treated as `verify-full`:
/// rustls always checks the host name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TlsMode {
    /// No TLS.
    Disable,
    /// TLS when the server offers it, without checking its certificate (libpq's default).
    Prefer,
    /// TLS required, certificate not checked (unless a root certificate is given).
    Require,
    /// TLS required and the certificate checked against the system roots and `sslrootcert`.
    VerifyFull,
}

/// Where and how to connect.
#[derive(Clone)]
pub(crate) struct PgTarget {
    config: tokio_postgres::Config,
    tls: TlsMode,
    root_cert: Option<std::path::PathBuf>,
    /// A display form of the URL without the password, for messages.
    pub(crate) shown: String,
}

impl std::fmt::Debug for PgTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.shown)
    }
}

impl PgTarget {
    /// Parses a `postgres://` URL. `sslmode` and `sslrootcert` are read here, the rest by the
    /// driver.
    pub(crate) fn parse(url: &str) -> Result<Self> {
        let url = url.trim();
        let (base, query) = match url.split_once('?') {
            Some((b, q)) => (b, q),
            None => (url, ""),
        };
        let mut tls = TlsMode::Prefer;
        let mut root_cert = None;
        let mut rest = Vec::new();
        for pair in query.split('&').filter(|p| !p.is_empty()) {
            let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
            match k {
                "sslmode" => {
                    tls = match v {
                        "disable" => TlsMode::Disable,
                        "allow" | "prefer" => TlsMode::Prefer,
                        "require" => TlsMode::Require,
                        "verify-ca" | "verify-full" => TlsMode::VerifyFull,
                        other => {
                            return Err(MetaError::Setup(format!(
                                "unknown sslmode {other:?} in the database URL"
                            )))
                        }
                    }
                }
                "sslrootcert" => root_cert = Some(std::path::PathBuf::from(percent_decode(v))),
                _ => rest.push(pair),
            }
        }
        if tls == TlsMode::Require && root_cert.is_some() {
            tls = TlsMode::VerifyFull;
        }
        let clean = if rest.is_empty() {
            base.to_owned()
        } else {
            format!("{base}?{}", rest.join("&"))
        };
        let mut config: tokio_postgres::Config = clean
            .parse()
            .map_err(|e| MetaError::Setup(format!("bad database URL: {e}")))?;
        config.ssl_mode(match tls {
            TlsMode::Disable => tokio_postgres::config::SslMode::Disable,
            TlsMode::Prefer => tokio_postgres::config::SslMode::Prefer,
            TlsMode::Require | TlsMode::VerifyFull => tokio_postgres::config::SslMode::Require,
        });
        if config.get_connect_timeout().is_none() {
            config.connect_timeout(Duration::from_secs(10));
        }
        if config.get_application_name().is_none() {
            config.application_name("loded");
        }
        Ok(Self {
            config,
            tls,
            root_cert,
            shown: hide_password(url),
        })
    }

    /// The same target with another application name, which other sessions can see.
    pub(crate) fn with_application_name(&self, name: &str) -> Self {
        let mut t = self.clone();
        t.config.application_name(name);
        t
    }

    /// Opens a connection.
    pub(crate) fn connect(&self) -> Result<PgConn> {
        let config = self.config.clone();
        let task = match self.tls {
            TlsMode::Disable => runtime()?.spawn(async move {
                let (client, conn) = config.connect(tokio_postgres::NoTls).await?;
                tokio::spawn(conn);
                Ok::<_, tokio_postgres::Error>(client)
            }),
            _ => {
                let tls = self.rustls()?;
                runtime()?.spawn(async move {
                    let (client, conn) = config.connect(tls).await?;
                    tokio::spawn(conn);
                    Ok(client)
                })
            }
        };
        let client = wait(task)
            .map_err(|e| MetaError::Setup(format!("connect task failed: {e}")))?
            .map_err(|e| {
                MetaError::Setup(format!(
                    "cannot connect to {}: {}",
                    self.shown,
                    describe(&e)
                ))
            })?;
        Ok(PgConn {
            client: Some(client),
            cache: HashMap::new(),
        })
    }

    fn rustls(&self) -> Result<tokio_postgres_rustls::MakeRustlsConnect> {
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let builder = rustls::ClientConfig::builder_with_provider(provider.clone())
            .with_safe_default_protocol_versions()
            .map_err(|e| MetaError::Setup(format!("TLS setup: {e}")))?;
        let config = if self.tls == TlsMode::VerifyFull {
            let mut roots = rustls::RootCertStore::empty();
            for cert in rustls_native_certs::load_native_certs().certs {
                let _ = roots.add(cert);
            }
            if let Some(path) = &self.root_cert {
                let pem = std::fs::read(path).map_err(|e| {
                    MetaError::Setup(format!("cannot read sslrootcert {}: {e}", path.display()))
                })?;
                for cert in rustls_pemfile_certs(&pem)? {
                    roots
                        .add(cert)
                        .map_err(|e| MetaError::Setup(format!("bad sslrootcert: {e}")))?;
                }
            }
            builder.with_root_certificates(roots).with_no_client_auth()
        } else {
            builder
                .dangerous()
                .with_custom_certificate_verifier(Arc::new(NoVerify(provider)))
                .with_no_client_auth()
        };
        Ok(tokio_postgres_rustls::MakeRustlsConnect::new(config))
    }
}

/// Certificates from PEM text.
fn rustls_pemfile_certs(pem: &[u8]) -> Result<Vec<rustls::pki_types::CertificateDer<'static>>> {
    use rustls::pki_types::pem::PemObject;
    rustls::pki_types::CertificateDer::pem_slice_iter(pem)
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| MetaError::Setup(format!("bad sslrootcert: {e}")))
}

/// Accepts any certificate: `sslmode=prefer` and `require` encrypt without authenticating
/// the server, as libpq does.
#[derive(Debug)]
struct NoVerify(Arc<rustls::crypto::CryptoProvider>);

impl rustls::client::danger::ServerCertVerifier for NoVerify {
    fn verify_server_cert(
        &self,
        _: &rustls::pki_types::CertificateDer<'_>,
        _: &[rustls::pki_types::CertificateDer<'_>],
        _: &rustls::pki_types::ServerName<'_>,
        _: &[u8],
        _: rustls::pki_types::UnixTime,
    ) -> std::result::Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 3 <= b.len() {
            let hex = std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or("");
            if let Ok(v) = u8::from_str_radix(hex, 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The URL with any password replaced by `***`.
pub(crate) fn hide_password(url: &str) -> String {
    let Some((scheme, rest)) = url.split_once("://") else {
        return url.to_owned();
    };
    let (auth, host) = match rest.rsplit_once('@') {
        Some((a, h)) => (a, h),
        None => return url.to_owned(),
    };
    let user = auth.split(':').next().unwrap_or("");
    let host = host.split('?').next().unwrap_or(host);
    format!("{scheme}://{user}:***@{host}")
}

/// One connection and its prepared statements.
pub(crate) struct PgConn {
    /// Always `Some`; an `Option` so the accessors can report a closed connection.
    client: Option<Client>,
    cache: HashMap<String, Statement>,
}

fn gone() -> MetaError {
    MetaError::Setup("the database connection is closed".into())
}

impl PgConn {
    /// Whether the connection has failed and must be replaced.
    pub(crate) fn is_closed(&self) -> bool {
        self.client.as_ref().is_none_or(Client::is_closed)
    }

    /// Starts a read transaction that sees one snapshot.
    pub(crate) fn begin_read(&mut self) -> Result<PgTx<'_>> {
        let tx = self
            .client
            .as_mut()
            .ok_or_else(gone)?
            .build_transaction()
            .isolation_level(IsolationLevel::RepeatableRead)
            .read_only(true)
            .start();
        let tx = wait(tx)?;
        Ok(PgTx {
            tx: RefCell::new(Some(tx)),
            cache: RefCell::new(&mut self.cache),
        })
    }

    /// Starts a write transaction.
    pub(crate) fn begin_write(&mut self) -> Result<PgTx<'_>> {
        let tx = wait(self.client.as_mut().ok_or_else(gone)?.transaction())?;
        Ok(PgTx {
            tx: RefCell::new(Some(tx)),
            cache: RefCell::new(&mut self.cache),
        })
    }

    /// The raw client, for statements outside a transaction.
    pub(crate) fn client(&mut self) -> Result<&mut Client> {
        self.client.as_mut().ok_or_else(gone)
    }
}

/// A transaction on a [`PgConn`].
pub(crate) struct PgTx<'c> {
    tx: RefCell<Option<Transaction<'c>>>,
    cache: RefCell<&'c mut HashMap<String, Statement>>,
}

impl PgTx<'_> {
    /// Commits.
    pub(crate) fn commit(self) -> Result<()> {
        match self.tx.into_inner() {
            Some(tx) => Ok(wait(tx.commit())?),
            None => Err(MetaError::Setup("transaction already ended".into())),
        }
    }

    fn with<T>(&self, f: impl FnOnce(&mut Transaction<'_>) -> Result<T>) -> Result<T> {
        let mut guard = self.tx.borrow_mut();
        let tx = guard
            .as_mut()
            .ok_or_else(|| MetaError::Setup("transaction already ended".into()))?;
        f(tx)
    }

    fn prepared(&self, sql: &str) -> Result<Statement> {
        if let Some(st) = self.cache.borrow().get(sql) {
            return Ok(st.clone());
        }
        let st = self.with(|tx| Ok(wait(tx.prepare(&to_postgres_params(sql)))?))?;
        let mut cache = self.cache.borrow_mut();
        if cache.len() >= STATEMENT_CACHE {
            cache.clear();
        }
        cache.insert(sql.to_owned(), st.clone());
        Ok(st)
    }
}

struct PgVal<'v, 'a>(&'v Val<'a>);

impl std::fmt::Debug for PgVal<'_, '_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

type BoxError = Box<dyn std::error::Error + Sync + Send>;

impl ToSql for PgVal<'_, '_> {
    fn to_sql(&self, ty: &Type, out: &mut BytesMut) -> std::result::Result<IsNull, BoxError> {
        match self.0 {
            Val::Null => Ok(IsNull::Yes),
            Val::Int(v) => match *ty {
                Type::INT8 => v.to_sql(ty, out),
                Type::INT4 => i32::try_from(*v)?.to_sql(ty, out),
                Type::INT2 => i16::try_from(*v)?.to_sql(ty, out),
                Type::OID => u32::try_from(*v)?.to_sql(ty, out),
                Type::BOOL => (*v != 0).to_sql(ty, out),
                Type::FLOAT8 => (*v as f64).to_sql(ty, out),
                _ => Err(format!("an integer cannot be sent as {ty}").into()),
            },
            Val::Text(t) => t.as_ref().to_sql(ty, out),
            Val::Blob(b) => b.as_ref().to_sql(ty, out),
        }
    }

    fn accepts(_: &Type) -> bool {
        true
    }

    tokio_postgres::types::to_sql_checked!();
}

fn cell(row: &tokio_postgres::Row, i: usize) -> Result<Cell> {
    let ty = row.columns()[i].type_().clone();
    let bad = |e: tokio_postgres::Error| MetaError::from(e);
    let c = match ty {
        Type::INT8 => row
            .try_get::<_, Option<i64>>(i)
            .map_err(bad)?
            .map(Cell::Int),
        Type::INT4 => row
            .try_get::<_, Option<i32>>(i)
            .map_err(bad)?
            .map(|v| Cell::Int(v.into())),
        Type::INT2 => row
            .try_get::<_, Option<i16>>(i)
            .map_err(bad)?
            .map(|v| Cell::Int(v.into())),
        Type::OID => row
            .try_get::<_, Option<u32>>(i)
            .map_err(bad)?
            .map(|v| Cell::Int(v.into())),
        Type::BOOL => row
            .try_get::<_, Option<bool>>(i)
            .map_err(bad)?
            .map(|v| Cell::Int(v.into())),
        Type::FLOAT8 => row
            .try_get::<_, Option<f64>>(i)
            .map_err(bad)?
            .map(Cell::Real),
        Type::FLOAT4 => row
            .try_get::<_, Option<f32>>(i)
            .map_err(bad)?
            .map(|v| Cell::Real(v.into())),
        Type::TEXT | Type::VARCHAR | Type::NAME | Type::BPCHAR | Type::UNKNOWN => row
            .try_get::<_, Option<String>>(i)
            .map_err(bad)?
            .map(Cell::Text),
        Type::BYTEA => row
            .try_get::<_, Option<Vec<u8>>>(i)
            .map_err(bad)?
            .map(Cell::Blob),
        other => {
            return Err(MetaError::BadValue(format!(
                "column {i} has type {other}, which Lode does not read; cast it in the query"
            )))
        }
    };
    Ok(c.unwrap_or(Cell::Null))
}

pub(crate) fn convert_rows(rows: Vec<tokio_postgres::Row>) -> Result<Vec<Row>> {
    rows.iter()
        .map(|r| {
            let cells = (0..r.len())
                .map(|i| cell(r, i))
                .collect::<Result<Vec<_>>>()?;
            Ok(Row::new(cells))
        })
        .collect()
}

fn params<'v, 'a>(vals: &'v [Val<'a>]) -> Vec<PgVal<'v, 'a>> {
    vals.iter().map(PgVal).collect()
}

fn refs<'r>(p: &'r [PgVal<'_, '_>]) -> Vec<&'r (dyn ToSql + Sync)> {
    p.iter().map(|v| v as &(dyn ToSql + Sync)).collect()
}

impl Sql for PgTx<'_> {
    fn dialect(&self) -> Dialect {
        Dialect::Postgres
    }

    fn exec(&self, sql: &str, args: &[Val<'_>]) -> Result<usize> {
        let st = self.prepared(sql)?;
        let p = params(args);
        let n = self.with(|tx| Ok(wait(tx.execute(&st, &refs(&p)))?))?;
        Ok(usize::try_from(n).unwrap_or(usize::MAX))
    }

    fn rows(&self, sql: &str, args: &[Val<'_>]) -> Result<Vec<Row>> {
        let st = self.prepared(sql)?;
        let p = params(args);
        let rows = self.with(|tx| Ok(wait(tx.query(&st, &refs(&p)))?))?;
        convert_rows(rows)
    }

    fn batch(&self, sql: &str) -> Result<()> {
        self.with(|tx| Ok(wait(tx.batch_execute(sql))?))
    }
}

/// A Postgres error with the server's message, which the driver's `Display` leaves out.
pub(crate) fn describe(e: &tokio_postgres::Error) -> String {
    match e.as_db_error() {
        Some(d) => match d.detail() {
            Some(detail) => format!("{} ({}): {detail}", d.message(), d.code().code()),
            None => format!("{} ({})", d.message(), d.code().code()),
        },
        None => match std::error::Error::source(e) {
            Some(src) => format!("{e}: {src}"),
            None => e.to_string(),
        },
    }
}

/// Whether a Postgres error is a unique-constraint violation.
pub(crate) fn is_unique(e: &tokio_postgres::Error) -> bool {
    e.code() == Some(&SqlState::UNIQUE_VIOLATION)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_parsing_reads_tls_settings_and_hides_the_password() {
        let t =
            PgTarget::parse("postgres://lode:secret@db.example.com:5433/lode?sslmode=verify-full")
                .unwrap();
        assert_eq!(t.tls, TlsMode::VerifyFull);
        assert_eq!(t.shown, "postgres://lode:***@db.example.com:5433/lode");
        let t = PgTarget::parse("postgres://lode@localhost/lode").unwrap();
        assert_eq!(t.tls, TlsMode::Prefer);
        let t = PgTarget::parse(
            "postgres://lode@localhost/lode?sslmode=require&sslrootcert=%2Fetc%2Fca.pem",
        )
        .unwrap();
        assert_eq!(t.tls, TlsMode::VerifyFull);
        assert_eq!(
            t.root_cert.as_deref(),
            Some(std::path::Path::new("/etc/ca.pem"))
        );
        assert!(PgTarget::parse("postgres://x@h/d?sslmode=sometimes").is_err());
    }
}
