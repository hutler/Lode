//! One copy of the app per user (T135). The first copy holds a lock file in the app's data
//! directory and listens on a loopback socket whose port and token it writes next to the lock.
//! A later start finds the lock taken, hands its `lode://` link (or nothing, to bring the
//! window forward) to the first copy over that socket, and exits.
//!
//! The token keeps other users of the computer from sending links: it sits in the user's own
//! data directory, and a connection that does not give it is closed unanswered. Links that
//! arrive are parsed as strictly as any other (see [`crate::links`]).

use std::fs::File;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// The lock file's name in the data directory.
const LOCK: &str = "instance.lock";
/// Where the first copy listens.
const ADDRESS: &str = "instance.json";
/// How long a later start keeps trying to reach the first copy.
const HAND_OVER_WAIT: Duration = Duration::from_secs(5);
/// How long either side waits for the other on an open connection.
const IO_WAIT: Duration = Duration::from_secs(3);
/// Longest message accepted: a link (at most [`crate::links::MAX_LINK`]) and the token.
const MAX_MESSAGE: u64 = 8 * 1024;

#[derive(Debug, Serialize, Deserialize)]
struct Address {
    port: u16,
    token: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct Message {
    token: String,
    link: Option<String>,
}

/// How this start goes on.
#[derive(Debug)]
pub enum Start {
    /// This is the first copy: it opens the window and takes links from later starts.
    First(Instance),
    /// Another copy runs and took the link; this one exits.
    HandedOver,
}

/// The first copy's lock and socket. The lock is held while this value lives.
#[derive(Debug)]
pub struct Instance {
    _lock: File,
    listener: TcpListener,
    token: String,
}

fn random_token() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 24];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Becomes the first copy, or hands `link` to the one that runs. An error when another copy
/// holds the lock but does not answer (it is stopping, or hangs): the caller then starts a
/// window that takes no links.
pub fn claim(data_dir: &Path, link: Option<&str>) -> std::io::Result<Start> {
    std::fs::create_dir_all(data_dir)?;
    let lock = File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(data_dir.join(LOCK))?;
    match lock.try_lock() {
        Ok(()) => Ok(Start::First(listen(lock, data_dir)?)),
        Err(std::fs::TryLockError::WouldBlock) => {
            if hand_over(data_dir, link, HAND_OVER_WAIT) {
                Ok(Start::HandedOver)
            } else {
                Err(std::io::Error::other(
                    "another Lode Desk is running but does not answer",
                ))
            }
        }
        Err(std::fs::TryLockError::Error(e)) => Err(e),
    }
}

fn listen(lock: File, data_dir: &Path) -> std::io::Result<Instance> {
    let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))?;
    let token = random_token();
    let address = Address {
        port: listener.local_addr()?.port(),
        token: token.clone(),
    };
    let file = data_dir.join(ADDRESS);
    let tmp = data_dir.join(format!("{ADDRESS}.tmp"));
    std::fs::write(&tmp, serde_json::to_vec(&address)?)?;
    restrict(&tmp);
    std::fs::rename(&tmp, &file)?;
    Ok(Instance {
        _lock: lock,
        listener,
        token,
    })
}

/// Only the user may read the token.
fn restrict(file: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o600));
    }
    #[cfg(not(unix))]
    let _ = file;
}

/// Gives `link` to the copy that runs, trying for up to `wait`. True when it took it.
pub fn hand_over(data_dir: &Path, link: Option<&str>, wait: Duration) -> bool {
    let started = Instant::now();
    loop {
        if try_hand_over(&data_dir.join(ADDRESS), link).is_ok() {
            return true;
        }
        if started.elapsed() >= wait {
            return false;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn try_hand_over(file: &PathBuf, link: Option<&str>) -> std::io::Result<()> {
    let address: Address = serde_json::from_slice(&std::fs::read(file)?)?;
    let mut stream = TcpStream::connect_timeout(
        &SocketAddr::from((Ipv4Addr::LOCALHOST, address.port)),
        IO_WAIT,
    )?;
    stream.set_read_timeout(Some(IO_WAIT))?;
    stream.set_write_timeout(Some(IO_WAIT))?;
    let mut line = serde_json::to_vec(&Message {
        token: address.token,
        link: link.map(str::to_owned),
    })?;
    line.push(b'\n');
    stream.write_all(&line)?;
    let mut answer = String::new();
    BufReader::new(stream).read_line(&mut answer)?;
    if answer.trim() == "ok" {
        Ok(())
    } else {
        Err(std::io::Error::other(
            "the running copy did not take the link",
        ))
    }
}

impl Instance {
    /// Takes links from later starts on a thread of its own, calling `on_link` with each (or
    /// with `None` when a start had no link and only wants the window brought forward).
    pub fn serve(self, on_link: impl Fn(Option<String>) + Send + 'static) {
        let Instance {
            _lock,
            listener,
            token,
        } = self;
        let spawned = std::thread::Builder::new()
            .name("lode-desk-links".into())
            .spawn(move || {
                // The lock lives as long as the thread, which lives as long as the app.
                let _lock = _lock;
                for stream in listener.incoming() {
                    let Ok(stream) = stream else { continue };
                    if let Some(link) = read_message(stream, &token) {
                        on_link(link);
                    }
                }
            });
        // Without the thread, later starts find no answer and start on their own.
        drop(spawned);
    }
}

/// The link of one connection, after checking its token and answering it.
fn read_message(stream: TcpStream, token: &str) -> Option<Option<String>> {
    stream.set_read_timeout(Some(IO_WAIT)).ok()?;
    stream.set_write_timeout(Some(IO_WAIT)).ok()?;
    let mut line = String::new();
    BufReader::new((&stream).take(MAX_MESSAGE))
        .read_line(&mut line)
        .ok()?;
    let message: Message = serde_json::from_str(line.trim()).ok()?;
    if !same(&message.token, token) {
        return None;
    }
    (&stream).write_all(b"ok\n").ok()?;
    Some(message.link)
}

/// Compares tokens without stopping at the first difference.
fn same(a: &str, b: &str) -> bool {
    a.len() == b.len()
        && a.bytes()
            .zip(b.bytes())
            .fold(0u8, |acc, (x, y)| acc | (x ^ y))
            == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_compare_in_full() {
        assert!(same("abc", "abc"));
        assert!(!same("abc", "abd"));
        assert!(!same("abc", "ab"));
        assert_eq!(random_token().len(), 48);
        assert_ne!(random_token(), random_token());
    }
}
