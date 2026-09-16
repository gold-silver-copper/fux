//! A thin BRP client for the harness: the descriptor file, one JSON-RPC call per HTTP/1.1
//! connection, the same envelope injection the product CLIs perform. Independent of the
//! crates under test on purpose: a bug in `fux::remote::client` must not hide behind itself.

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::Path;
use std::time::Duration;

use serde_json::{Value, json};

use super::{Result, err};

/// Whole-call deadline: connect, write, read.
const TIMEOUT: Duration = Duration::from_secs(20);
const MAX_REPLY_BYTES: u64 = 16 * 1024 * 1024;

/// `<runtime>/<fux|zor>/<server>.brp.json` as the server wrote it.
#[derive(Clone, Debug)]
pub struct Descriptor {
    pub raw: Value,
    pub host: String,
    pub port: u16,
    pub token: String,
    pub instance: String,
    pub pid: u32,
}

impl Descriptor {
    pub fn read(path: &Path) -> Result<Self> {
        let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let raw: Value = serde_json::from_slice(&bytes)
            .map_err(|e| format!("{}: not a descriptor: {e}", path.display()))?;
        Self::parse(raw).map_err(|e| format!("{}: {e}", path.display()).into())
    }

    pub fn parse(raw: Value) -> Result<Self> {
        let field = |name: &str| raw.get(name).ok_or_else(|| format!("descriptor lacks `{name}`"));
        let http = field("http")?;
        let text = |value: Option<&Value>, name: &str| {
            value
                .and_then(Value::as_str)
                .map(str::to_owned)
                .ok_or_else(|| format!("descriptor `{name}` is not a string"))
        };
        Ok(Self {
            host: text(http.get("host"), "http.host")?,
            port: u16::try_from(http.get("port").and_then(Value::as_u64).unwrap_or(0))
                .map_err(|_| "descriptor `http.port` out of range")?,
            token: text(field("token").ok(), "token")?,
            instance: text(field("instance").ok(), "instance")?,
            pid: u32::try_from(field("pid")?.as_u64().unwrap_or(0))
                .map_err(|_| "descriptor `pid` out of range")?,
            raw,
        })
    }

    /// True while the server answers `<prefix>/server.info` with this descriptor's token.
    pub fn alive(&self, prefix: &str) -> bool {
        self.call(&format!("{prefix}/server.info"), json!({})).is_ok()
    }

    /// `method` with `token` and `instance` injected, as the product CLIs do.
    pub fn call(&self, method: &str, params: Value) -> Result<Value> {
        let Value::Object(mut fields) = params else {
            return Err(err("params must be an object"));
        };
        fields.insert("token".into(), Value::String(self.token.clone()));
        fields.insert("instance".into(), Value::String(self.instance.clone()));
        request(&self.host, self.port, method, Value::Object(fields))
    }
}

/// One JSON-RPC call with `params` exactly as given.
pub fn request(host: &str, port: u16, method: &str, params: Value) -> Result<Value> {
    let body = serde_json::to_vec(&json!({
        "jsonrpc": "2.0", "id": 1, "method": method, "params": params,
    }))?;
    let reply = post(host, port, &body)?;
    let Value::Object(mut reply) = reply else {
        return Err(err("reply is not an object"));
    };
    if let Some(error) = reply.remove("error") {
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("unknown error");
        return Err(format!("{method}: rpc error: {message}").into());
    }
    reply
        .remove("result")
        .ok_or_else(|| format!("{method}: reply has neither result nor error").into())
}

fn post(host: &str, port: u16, body: &[u8]) -> Result<Value> {
    let address = (host, port)
        .to_socket_addrs()?
        .next()
        .ok_or_else(|| err("unresolvable host"))?;
    let mut stream = TcpStream::connect_timeout(&address, TIMEOUT)?;
    stream.set_read_timeout(Some(TIMEOUT))?;
    stream.set_write_timeout(Some(TIMEOUT))?;
    let header = format!(
        "POST / HTTP/1.1\r\nHost: {host}:{port}\r\nContent-Type: application/json\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(header.as_bytes())?;
    stream.write_all(body)?;
    stream.flush()?;
    let mut raw = Vec::new();
    stream.take(MAX_REPLY_BYTES).read_to_end(&mut raw)?;
    parse_response(&raw)
}

/// `HTTP/1.1 <status> ...\r\n<headers>\r\n\r\n<body>`; the body is `Content-Length` bounded,
/// chunked, or runs to EOF (`Connection: close`).
fn parse_response(raw: &[u8]) -> Result<Value> {
    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or_else(|| err("no header terminator"))?;
    let (head, rest) = raw.split_at(split);
    let body = rest.get(4..).unwrap_or_default();
    let head = core::str::from_utf8(head)?;
    let mut lines = head.split("\r\n");
    let status_line = lines.next().unwrap_or_default();
    let status: u16 = status_line
        .split(' ')
        .nth(1)
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| format!("status line `{status_line}`"))?;
    let mut length = None;
    let mut chunked = false;
    for (name, value) in lines.filter_map(|line| line.split_once(':')) {
        if name.eq_ignore_ascii_case("content-length") {
            length = value.trim().parse::<usize>().ok();
        } else if name.eq_ignore_ascii_case("transfer-encoding") {
            chunked = value.trim().eq_ignore_ascii_case("chunked");
        }
    }
    let body = if chunked {
        dechunk(body)?
    } else {
        match length {
            Some(n) if n <= body.len() => body.get(..n).unwrap_or_default(),
            _ => body,
        }
        .to_vec()
    };
    if status != 200 {
        return Err(format!("http {status}: {}", String::from_utf8_lossy(&body)).into());
    }
    Ok(serde_json::from_slice(&body)?)
}

/// `<hex size>[;ext]\r\n<data>\r\n` repeated until a zero-size chunk.
fn dechunk(mut body: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    loop {
        let line_end = body
            .windows(2)
            .position(|w| w == b"\r\n")
            .ok_or_else(|| err("chunk size line"))?;
        let size_text = core::str::from_utf8(&body[..line_end])?;
        let size_text = size_text.split(';').next().unwrap_or_default().trim();
        let size = usize::from_str_radix(size_text, 16)?;
        body = &body[line_end + 2..];
        if size == 0 {
            return Ok(out);
        }
        let data = body.get(..size).ok_or_else(|| err("short chunk"))?;
        out.extend_from_slice(data);
        body = body.get(size + 2..).unwrap_or_default();
    }
}
