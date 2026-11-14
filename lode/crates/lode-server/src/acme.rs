//! Built-in HTTPS certificates from an ACME server such as Let's Encrypt (T081).
//!
//! The certificate is obtained and renewed over the TLS-ALPN-01 challenge on the HTTPS port
//! itself, so no other port and no web server are needed. While a challenge is open, a TLS
//! client that offers only the `acme-tls/1` protocol gets the challenge certificate; everyone
//! else gets the real one.
//!
//! Files live in `<data_dir>/acme/<directory id>/`, where the id is derived from the ACME
//! directory URL, so a certificate from a staging server is never served after switching to
//! production: `account.json` (the ACME account), and `<domain>.crt.pem` and
//! `<domain>.key.pem`. A restart serves the cached certificate at once.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use instant_acme::{
    Account, AccountCredentials, AuthorizationStatus, ChallengeType, Identifier, NewAccount,
    NewOrder, OrderStatus, RetryPolicy,
};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::server::{ClientHello, ResolvesServerCert};
use rustls::sign::CertifiedKey;

use crate::config::AcmeConfig;

/// The ALPN protocol of the TLS-ALPN-01 challenge (RFC 8737).
pub(crate) const ACME_TLS_ALPN: &[u8] = b"acme-tls/1";

/// First wait after a failed attempt; doubled for each further failure.
const RETRY_FIRST: Duration = Duration::from_secs(5 * 60);
/// Longest wait between attempts after failures.
const RETRY_MAX: Duration = Duration::from_secs(6 * 60 * 60);
/// Longest sleep between checks while the certificate is good, so a changed clock or a
/// certificate replaced on disk is noticed within a day.
const CHECK_MAX: Duration = Duration::from_secs(12 * 60 * 60);

/// Errors obtaining or loading a certificate.
#[derive(Debug, thiserror::Error)]
pub enum AcmeError {
    /// The ACME server or the protocol failed.
    #[error("ACME: {0}")]
    Protocol(#[from] instant_acme::Error),
    /// The order did not become ready, typically because the challenge failed.
    #[error("the ACME order for {domain} ended as {status:?}; check that {domain} resolves to this server and that port 443 reaches it")]
    OrderFailed {
        /// The domain.
        domain: String,
        /// Final order status.
        status: OrderStatus,
    },
    /// The authorization was neither pending nor valid.
    #[error("the ACME authorization for {0} is not pending (status {1:?})")]
    Authorization(String, AuthorizationStatus),
    /// The ACME server offers no TLS-ALPN-01 challenge.
    #[error("the ACME server offers no tls-alpn-01 challenge for {0}")]
    NoChallenge(String),
    /// Building the challenge certificate failed.
    #[error("cannot build the challenge certificate: {0}")]
    ChallengeCert(String),
    /// A certificate or key could not be read or used.
    #[error("certificate for {domain}: {message}")]
    Certificate {
        /// The domain.
        domain: String,
        /// What is wrong.
        message: String,
    },
    /// Reading or writing the cache failed.
    #[error("{context}: {source}")]
    Io {
        /// What was being done.
        context: String,
        /// Cause.
        source: std::io::Error,
    },
}

fn io_err(context: String) -> impl FnOnce(std::io::Error) -> AcmeError {
    move |source| AcmeError::Io { context, source }
}

/// A served certificate and when to renew it (Unix seconds).
#[derive(Clone)]
struct Current {
    key: Arc<CertifiedKey>,
    renew_at: i64,
}

/// Picks the certificate for each TLS handshake.
#[derive(Default)]
pub(crate) struct CertResolver {
    current: RwLock<Option<Current>>,
    challenges: Mutex<HashMap<String, Arc<CertifiedKey>>>,
}

impl std::fmt::Debug for CertResolver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CertResolver").finish_non_exhaustive()
    }
}

impl ResolvesServerCert for CertResolver {
    fn resolve(&self, hello: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        let challenge = hello
            .alpn()
            .is_some_and(|mut protocols| protocols.any(|p| p == ACME_TLS_ALPN));
        if challenge {
            let name = hello.server_name()?.to_ascii_lowercase();
            let challenges = self.challenges.lock().unwrap_or_else(|e| e.into_inner());
            return challenges.get(&name).cloned();
        }
        let current = self.current.read().unwrap_or_else(|e| e.into_inner());
        current.as_ref().map(|c| Arc::clone(&c.key))
    }
}

