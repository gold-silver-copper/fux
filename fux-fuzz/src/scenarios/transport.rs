//! Hunt 6, area 1: the HTTP transport fux owns since PR #45, spoken to with raw
//! sockets rather than through a cooperative client. Every case here is
//! correctly refused or absorbed today, so this scenario is coverage and runs
//! in the default smoke. The transport breaks hunt 6 did find are about
//! resources, not parsing, and live in `fux-fuzz/repro/006` and `007`.
//!
//! Each case opens its own connection and closes it, so the listener's backlog
//! is never left full between cases.
use super::*;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

const READ: Duration = Duration::from_millis(1500);

/// One request, one connection. `None` means the server sent nothing before
/// the read deadline, which is correct for a request that is never completed.
fn exchange(server: &Server, payload: &[u8]) -> Result<Option<String>> {
    let mut stream = UnixStream::connect(server.socket())?;
    stream.set_read_timeout(Some(READ))?;
    stream.set_write_timeout(Some(READ))?;
    if stream.write_all(payload).is_err() {
        // A request the server rejects at the protocol level can close the
        // connection before the whole payload is written; that is an answer.
        return Ok(None);
    }
    let _ = stream.flush();
    let mut got = Vec::new();
    let mut chunk = [0_u8; 8192];
    loop {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                got.extend_from_slice(chunk.get(..n).unwrap_or_default());
                if got.len() > 4 << 20 {
                    break;
                }
            }
            Err(_) => break,
        }
    }
    if got.is_empty() {
        return Ok(None);
    }
    Ok(Some(String::from_utf8_lossy(&got).into_owned()))
}

