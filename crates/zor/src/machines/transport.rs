//! How a machine is reached. `Direct` is loopback/LAN TCP to a remote fux/zor BRP endpoint (the
//! descriptor contents travel in the private catalog). `Koh` follows the gateway contract
//! (`references/koh/GATEWAY-CONTRACT.md`): an owned local `koh gateway connect` helper forwards
//! the authenticated QUIC session. The published koh forwards Unix sockets, while fux and zor
//! listen on TCP, so the Koh path probes `koh gateway connect --help` once per helper and
//! reports `unavailable` with the reason until the helper advertises TCP forwarding. The
//! pinned reference checkout is never modified.

use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use bevy_platform::collections::HashMap;
use serde::{Deserialize, Serialize};

pub use crate::remote::Descriptor;

pub const DEFAULT_HELPER: &str = "koh";
/// The contract caps a connection/admission handshake at ten seconds.
pub const HELPER_READY: Duration = Duration::from_secs(10);
/// Flags a TCP-capable helper might advertise; the first one found in `--help` is used.
const TCP_FLAGS: [&str; 3] = ["--tcp", "--listen-tcp", "--listen"];

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Transport {
    /// TCP to `host:port`; `brp` holds the remote service's descriptor contents (token,
    /// instance nonce, attachment endpoint). The descriptor's own `http` is replaced by
    /// `host:port` and its attachment host by `host`.
    Direct {
        host: String,
        port: u16,
        brp: Descriptor,
    },
    /// An owned `koh gateway connect` helper; `brp` as for `Direct` (the remote's descriptor is
    /// still needed for the token and instance nonce).
    Koh {
        #[serde(default = "default_helper")]
        helper: String,
        endpoint: String,
        key_file: String,
        #[serde(default)]
        direct: Option<String>,
        #[serde(default)]
        relay_url: Option<String>,
        brp: Descriptor,
    },
}

fn default_helper() -> String {
    DEFAULT_HELPER.to_owned()
}

/// The transport cannot be used now; the reason is shown as the machine's problem.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unavailable(pub String);

impl core::fmt::Display for Unavailable {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "unavailable: {}", self.0)
    }
}

impl core::error::Error for Unavailable {}

/// A usable endpoint: the descriptor to call and, for Koh, the owned helper.
#[derive(Debug)]
pub struct Resolved {
    pub descriptor: Descriptor,
    helper: Option<Child>,
}

impl Drop for Resolved {
    fn drop(&mut self) {
        if let Some(mut child) = self.helper.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl Transport {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Direct { .. } => "direct",
            Self::Koh { .. } => "koh",
        }
    }

    /// A short address for listings: `host:port` or `koh:ENDPOINT`.
    pub fn address(&self) -> String {
        match self {
            Self::Direct { host, port, .. } => format!("{host}:{port}"),
            Self::Koh { endpoint, .. } => format!("koh:{endpoint}"),
        }
    }