impl CertResolver {
    fn set_current(&self, current: Current) {
        *self.current.write().unwrap_or_else(|e| e.into_inner()) = Some(current);
    }

    fn renew_at(&self) -> Option<i64> {
        let current = self.current.read().unwrap_or_else(|e| e.into_inner());
        current.as_ref().map(|c| c.renew_at)
    }

    fn set_challenge(&self, domain: &str, key: Option<Arc<CertifiedKey>>) {
        let mut challenges = self.challenges.lock().unwrap_or_else(|e| e.into_inner());
        match key {
            Some(key) => {
                challenges.insert(domain.to_owned(), key);
            }
            None => {
                challenges.remove(domain);
            }
        }
    }
}

/// Obtains and renews the certificate for one domain.
pub(crate) struct Acme {
    config: AcmeConfig,
    dir: PathBuf,
    resolver: Arc<CertResolver>,
}

impl Acme {
    /// Prepares the cache directory and loads a cached certificate, if there is a usable one.
    pub(crate) fn new(config: AcmeConfig, data_dir: &Path) -> Result<Self, AcmeError> {
        let id = blake3::hash(config.directory.as_bytes()).to_hex();
        let dir = data_dir.join("acme").join(&id.as_str()[..16]);
        std::fs::create_dir_all(&dir)
            .map_err(io_err(format!("cannot create {}", dir.display())))?;
        let acme = Self {
            config,
            dir,
            resolver: Arc::default(),
        };
        match acme.load_cached() {
            Ok(Some(current)) => {
                tracing::info!(domain = %acme.config.domain, "using the cached certificate");
                acme.resolver.set_current(current);
            }
            Ok(None) => {}
            Err(e) => tracing::warn!(error = %e, "ignoring the cached certificate"),
        }
        Ok(acme)
    }

    /// The certificate resolver for the TLS configuration.
    pub(crate) fn resolver(&self) -> Arc<CertResolver> {
        Arc::clone(&self.resolver)
    }

    fn cert_path(&self) -> PathBuf {
        self.dir.join(format!("{}.crt.pem", self.config.domain))
    }

    fn key_path(&self) -> PathBuf {
        self.dir.join(format!("{}.key.pem", self.config.domain))
    }

    fn load_cached(&self) -> Result<Option<Current>, AcmeError> {
        let (cert, key) = (self.cert_path(), self.key_path());
        if !cert.exists() || !key.exists() {
            return Ok(None);
        }
        let cert_pem =
            std::fs::read(&cert).map_err(io_err(format!("cannot read {}", cert.display())))?;
        let key_pem =
            std::fs::read(&key).map_err(io_err(format!("cannot read {}", key.display())))?;
        self.certified(&cert_pem, &key_pem).map(Some)
    }

    fn certified(&self, cert_pem: &[u8], key_pem: &[u8]) -> Result<Current, AcmeError> {
        let bad = |message: String| AcmeError::Certificate {
            domain: self.config.domain.clone(),
            message,
        };
        let chain: Vec<CertificateDer<'static>> = CertificateDer::pem_slice_iter(cert_pem)
            .collect::<Result<_, _>>()
            .map_err(|e| bad(e.to_string()))?;
        let leaf = chain.first().ok_or_else(|| bad("no certificate".into()))?;
        let (not_before, not_after) =
            validity(leaf).ok_or_else(|| bad("cannot read the validity period".into()))?;
        let key = PrivateKeyDer::from_pem_slice(key_pem).map_err(|e| bad(e.to_string()))?;
        let signer =
            rustls::crypto::ring::sign::any_supported_type(&key).map_err(|e| bad(e.to_string()))?;
        Ok(Current {
            key: Arc::new(CertifiedKey::new(chain, signer)),
            renew_at: renew_at(not_before, not_after),
        })
    }

    /// Keeps the certificate current until the task is dropped.
    pub(crate) async fn maintain(self) {
        let mut failures = 0u32;
        loop {
            let wait = match self.renew_if_due().await {
                Ok(wait) => {
                    failures = 0;
                    wait
                }
                Err(e) => {
                    failures = failures.saturating_add(1);
                    let wait = retry_delay(failures);
                    tracing::error!(
                        domain = %self.config.domain,
                        error = %e,
                        retry_in_seconds = wait.as_secs(),
                        "cannot obtain a certificate"
                    );
                    wait
                }
            };
            tokio::time::sleep(wait).await;
        }
    }

