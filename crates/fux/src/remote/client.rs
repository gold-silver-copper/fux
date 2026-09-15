//! Thin BRP client: one JSON-RPC call per TCP connection over a minimal HTTP/1.1 POST. No
//! hyper, no reactor: the CLI and zor's observation loop are request/reply.

use std::io::{BufRead, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::Path;
use std::time::Duration;

use serde_json::{Map, Value, json};

pub use super::descriptor::{Descriptor, DescriptorError, read_descriptor};

/// Whole-call deadline: connect, write, read.
pub const TIMEOUT: Duration = Duration::from_secs(30);
/// Bound on a reply body; captures are bounded server-side well below this.
pub const MAX_REPLY_BYTES: usize = 64 * 1024 * 1024;

#[derive(Debug)]
pub enum ClientError {
    Descriptor(DescriptorError),
    /// Params must be a JSON object so `token`/`instance` can be injected.
    Params,
    Io(std::io::Error),
    /// The HTTP layer answered something that is not a JSON-RPC reply.
    Http {
        status: u16,
        body: String,
    },
    Malformed(String),
    Rpc {
        code: i16,
        message: String,
    },
}

impl core::fmt::Display for ClientError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Descriptor(e) => write!(f, "descriptor: {e}"),
            Self::Params => f.write_str("params must be a JSON object"),
            Self::Io(e) => write!(f, "connection: {e}"),
            Self::Http { status, body } => write!(f, "http {status}: {body}"),
            Self::Malformed(m) => write!(f, "malformed reply: {m}"),
            Self::Rpc { code, message } => write!(f, "error {code}: {message}"),
        }
    }
}

impl std::error::Error for ClientError {}

impl From<std::io::Error> for ClientError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<DescriptorError> for ClientError {
    fn from(e: DescriptorError) -> Self {
        Self::Descriptor(e)
    }
}

/// Calls `method` on the server named by `brp`, injecting `token` and `instance` from the
/// descriptor into `params` (which must be an object; `{}` for no parameters).
pub fn call(brp: &Path, method: &str, params: Value) -> Result<Value, ClientError> {
    let descriptor = read_descriptor(brp)?;
    call_with(&descriptor, method, params)
}

/// Same as [`call`] with an already-read descriptor (repeated calls).
pub fn call_with(
    descriptor: &Descriptor,
    method: &str,
    params: Value,
) -> Result<Value, ClientError> {
    let Value::Object(mut fields) = params else {
        return Err(ClientError::Params);
    };
    fields.insert("token".into(), Value::String(descriptor.token.clone()));
    fields.insert(
        "instance".into(),
        Value::String(descriptor.instance.clone()),
    );
    request(
        &descriptor.http.host,
        descriptor.http.port,
        method,
        Value::Object(fields),
    )
}

/// One JSON-RPC call with `params` sent exactly as given (no envelope injection).
pub fn request(host: &str, port: u16, method: &str, params: Value) -> Result<Value, ClientError> {
    let reply = post(host, port, &encode_request(method, params)?)?;
    unwrap_reply(reply)
}

/// The JSON-RPC request body for `method` with `params` as given.
pub fn encode_request(method: &str, params: Value) -> Result<Vec<u8>, ClientError> {
    let request = json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params });
    serde_json::to_vec(&request).map_err(|e| ClientError::Malformed(e.to_string()))
}

/// `result` of a JSON-RPC reply object, or its `error` as [`ClientError::Rpc`].
pub fn unwrap_reply(reply: Value) -> Result<Value, ClientError> {
    let Value::Object(mut reply) = reply else {
        return Err(ClientError::Malformed("reply is not an object".into()));
    };
    if let Some(Value::Object(error)) = reply.remove("error") {
        return Err(rpc_error(&error));
    }
    reply
        .remove("result")
        .ok_or_else(|| ClientError::Malformed("reply has neither result nor error".into()))
}

