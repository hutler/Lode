//! Single sign-on from a program on the user's computer (T122).
//!
//! [`Client::begin_sso`] listens on a loopback port and returns the address to open in a
//! browser. The browser goes through the server to the company's identity provider and back;
//! the server then sends it to the loopback port with a one-time code, which
//! [`SsoLogin::finish`] trades, with the PKCE verifier only this program knows, for a token.

use std::time::Duration;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use lode_proto::auth::{LoginResponse, SsoTokenRequest};
use rand::RngCore;
use reqwest::{Method, Url};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use crate::{Client, Error, Result, Retry};

/// Longest request line and headers read from the browser.
const MAX_REQUEST: usize = 16 * 1024;
const CALLBACK_PATH: &str = "/lode-sso";

/// A sign-in waiting for the browser.
pub struct SsoLogin {
    client: Client,
    listener: TcpListener,
    url: String,
    state: String,
    verifier: String,
}

impl std::fmt::Debug for SsoLogin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SsoLogin")
            .field("url", &self.url)
            .finish_non_exhaustive()
    }
}

fn random() -> String {
    let mut bytes = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

/// What the browser brought to the loopback port.
enum Arrival {
    Code(String),
    Refused(String),
    /// Something else, such as a request for an icon: answer it and keep waiting.
    Other,
}

const DONE_PAGE: &str =
    "<!doctype html><html><head><meta charset=\"utf-8\"><title>Lode sign-in</title></head>\
<body style=\"font-family:system-ui,sans-serif;max-width:36em;margin:4em auto\"><h1>Signed in</h1>\
<p>You can close this tab and go back to Lode.</p></body></html>";

fn failed_page(message: &str) -> String {
    let text = message
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;");
    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>Lode sign-in</title></head>\
         <body style=\"font-family:system-ui,sans-serif;max-width:36em;margin:4em auto\">\
         <h1>Sign-in failed</h1><p>{text}</p></body></html>"
    )
}

impl Client {
    /// Starts a single sign-on: listens on a loopback port and returns the sign-in, whose
    /// [`SsoLogin::url`] the user opens in a browser. `token_name` labels the token.
    pub async fn begin_sso(&self, token_name: Option<&str>) -> Result<SsoLogin> {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
        let port = listener.local_addr()?.port();
        let state = random();
        let verifier = random();
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        let mut url = self.url(&["auth", "sso", "start"])?;
        {
            let mut q = url.query_pairs_mut();
            q.append_pair(
                "redirect_uri",
                &format!("http://127.0.0.1:{port}{CALLBACK_PATH}"),
            )
            .append_pair("state", &state)
            .append_pair("code_challenge", &challenge)
            .append_pair("code_challenge_method", "S256");
            if let Some(name) = token_name {
                q.append_pair("token_name", name);
            }
        }
        Ok(SsoLogin {
            client: self.clone(),
            listener,
            url: url.to_string(),
            state,
            verifier,
        })
    }
}

impl SsoLogin {
    /// The address to open in a browser.
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Waits up to `timeout` for the browser to come back, then trades the one-time code
    /// for a token.
    pub async fn finish(self, timeout: Duration) -> Result<LoginResponse> {
        let code = tokio::time::timeout(timeout, self.wait())
            .await
            .map_err(|_| {
                Error::SignIn(format!(
                    "the sign-in in the browser was not finished within {} minutes",
                    timeout.as_secs().div_ceil(60)
                ))
            })??;
        let url = self.client.url(&["auth", "sso", "token"])?;
        self.client
            .json_body(
                Retry::No,
                Method::POST,
                url,
                &SsoTokenRequest {
                    code,
                    code_verifier: self.verifier.clone(),
                },
            )
            .await
    }

    async fn wait(&self) -> Result<String> {
        loop {
            let (mut stream, _) = self.listener.accept().await?;
            let arrival = match read_request(&mut stream).await {
                Ok(target) => self.arrival(&target),
                Err(_) => Arrival::Other,
            };
            match arrival {
                Arrival::Code(code) => {
                    respond(&mut stream, "200 OK", DONE_PAGE).await;
                    return Ok(code);
                }
                Arrival::Refused(message) => {
                    respond(&mut stream, "200 OK", &failed_page(&message)).await;
                    return Err(Error::SignIn(message));
                }
                Arrival::Other => respond(&mut stream, "404 Not Found", "").await,
            }
        }
    }

    fn arrival(&self, target: &str) -> Arrival {
        let Ok(url) = Url::parse(&format!("http://127.0.0.1{target}")) else {
            return Arrival::Other;
        };
        if url.path() != CALLBACK_PATH {
            return Arrival::Other;
        }
        let get = |name: &str| {
            url.query_pairs()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.into_owned())
        };
        if get("state").as_deref() != Some(self.state.as_str()) {
            // Not this sign-in's answer: ignore it rather than fail.
            return Arrival::Other;
        }
        if let Some(code) = get("code").filter(|c| !c.is_empty()) {
            return Arrival::Code(code);
        }
        let message = get("error_description")
            .or_else(|| get("error"))
            .unwrap_or_else(|| "the server sent no sign-in code".to_owned());
        Arrival::Refused(message)
    }
}

/// Reads the request head and returns its target, such as `/lode-sso?code=…`.
async fn read_request(stream: &mut TcpStream) -> std::io::Result<String> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 2048];
    loop {
        let n = tokio::time::timeout(Duration::from_secs(10), stream.read(&mut chunk))
            .await
            .map_err(|_| std::io::Error::from(std::io::ErrorKind::TimedOut))??;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
        if buf.windows(4).any(|w| w == b"\r\n\r\n") || buf.len() > MAX_REQUEST {
            break;
        }
    }
    let head = String::from_utf8_lossy(&buf);
    let line = head.lines().next().unwrap_or("");
    let mut parts = line.split(' ');
    match (parts.next(), parts.next()) {
        (Some("GET"), Some(target)) if target.starts_with('/') => Ok(target.to_owned()),
        _ => Err(std::io::Error::from(std::io::ErrorKind::InvalidData)),
    }
}

async fn respond(stream: &mut TcpStream, status: &str, body: &str) {
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\n\
         Cache-Control: no-store\r\nConnection: close\r\n\r\n",
        body.len()
    );
    // The browser may already be gone; the sign-in result does not depend on this.
    let _ = stream.write_all(head.as_bytes()).await;
    let _ = stream.write_all(body.as_bytes()).await;
    let _ = stream.shutdown().await;
}