    /// Obtains a certificate if there is none or it is due; returns how long to wait before
    /// the next check.
    async fn renew_if_due(&self) -> Result<Duration, AcmeError> {
        let now = time::OffsetDateTime::now_utc().unix_timestamp();
        let due = match self.resolver.renew_at() {
            Some(at) if at > now => at,
            _ => {
                self.obtain().await?;
                self.resolver.renew_at().unwrap_or(now)
            }
        };
        let secs = u64::try_from(due.saturating_sub(now)).unwrap_or(0);
        Ok(Duration::from_secs(secs).clamp(Duration::from_secs(60), CHECK_MAX))
    }

    async fn account(&self) -> Result<Account, AcmeError> {
        let builder = match &self.config.ca {
            Some(ca) => Account::builder_with_root(ca)?,
            None => Account::builder()?,
        };
        let path = self.dir.join("account.json");
        if let Ok(text) = std::fs::read(&path) {
            match serde_json::from_slice::<AccountCredentials>(&text) {
                Ok(credentials) => return Ok(builder.from_credentials(credentials).await?),
                Err(e) => tracing::warn!(error = %e, "ignoring an unreadable ACME account file"),
            }
        }
        let contact: Vec<String> = self
            .config
            .email
            .iter()
            .map(|e| format!("mailto:{e}"))
            .collect();
        let contact: Vec<&str> = contact.iter().map(String::as_str).collect();
        let (account, credentials) = builder
            .create(
                &NewAccount {
                    contact: &contact,
                    terms_of_service_agreed: true,
                    only_return_existing: false,
                },
                self.config.directory.clone(),
                None,
            )
            .await?;
        let json = serde_json::to_vec_pretty(&credentials).map_err(|e| AcmeError::Io {
            context: "cannot encode the ACME account".into(),
            source: std::io::Error::other(e),
        })?;
        write_private(&path, &json)?;
        tracing::info!(directory = %self.config.directory, "created an ACME account");
        Ok(account)
    }

    async fn obtain(&self) -> Result<(), AcmeError> {
        let domain = self.config.domain.clone();
        tracing::info!(%domain, directory = %self.config.directory, "requesting a certificate");
        let account = self.account().await?;
        let identifiers = [Identifier::Dns(domain.clone())];
        let mut order = account.new_order(&NewOrder::new(&identifiers)).await?;
        let mut authorizations = order.authorizations();
        while let Some(authz) = authorizations.next().await {
            let mut authz = authz?;
            match authz.status {
                AuthorizationStatus::Pending => {}
                AuthorizationStatus::Valid => continue,
                other => return Err(AcmeError::Authorization(domain.clone(), other)),
            }
            let mut challenge = authz
                .challenge(ChallengeType::TlsAlpn01)
                .ok_or_else(|| AcmeError::NoChallenge(domain.clone()))?;
            let digest = challenge.key_authorization().digest();
            let cert = challenge_cert(&domain, digest.as_ref())?;
            self.resolver.set_challenge(&domain, Some(cert));
            challenge.set_ready().await?;
        }
        let retries = RetryPolicy::new().timeout(Duration::from_secs(120));
        let status = order.poll_ready(&retries).await;
        self.resolver.set_challenge(&domain, None);
        let status = status?;
        if status != OrderStatus::Ready {
            return Err(AcmeError::OrderFailed { domain, status });
        }
        let key_pem = order.finalize().await?;
        let cert_pem = order.poll_certificate(&retries).await?;
        let current = self.certified(cert_pem.as_bytes(), key_pem.as_bytes())?;
        write_private(&self.key_path(), key_pem.as_bytes())?;
        write_private(&self.cert_path(), cert_pem.as_bytes())?;
        self.resolver.set_current(current);
        tracing::info!(%domain, "obtained a certificate");
        Ok(())
    }
}

/// A self-signed certificate for `domain` carrying the challenge digest (RFC 8737 §3).
fn challenge_cert(domain: &str, digest: &[u8]) -> Result<Arc<CertifiedKey>, AcmeError> {
    let fail = |e: &dyn std::fmt::Display| AcmeError::ChallengeCert(e.to_string());
    if digest.len() != 32 {
        return Err(AcmeError::ChallengeCert("the digest is not SHA-256".into()));
    }
    let mut params =
        rcgen::CertificateParams::new(vec![domain.to_owned()]).map_err(|e| fail(&e))?;
    params.custom_extensions = vec![rcgen::CustomExtension::new_acme_identifier(digest)];
    let key = rcgen::KeyPair::generate().map_err(|e| fail(&e))?;
    let cert = params.self_signed(&key).map_err(|e| fail(&e))?;
    let key_der = PrivateKeyDer::try_from(key.serialize_der()).map_err(|e| fail(&e))?;
    let signer = rustls::crypto::ring::sign::any_supported_type(&key_der).map_err(|e| fail(&e))?;
    Ok(Arc::new(CertifiedKey::new(
        vec![cert.der().clone()],
        signer,
    )))
}