/// Opens a `+watch` stream and calls `on_item` for every JSON-RPC item the server emits
/// (`text/event-stream`, one `data:` line per item), until the server closes the stream or
/// `on_item` returns `false`. The read timeout is disabled: a quiet stream is not an error.
pub fn stream(
    descriptor: &Descriptor,
    method: &str,
    params: Value,
    mut on_item: impl FnMut(Value) -> bool,
) -> Result<(), ClientError> {
    let Value::Object(mut fields) = params else {
        return Err(ClientError::Params);
    };
    fields.insert("token".into(), Value::String(descriptor.token.clone()));
    fields.insert(
        "instance".into(),
        Value::String(descriptor.instance.clone()),
    );
    let request =
        json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": Value::Object(fields) });
    let body = serde_json::to_vec(&request).map_err(|e| ClientError::Malformed(e.to_string()))?;
    let host = &descriptor.http.host;
    let port = descriptor.http.port;
    let address = (host.as_str(), port)
        .to_socket_addrs()?
        .next()
        .ok_or_else(|| std::io::Error::other("unresolvable host"))?;
    let mut tcp = TcpStream::connect_timeout(&address, TIMEOUT)?;
    tcp.set_write_timeout(Some(TIMEOUT))?;
    let header = format!(
        "POST / HTTP/1.1\r\nHost: {host}:{port}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
        body.len()
    );
    tcp.write_all(header.as_bytes())?;
    tcp.write_all(&body)?;
    tcp.flush()?;
    let mut reader = std::io::BufReader::new(tcp);
    let mut line = String::new();
    // Status line and headers.
    reader.read_line(&mut line)?;
    let status: u16 = line
        .split(' ')
        .nth(1)
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| ClientError::Malformed(format!("status line `{}`", line.trim())))?;
    let mut chunked = false;
    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 || line == "\r\n" {
            break;
        }
        if let Some((name, value)) = line.split_once(':')
            && name.eq_ignore_ascii_case("transfer-encoding")
        {
            chunked = value.trim().eq_ignore_ascii_case("chunked");
        }
    }
    if status != 200 {
        let mut rest = String::new();
        let _ = reader.read_to_string(&mut rest);
        return Err(ClientError::Http { status, body: rest });
    }
    // Chunk framing carries whole SSE records; each record is `data: <json>\n\n`.
    let mut chunk = Vec::new();
    loop {
        let payload: &[u8] = if chunked {
            line.clear();
            if reader.read_line(&mut line)? == 0 {
                return Ok(());
            }
            let size_hex = line.split(';').next().unwrap_or_default().trim();
            let size = usize::from_str_radix(size_hex, 16)
                .map_err(|_| ClientError::Malformed("bad chunked encoding".into()))?;
            if size == 0 {
                return Ok(());
            }
            chunk.clear();
            chunk.resize(size + 2, 0);
            reader.read_exact(&mut chunk)?;
            chunk.get(..size).unwrap_or_default()
        } else {
            line.clear();
            if reader.read_line(&mut line)? == 0 {
                return Ok(());
            }
            line.as_bytes()
        };
        for record in payload.split(|b| *b == b'\n') {
            let Some(data) = record.strip_prefix(b"data:") else {
                continue;
            };
            let item: Value = serde_json::from_slice(data.trim_ascii())
                .map_err(|e| ClientError::Malformed(e.to_string()))?;
            let item = match item {
                Value::Object(mut reply) => {
                    if let Some(Value::Object(error)) = reply.remove("error") {
                        return Err(rpc_error(&error));
                    }
                    reply.remove("result").unwrap_or(Value::Null)
                }
                other => other,
            };
            if !on_item(item) {
                return Ok(());
            }
        }
    }
}

fn rpc_error(error: &Map<String, Value>) -> ClientError {
    let code = error
        .get("code")
        .and_then(Value::as_i64)
        .and_then(|c| i16::try_from(c).ok())
        .unwrap_or_default();
    let message = error
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    ClientError::Rpc { code, message }
}