    pub fn descriptor(&self) -> &Descriptor {
        match self {
            Self::Direct { brp, .. } | Self::Koh { brp, .. } => brp,
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        match self {
            Self::Direct { host, port, brp } => {
                if host.is_empty() {
                    return Err("empty host".into());
                }
                if *port == 0 {
                    return Err("port 0".into());
                }
                descriptor_ok(brp)
            }
            Self::Koh {
                helper,
                endpoint,
                key_file,
                direct,
                relay_url,
                brp,
            } => {
                if helper.is_empty() || endpoint.is_empty() {
                    return Err("empty helper or endpoint".into());
                }
                if !Path::new(key_file).is_absolute() {
                    return Err(format!("key_file {key_file:?} is not absolute"));
                }
                if direct.is_some() && relay_url.is_some() {
                    return Err("`direct` and `relay_url` are mutually exclusive".into());
                }
                descriptor_ok(brp)
            }
        }
    }

    /// Resolves to a callable descriptor. `Direct` never fails; `Koh` needs a TCP-capable
    /// helper.
    pub fn resolve(&self) -> Result<Resolved, Unavailable> {
        match self {
            Self::Direct { host, port, brp } => Ok(Resolved {
                descriptor: rewrite(brp, host, *port),
                helper: None,
            }),
            Self::Koh {
                helper,
                endpoint,
                key_file,
                direct,
                relay_url,
                brp,
            } => {
                let flag = probe_helper(helper)?;
                let port = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
                    .and_then(|l| l.local_addr().map(|a| a.port()))
                    .map_err(|e| Unavailable(format!("no free loopback port: {e}")))?;
                let mut command = Command::new(helper);
                command
                    .arg("gateway")
                    .arg("connect")
                    .arg(endpoint)
                    .arg("--key-file")
                    .arg(key_file)
                    .arg(flag)
                    .arg(format!("127.0.0.1:{port}"))
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null());
                if let Some(direct) = direct {
                    command.arg("--direct").arg(direct);
                }
                if let Some(relay) = relay_url {
                    command.arg("--relay-url").arg(relay);
                }
                let mut child = command
                    .spawn()
                    .map_err(|e| Unavailable(format!("cannot start {helper}: {e}")))?;
                let deadline = Instant::now() + HELPER_READY;
                loop {
                    if TcpStream::connect((Ipv4Addr::LOCALHOST, port)).is_ok() {
                        break;
                    }
                    if let Ok(Some(status)) = child.try_wait() {
                        return Err(Unavailable(format!("{helper} exited: {status}")));
                    }
                    if Instant::now() > deadline {
                        let _ = child.kill();
                        let _ = child.wait();
                        return Err(Unavailable(format!(
                            "{helper} did not open its TCP forward within {HELPER_READY:?}"
                        )));
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
                Ok(Resolved {
                    descriptor: rewrite(brp, "127.0.0.1", port),
                    helper: Some(child),
                })
            }
        }
    }
}

fn descriptor_ok(brp: &Descriptor) -> Result<(), String> {
    if brp.token.is_empty() {
        return Err("descriptor without a token".into());
    }
    if brp.instance.is_empty() {
        return Err("descriptor without an instance nonce".into());
    }
    Ok(())
}

/// The descriptor as seen from here: `http` and the attachment host replaced by the address.
pub fn rewrite(brp: &Descriptor, host: &str, port: u16) -> Descriptor {
    let mut out = brp.clone();
    out.http.host = host.to_owned();
    out.http.port = port;
    if let Some(attach) = &mut out.attach {
        attach.host = host.to_owned();
    }
    out
}

/// `koh gateway connect --help`, once per helper executable: the TCP flag it advertises, or
/// why it cannot be used.
pub fn probe_helper(helper: &str) -> Result<&'static str, Unavailable> {
    static PROBES: LazyLock<Mutex<HashMap<String, Result<&'static str, Unavailable>>>> =
        LazyLock::new(|| Mutex::new(HashMap::new()));
    let mut probes = PROBES.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(known) = probes.get(helper) {
        return known.clone();
    }
    let result = probe_once(helper);
    probes.insert(helper.to_owned(), result.clone());
    result
}

fn probe_once(helper: &str) -> Result<&'static str, Unavailable> {
    let output = Command::new(helper)
        .args(["gateway", "connect", "--help"])
        .stdin(Stdio::null())
        .output()
        .map_err(|e| Unavailable(format!("koh helper {helper:?} cannot run: {e}")))?;
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    if !output.status.success() && !text.contains("--socket") {
        return Err(Unavailable(format!(
            "{helper} gateway connect --help failed: {}",
            output.status
        )));
    }
    TCP_FLAGS
        .into_iter()
        .find(|flag| text.contains(flag))
        .ok_or_else(|| {
            Unavailable("koh forwards Unix sockets; TCP forwarding not published".into())
        })
}