/// Writes a file through a temporary name, readable only by its owner where that applies.
fn write_private(path: &Path, data: &[u8]) -> Result<(), AcmeError> {
    use std::io::Write;
    let tmp = path.with_extension("tmp");
    let context = || format!("cannot write {}", path.display());
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&tmp).map_err(io_err(context()))?;
    file.write_all(data).map_err(io_err(context()))?;
    file.sync_all().map_err(io_err(context()))?;
    drop(file);
    std::fs::rename(&tmp, path).map_err(io_err(context()))
}

/// When to renew a certificate valid from `not_before` to `not_after`: once two thirds of
/// its lifetime have passed (60 of 90 days for Let's Encrypt).
fn renew_at(not_before: i64, not_after: i64) -> i64 {
    not_after - (not_after - not_before).max(0) / 3
}

fn retry_delay(failures: u32) -> Duration {
    let factor = 1u32 << failures.saturating_sub(1).min(16);
    RETRY_FIRST.saturating_mul(factor).min(RETRY_MAX)
}

/// When the first certificate in a PEM file expires, as Unix seconds (T140: shown on the
/// admin overview).
pub(crate) fn pem_expiry(pem: &[u8]) -> Option<i64> {
    let leaf = CertificateDer::pem_slice_iter(pem).next()?.ok()?;
    validity(&leaf).map(|(_, not_after)| not_after)
}

/// Where the certificate obtained for `config` is cached under `data_dir`.
pub(crate) fn cached_cert_path(config: &AcmeConfig, data_dir: &Path) -> PathBuf {
    let id = blake3::hash(config.directory.as_bytes()).to_hex();
    data_dir
        .join("acme")
        .join(&id.as_str()[..16])
        .join(format!("{}.crt.pem", config.domain))
}

/// The validity period of a DER certificate, as Unix seconds.
fn validity(der: &[u8]) -> Option<(i64, i64)> {
    let (tag, cert, _) = tlv(der)?;
    if tag != 0x30 {
        return None;
    }
    let (tag, tbs, _) = tlv(cert)?;
    if tag != 0x30 {
        return None;
    }
    let mut rest = tbs;
    let (tag, _, after) = tlv(rest)?;
    if tag == 0xa0 {
        rest = after; // explicit version
    }
    // serialNumber, signature, issuer
    for expected in [0x02, 0x30, 0x30] {
        let (tag, _, after) = tlv(rest)?;
        if tag != expected {
            return None;
        }
        rest = after;
    }
    let (tag, validity, _) = tlv(rest)?;
    if tag != 0x30 {
        return None;
    }
    let (tag1, t1, after) = tlv(validity)?;
    let (tag2, t2, _) = tlv(after)?;
    Some((der_time(tag1, t1)?, der_time(tag2, t2)?))
}

/// One DER tag-length-value: the tag, the contents and what follows.
fn tlv(input: &[u8]) -> Option<(u8, &[u8], &[u8])> {
    let (&tag, rest) = input.split_first()?;
    let (&first, mut rest) = rest.split_first()?;
    let len = if first < 0x80 {
        usize::from(first)
    } else {
        let n = usize::from(first & 0x7f);
        if n == 0 || n > 4 || rest.len() < n {
            return None;
        }
        let (bytes, after) = rest.split_at(n);
        rest = after;
        bytes
            .iter()
            .fold(0usize, |acc, b| (acc << 8) | usize::from(*b))
    };
    if rest.len() < len {
        return None;
    }
    let (content, after) = rest.split_at(len);
    Some((tag, content, after))
}