fn post(host: &str, port: u16, body: &[u8]) -> Result<Value, ClientError> {
    let address = (host, port)
        .to_socket_addrs()?
        .next()
        .ok_or_else(|| std::io::Error::other("unresolvable host"))?;
    let mut stream = TcpStream::connect_timeout(&address, TIMEOUT)?;
    stream.set_read_timeout(Some(TIMEOUT))?;
    stream.set_write_timeout(Some(TIMEOUT))?;
    let header = format!(
        "POST / HTTP/1.1\r\nHost: {host}:{port}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(header.as_bytes())?;
    stream.write_all(body)?;
    stream.flush()?;
    let mut raw = Vec::new();
    stream.take(MAX_REPLY_BYTES as u64).read_to_end(&mut raw)?;
    parse_response(&raw)
}

/// Parses `HTTP/1.1 <status> ...\r\n<headers>\r\n\r\n<body>`; the body is `Content-Length`
/// bounded, `Transfer-Encoding: chunked` (hyper's default for streamed replies), or runs to
/// EOF (`Connection: close`).
pub fn parse_response(raw: &[u8]) -> Result<Value, ClientError> {
    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or_else(|| ClientError::Malformed("no header terminator".into()))?;
    let (head, rest) = raw.split_at(split);
    let body = rest.get(4..).unwrap_or_default();
    let head = core::str::from_utf8(head).map_err(|e| ClientError::Malformed(e.to_string()))?;
    let mut lines = head.split("\r\n");
    let status_line = lines.next().unwrap_or_default();
    let status: u16 = status_line
        .split(' ')
        .nth(1)
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| ClientError::Malformed(format!("status line `{status_line}`")))?;
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
        return Err(ClientError::Http {
            status,
            body: String::from_utf8_lossy(&body).into_owned(),
        });
    }
    serde_json::from_slice(&body).map_err(|e| ClientError::Malformed(e.to_string()))
}

/// `<hex size>[;ext]\r\n<data>\r\n` repeated until a zero-size chunk.
fn dechunk(mut body: &[u8]) -> Result<Vec<u8>, ClientError> {
    let malformed = || ClientError::Malformed("bad chunked encoding".into());
    let mut out = Vec::with_capacity(body.len());
    loop {
        let line_end = body
            .windows(2)
            .position(|w| w == b"\r\n")
            .ok_or_else(malformed)?;
        let size_line = core::str::from_utf8(body.get(..line_end).unwrap_or_default())
            .map_err(|_| malformed())?;
        let size_hex = size_line.split(';').next().unwrap_or_default().trim();
        let size = usize::from_str_radix(size_hex, 16).map_err(|_| malformed())?;
        body = body.get(line_end + 2..).ok_or_else(malformed)?;
        if size == 0 {
            return Ok(out);
        }
        out.extend_from_slice(body.get(..size).ok_or_else(malformed)?);
        body = body.get(size + 2..).ok_or_else(malformed)?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_status_headers_and_content_length_bounded_body() {
        let raw = b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\ncontent-length: 13\r\n\r\n{\"result\":{}}trailing";
        assert_eq!(parse_response(raw).unwrap(), json!({"result": {}}));
        let raw = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\n{\"res\r\n8;ext=1\r\nult\":{}}\r\n0\r\n\r\n";
        assert_eq!(parse_response(raw).unwrap(), json!({"result": {}}));
        assert!(matches!(
            parse_response(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nzz\r\n"),
            Err(ClientError::Malformed(_))
        ));
        let raw = b"HTTP/1.1 500 Internal\r\n\r\nboom";
        assert!(matches!(
            parse_response(raw),
            Err(ClientError::Http { status: 500, .. })
        ));
        assert!(matches!(
            parse_response(b"garbage"),
            Err(ClientError::Malformed(_))
        ));
    }
}