fn post(body: &str) -> Vec<u8> {
    format!(
        "POST / HTTP/1.1\r\nHost: fux\r\nContent-Type: application/json\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .into_bytes()
}

pub(super) fn run(s: &mut Server) -> Result<()> {
    let f = s.attach(24, 80)?;
    let viewer = s.frontend(f)?.viewer;
    s.wait("first shell output", |s| {
        Ok(s.frame(viewer, 24, 80)?.contains("DEFAULT-SHELL"))
    })?;

    let discover = r#"{"jsonrpc":"2.0","id":1,"method":"rpc.discover"}"#;
    let watch = format!(
        r#"{{"jsonrpc":"2.0","id":1,"method":"fux.frame+watch","params":{{"viewer":{viewer}}}}}"#
    );

    // (payload, must the reply mention this?) -- None where no reply is owed.
    let mut cases: Vec<(String, Vec<u8>, Option<&str>)> = vec![
        (
            "GET with no body".into(),
            b"GET / HTTP/1.1\r\nHost: fux\r\nConnection: close\r\n\r\n".to_vec(),
            Some("-32600"),
        ),
        (
            "OPTIONS with no body".into(),
            b"OPTIONS / HTTP/1.1\r\nHost: fux\r\nConnection: close\r\n\r\n".to_vec(),
            Some("-32600"),
        ),
        (
            "HTTP/1.0".into(),
            format!(
                "POST / HTTP/1.0\r\nContent-Length: {}\r\n\r\n{discover}",
                discover.len()
            )
            .into_bytes(),
            Some("\"result\""),
        ),
        (
            "a path that is not /".into(),
            format!(
                "POST /anything HTTP/1.1\r\nHost: fux\r\nContent-Length: {}\r\n\
                 Connection: close\r\n\r\n{discover}",
                discover.len()
            )
            .into_bytes(),
            Some("\"result\""),
        ),
        (
            "Expect: 100-continue".into(),
            format!(
                "POST / HTTP/1.1\r\nHost: fux\r\nExpect: 100-continue\r\nContent-Length: {}\r\n\
                 Connection: close\r\n\r\n{discover}",
                discover.len()
            )
            .into_bytes(),
            Some("\"result\""),
        ),
        (
            "Content-Length and Transfer-Encoding together".into(),
            format!(
                "POST / HTTP/1.1\r\nHost: fux\r\nContent-Length: {}\r\n\
                 Transfer-Encoding: chunked\r\nConnection: close\r\n\r\n{discover}",
                discover.len()
            )
            .into_bytes(),
            Some("-32600"),
        ),
        (
            "well formed chunked body".into(),
            format!(
                "POST / HTTP/1.1\r\nHost: fux\r\nTransfer-Encoding: chunked\r\n\
                 Connection: close\r\n\r\n{:x}\r\n{discover}\r\n0\r\n\r\n",
                discover.len()
            )
            .into_bytes(),
            Some("\"result\""),
        ),
        (
            "chunked body with a bad chunk size".into(),
            format!(
                "POST / HTTP/1.1\r\nHost: fux\r\nTransfer-Encoding: chunked\r\n\
                 Connection: close\r\n\r\nzz\r\n{discover}"
            )
            .into_bytes(),
            Some("-32600"),
        ),
        (
            "Content-Length shorter than the body".into(),
            format!("POST / HTTP/1.1\r\nHost: fux\r\nContent-Length: 5\r\nConnection: close\r\n\r\n{discover}")
                .into_bytes(),
            Some("-32600"),
        ),
        (
            "negative Content-Length".into(),
            format!("POST / HTTP/1.1\r\nHost: fux\r\nContent-Length: -1\r\nConnection: close\r\n\r\n{discover}")
                .into_bytes(),
            None,
        ),
        (
            "invalid UTF-8 in the body".into(),
            b"POST / HTTP/1.1\r\nHost: fux\r\nContent-Length: 6\r\nConnection: close\r\n\r\n\xff\xfe\xfd\xfc\xfb\xfa".to_vec(),
            Some("-32600"),
        ),
        (
            "invalid UTF-8 in the request target".into(),
            format!("POST /\u{fffd} HTTP/1.1\r\nHost: fux\r\nContent-Length: {}\r\n\r\n{discover}", discover.len())
                .into_bytes(),
            None,
        ),
        (
            "a NUL byte in a header value".into(),
            format!("POST / HTTP/1.1\r\nHost: fux\r\nX-N: \0\r\nContent-Length: {}\r\n\r\n{discover}", discover.len())
                .into_bytes(),
            None,
        ),
        (
            "bare LF line endings".into(),
            format!("POST / HTTP/1.1\nHost: fux\nContent-Length: {}\n\n{discover}", discover.len())
                .into_bytes(),
            None,
        ),
        ("bytes that are not HTTP".into(), (0_u8..=255).cycle().take(2048).collect(), None),
        ("empty body".into(), post(""), Some("-32600")),
        ("body is null".into(), post("null"), Some("-32600")),
        ("body is a number".into(), post("7"), Some("-32600")),
        ("body is an empty array".into(), post("[]"), Some("[]")),
        ("body is not JSON".into(), post("{not json"), Some("-32600")),
        (
            "a request with no method".into(),
            post(r#"{"jsonrpc":"2.0","id":3}"#),
            Some("-32600"),
        ),
        (
            "id is an object".into(),
            post(r#"{"jsonrpc":"2.0","id":{"a":1},"method":"rpc.discover"}"#),
            Some("\"result\""),
        ),
        (
            "a method that does not exist".into(),
            post(r#"{"jsonrpc":"2.0","id":4,"method":"no.such.method"}"#),
            Some("-32601"),
        ),
        (
            "a 1000-request batch".into(),
            post(&format!(
                "[{}]",
                std::iter::repeat_n(discover, 1000).collect::<Vec<_>>().join(",")
            )),
            Some("\"result\""),
        ),
    ];

    // 10 000 headers, one 1 MiB header, and a 1 MiB request target: hyper must
    // refuse each without the server noticing.
    let mut many = String::from("POST / HTTP/1.1\r\nHost: fux\r\n");
    for i in 0..10_000 {
        many.push_str(&format!("X-{i}: v\r\n"));
    }
    many.push_str(&format!(
        "Content-Length: {}\r\nConnection: close\r\n\r\n{discover}",
        discover.len()
    ));
    cases.push(("10000 headers".into(), many.into_bytes(), None));
    cases.push((
        "one 1 MiB header".into(),
        format!(
            "POST / HTTP/1.1\r\nHost: fux\r\nX-Big: {}\r\nContent-Length: {}\r\n\r\n{discover}",
            "a".repeat(1 << 20),
            discover.len()
        )
        .into_bytes(),
        None,
    ));
    cases.push((
        "params nested 10000 deep".into(),
        post(&format!(
            r#"{{"jsonrpc":"2.0","id":1,"method":"rpc.discover","params":{}{}}}"#,
            "[".repeat(10_000),
            "]".repeat(10_000)
        )),
        Some("-32600"),
    ));
    cases.push((
        "id is a 1 MiB string".into(),
        post(&format!(
            r#"{{"jsonrpc":"2.0","id":"{}","method":"rpc.discover"}}"#,
            "a".repeat(1 << 20)
        )),
        Some("\"result\""),
    ));
    cases.push((
        "ten pipelined requests on one connection".into(),
        {
            let mut v = Vec::new();
            for _ in 0..9 {
                v.extend_from_slice(
                    format!(
                        "POST / HTTP/1.1\r\nHost: fux\r\nContent-Length: {}\r\n\r\n{discover}",
                        discover.len()
                    )
                    .as_bytes(),
                );
            }
            v.extend_from_slice(
                format!(
                    "POST / HTTP/1.1\r\nHost: fux\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{discover}",
                    discover.len()
                )
                .as_bytes(),
            );
            v
        },
        Some("\"result\""),
    ));

    for (name, payload, expect) in cases {
        let reply = exchange(s, &payload)?;
        if let (Some(needle), Some(text)) = (expect, reply.as_deref()) {
            ensure(
                text.contains(needle),
                &format!("application: {name}: reply did not contain {needle:?}: {text:.200}"),
            )?;
        }
        // Whatever the case did, the next ordinary request on a fresh
        // connection must succeed and the viewer must still paint.
        s.healthy()?;
        let next = exchange(s, &post(discover))?
            .ok_or_else(|| format!("application: {name}: the next request got no reply"))?;
        ensure(
            next.contains("\"result\""),
            &format!("application: {name}: the next request failed: {next:.200}"),
        )?;
        ensure(
            !s.frame(viewer, 24, 80)?.is_empty(),
            &format!("application: {name}: the viewer stopped painting"),
        )?;
    }

    // A connection opened and left silent must not hold anything up.
    let idle = UnixStream::connect(s.socket())?;
    let start = Instant::now();
    s.rpc("rpc.discover", Value::Null)?;
    ensure(
        start.elapsed() < Duration::from_secs(2),
        "application: an idle connection delayed an ordinary request",
    )?;
    drop(idle);

    // A watch inside a batch is refused. It is aimed at a spare viewer of its
    // own, because a refused batch still detaches the viewer it names (hunt 6
    // finding 003), and this case is about the protocol reply, not that.
    let spare = s
        .rpc("fux.attach", json!({"rows":24,"cols":80}))?
        .get("viewer")
        .and_then(Value::as_u64)
        .ok_or("attach returned no viewer")?;
    let spare_watch = format!(
        r#"{{"jsonrpc":"2.0","id":1,"method":"fux.frame+watch","params":{{"viewer":{spare}}}}}"#
    );
    let reply = exchange(s, &post(&format!("[{spare_watch}]")))?
        .ok_or("a batch containing a watch got no reply")?;
    ensure(
        reply.contains("Streaming can not be used in batch requests"),
        &format!("application: a watch in a batch was not refused: {reply:.200}"),
    )?;
    s.healthy()?;
    ensure(
        !s.frame(viewer, 24, 80)?.is_empty(),
        "application: the driver stopped painting after a refused batch watch",
    )?;

    // A watch opened and dropped without reading detaches only its own viewer.
    let before = s.query("fux::model::Viewer")?.len();
    {
        let mut stream = UnixStream::connect(s.socket())?;
        stream.set_read_timeout(Some(READ))?;
        stream.write_all(&post(&watch))?;
        stream.flush()?;
        // Read the stream's first bytes, so the request is dispatched before
        // the connection goes away.
        let mut chunk = [0_u8; 4096];
        let _ = stream.read(&mut chunk);
    }
    s.wait("the dropped watch detached its viewer", |s| {
        Ok(s.query("fux::model::Viewer")?.len() < before)
    })?;
    s.healthy()?;
    Ok(())
}