/// UTCTime (`YYMMDDHHMMSSZ`) or GeneralizedTime (`YYYYMMDDHHMMSSZ`) as Unix seconds.
fn der_time(tag: u8, bytes: &[u8]) -> Option<i64> {
    let text = std::str::from_utf8(bytes).ok()?.strip_suffix('Z')?;
    let (year, rest) = match (tag, text.len()) {
        (0x17, 12) => {
            let yy: i32 = text.get(..2)?.parse().ok()?;
            (if yy >= 50 { 1900 + yy } else { 2000 + yy }, text.get(2..)?)
        }
        (0x18, 14) => (text.get(..4)?.parse().ok()?, text.get(4..)?),
        _ => return None,
    };
    let num = |r: std::ops::Range<usize>| -> Option<u8> { rest.get(r)?.parse().ok() };
    let month = time::Month::try_from(num(0..2)?).ok()?;
    let date = time::Date::from_calendar_date(year, month, num(2..4)?).ok()?;
    let clock = time::Time::from_hms(num(4..6)?, num(6..8)?, num(8..10)?).ok()?;
    Some(
        time::PrimitiveDateTime::new(date, clock)
            .assume_utc()
            .unix_timestamp(),
    )
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn cert_valid(from: (i32, u8, u8), to: (i32, u8, u8)) -> Vec<u8> {
        let mut params = rcgen::CertificateParams::new(vec!["lode.example.com".into()]).unwrap();
        params.not_before = rcgen::date_time_ymd(from.0, from.1, from.2);
        params.not_after = rcgen::date_time_ymd(to.0, to.1, to.2);
        let key = rcgen::KeyPair::generate().unwrap();
        params.self_signed(&key).unwrap().der().to_vec()
    }

    #[test]
    fn reads_the_validity_period() {
        // UTCTime before 2050, GeneralizedTime from 2050 on.
        let (from, to) = validity(&cert_valid((2026, 11, 1), (2027, 1, 30))).unwrap();
        assert_eq!(from, 1_793_491_200);
        assert_eq!(to, 1_801_267_200);
        let (_, to) = validity(&cert_valid((2049, 1, 1), (2051, 6, 2))).unwrap();
        assert_eq!(to, 2_569_276_800);
        assert!(validity(b"\x30\x03\x02\x01\x01").is_none());
        assert!(validity(&[]).is_none());
    }

    #[test]
    fn renews_after_two_thirds_and_backs_off() {
        let day = 86_400;
        assert_eq!(renew_at(0, 90 * day), 60 * day);
        assert_eq!(retry_delay(1), RETRY_FIRST);
        assert_eq!(retry_delay(2), RETRY_FIRST * 2);
        assert_eq!(retry_delay(40), RETRY_MAX);
    }

    #[test]
    fn challenge_certificate_carries_the_digest() {
        let cert = challenge_cert("lode.example.com", &[7u8; 32]).unwrap();
        let der = cert.cert[0].as_ref();
        // id-pe-acmeIdentifier (1.3.6.1.5.5.7.1.31), critical, OCTET STRING of the digest.
        let oid = [0x06, 0x08, 0x2b, 0x06, 0x01, 0x05, 0x05, 0x07, 0x01, 0x1f];
        let at = der.windows(oid.len()).position(|w| w == oid).unwrap();
        let mut expected = vec![0x01, 0x01, 0xff, 0x04, 0x22, 0x04, 0x20];
        expected.extend([7u8; 32]);
        assert_eq!(
            &der[at + oid.len()..at + oid.len() + expected.len()],
            &expected[..]
        );
        assert!(challenge_cert("lode.example.com", &[1, 2, 3]).is_err());
    }

    #[test]
    fn caches_by_directory_and_reloads() {
        let dir = tempfile::tempdir().unwrap();
        let config = |directory: &str| AcmeConfig {
            domain: "lode.example.com".into(),
            email: None,
            directory: directory.into(),
            ca: None,
        };
        let a = Acme::new(config("https://a.example/dir"), dir.path()).unwrap();
        assert!(a.resolver.renew_at().is_none());
        let key = rcgen::KeyPair::generate().unwrap();
        let params = rcgen::CertificateParams::new(vec!["lode.example.com".into()]).unwrap();
        let cert = params.self_signed(&key).unwrap();
        write_private(&a.cert_path(), cert.pem().as_bytes()).unwrap();
        write_private(&a.key_path(), key.serialize_pem().as_bytes()).unwrap();

        let again = Acme::new(config("https://a.example/dir"), dir.path()).unwrap();
        assert!(again.resolver.renew_at().is_some());
        let other = Acme::new(config("https://b.example/dir"), dir.path()).unwrap();
        assert!(other.resolver.renew_at().is_none());
    }
}
