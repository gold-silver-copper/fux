//! Loopback-only deterministic OpenAI-compatible fixture, with no cloud credentials.
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
fn request(peer: &mut TcpStream) -> Result<Value> {
    let end = Instant::now() + Duration::from_secs(3);
    let mut header = Vec::new();
    while !header.ends_with(b"\r\n\r\n") {
        peer.set_read_timeout(Some(
            end.checked_duration_since(Instant::now())
                .context("HTTP header deadline")?,
        ))?;
        let mut b = [0];
        peer.read_exact(&mut b)?;
        header.push(b[0]);
        ensure!(header.len() <= 65536, "HTTP header bound");
    }
    let header = std::str::from_utf8(&header)?;
    let mut lines = header.split("\r\n");
    ensure!(
        lines.next() == Some("POST /v1/chat/completions HTTP/1.1"),
        "fixture route"
    );
    let mut size = None;
    for line in lines.filter(|s| !s.is_empty()) {
        let (k, v) = line.split_once(':').context("HTTP header")?;
        if k.eq_ignore_ascii_case("content-length") {
            ensure!(size.is_none(), "duplicate content-length");
            size = Some(v.trim().parse::<usize>()?);
        }
        ensure!(
            !k.eq_ignore_ascii_case("transfer-encoding"),
            "fixture requires content length"
        );
    }
    let size = size.context("content length")?;
    ensure!((1..=1048576).contains(&size), "body bound");
    let mut body = vec![0; size];
    peer.set_read_timeout(Some(
        end.checked_duration_since(Instant::now())
            .context("HTTP body deadline")?,
    ))?;
    peer.read_exact(&mut body)?;
    Ok(serde_json::from_slice(&body)?)
}
fn response(body: &Value, fixture: &str) -> Result<(Value, Vec<u8>)> {
    let messages = body["messages"].as_array().context("messages")?;
    let tool_turn = messages.iter().any(|m| {
        m["role"] == "user"
            && m.get("content")
                .is_some_and(|v| v.to_string().contains("ZOR_TOOL_STOP"))
    });
    let tool_result = messages.iter().any(|m| m["role"] == "tool");
    let label = if tool_result {
        "after-tool"
    } else if tool_turn {
        "tool-stop"
    } else {
        "text"
    };
    let delta = if label == "tool-stop" {
        json!({"role":"assistant","tool_calls":[{"index":0,"id":"fixture-read","type":"function","function":{"name":"read","arguments":serde_json::to_string(&json!({"filePath":fixture}))?}}]})
    } else {
        json!({"role":"assistant","content":"FIXTURE RESPONSE"})
    };
    let mut payload = String::new();
    for (delta, finish) in [(delta, Value::Null), (json!({}), json!("stop"))] {
        let chunk = json!({"id":"chatcmpl-fixture","object":"chat.completion.chunk","created":1,"model":"fixture","choices":[{"index":0,"delta":delta,"finish_reason":finish}]});
        payload.push_str(&format!("data: {chunk}\n\n"));
    }
    payload.push_str("data: [DONE]\n\n");
    let roles = messages
        .iter()
        .map(|m| m.get("role").cloned().context("role"))
        .collect::<Result<Vec<_>>>()?;
    Ok((
        json!({"response":label,"roles":roles,"model":body.get("model").context("model")?,"stream":body.get("stream")}),
        payload.into_bytes(),
    ))
}
fn integration_response(body: &Value, fixture: &str) -> Result<(Value, Vec<u8>)> {
    let messages = body["messages"].as_array().context("messages")?;
    let has = |marker: &str| {
        messages.iter().any(|m| {
            m["role"] == "user"
                && m.get("content")
                    .is_some_and(|v| v.to_string().contains(marker))
        })
    };
    let (label, name, id, args) = if has("ZOR_QUESTION") {
        (
            "question",
            "question",
            "fixture-question",
            json!({"questions":crate::integration::questions()}),
        )
    } else if has("ZOR_APPROVAL") {
        (
            "approval",
            "bash",
            "fixture-approval",
            json!({"command":"printf harmless","description":"Fixture permission request"}),
        )
    } else {
        let (record, bytes) = response(body, fixture)?;
        return Ok((
            json!({"response":record["response"],"roles":record["roles"]}),
            bytes,
        ));
    };
    let delta = json!({"role":"assistant","tool_calls":[{"index":0,"id":id,"type":"function","function":{"name":name,"arguments":serde_json::to_string(&args)?}}]});
    let mut payload = String::new();
    for (delta, finish) in [(delta, Value::Null), (json!({}), json!("stop"))] {
        let chunk = json!({"id":"chatcmpl-fixture","object":"chat.completion.chunk","created":1,"model":"fixture","choices":[{"index":0,"delta":delta,"finish_reason":finish}]});
        payload.push_str(&format!("data: {chunk}\n\n"));
    }
    payload.push_str("data: [DONE]\n\n");
    let roles = messages
        .iter()
        .map(|m| m.get("role").cloned().context("role"))
        .collect::<Result<Vec<_>>>()?;
    Ok((
        json!({"response":label,"roles":roles}),
        payload.into_bytes(),
    ))
}
fn resume_response(body: &Value) -> Result<(Value, Vec<u8>)> {
    let messages = body
        .get("messages")
        .filter(|v| v.is_array())
        .context("messages")?;
    let second = messages.as_array().unwrap().iter().any(|m| {
        m["role"] == "user"
            && m.get("content")
                .is_some_and(|v| v.to_string().contains("RESUME_SECOND"))
    });
    let answer = if second {
        "RESUME_REPLY_2"
    } else {
        "RESUME_REPLY_1"
    };
    let mut payload = String::new();
    for (delta, finish) in [
        (json!({"role":"assistant","content":answer}), Value::Null),
        (json!({}), json!("stop")),
    ] {
        let chunk = json!({"id":"chatcmpl-fixture","object":"chat.completion.chunk","created":1,"model":"fixture","choices":[{"index":0,"delta":delta,"finish_reason":finish}]});
        payload.push_str(&format!("data: {chunk}\n\n"));
    }
    payload.push_str("data: [DONE]\n\n");
    Ok((messages.clone(), payload.into_bytes()))
}
pub struct Provider {
    pub port: u16,
    stop: Arc<AtomicBool>,
    records: Arc<Mutex<Vec<Value>>>,
    thread: Option<JoinHandle<Result<()>>>,
}
impl Provider {
    pub fn start(fixture: &Path) -> Result<Self> {
        Self::start_with(Some(fixture), false)
    }
    pub fn start_resume() -> Result<Self> {
        Self::start_with(None, false)
    }
    pub fn start_integration(fixture: &Path) -> Result<Self> {
        Self::start_with(Some(fixture), true)
    }
    fn start_with(fixture: Option<&Path>, integration: bool) -> Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        let port = listener.local_addr()?.port();
        listener.set_nonblocking(true)?;
        let stop = Arc::new(AtomicBool::new(false));
        let records = Arc::new(Mutex::new(Vec::new()));
        let shared = stop.clone();
        let requests = records.clone();
        let fixture = fixture
            .map(|p| p.to_str().context("fixture path").map(str::to_owned))
            .transpose()?;
        let thread = thread::spawn(move || -> Result<()> {
            while !shared.load(Ordering::Acquire) {
                let mut peer = match listener.accept() {
                    Ok((p, _)) => p,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10));
                        continue;
                    }
                    Err(e) => return Err(e.into()),
                };
                peer.set_write_timeout(Some(Duration::from_secs(3)))?;
                let result = (|| -> Result<Vec<u8>> {
                    ensure!(requests.lock().unwrap().len() < 8, "fixture request budget");
                    let body = request(&mut peer)?;
                    let (record, payload) = if let Some(fixture) = &fixture {
                        if integration {
                            integration_response(&body, fixture)?
                        } else {
                            response(&body, fixture)?
                        }
                    } else {
                        resume_response(&body)?
                    };
                    requests.lock().unwrap().push(record);
                    Ok(payload)
                })();
                match result {
                    Ok(payload) => {
                        write!(
                            peer,
                            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            payload.len()
                        )?;
                        peer.write_all(&payload)?;
                    }
                    Err(e) => {
                        let _=peer.write_all(b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                        return Err(e);
                    }
                }
            }
            Ok(())
        });
        Ok(Self {
            port,
            stop,
            records,
            thread: Some(thread),
        })
    }
    pub fn records(&self) -> Vec<Value> {
        self.records.lock().unwrap().clone()
    }
    pub fn close(&mut self) -> Result<()> {
        self.stop.store(true, Ordering::Release);
        if let Some(t) = self.thread.take() {
            let end = Instant::now() + Duration::from_secs(8);
            while !t.is_finished() {
                ensure!(Instant::now() < end, "provider thread did not stop");
                thread::sleep(Duration::from_millis(10));
            }
            t.join().map_err(|_| anyhow::anyhow!("provider panic"))??;
        }
        Ok(())
    }
}
impl Drop for Provider {
    fn drop(&mut self) {
        if let Err(e) = self.close() {
            eprintln!("provider cleanup: {e:#}");
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn plain_tool_and_continuation_keep_stop_finish_and_owned_path() -> Result<()> {
        for (messages, label) in [
            (json!([{"role":"user","content":"hello"}]), "text"),
            (
                json!([{"role":"user","content":"ZOR_TOOL_STOP"}]),
                "tool-stop",
            ),
            (
                json!([{"role":"user","content":"ZOR_TOOL_STOP"},{"role":"tool","content":"Owned fixture."}]),
                "after-tool",
            ),
        ] {
            let (record, bytes) = response(
                &json!({"model":"fixture","stream":true,"messages":messages}),
                "/owned/fixture.txt",
            )?;
            ensure!(record["response"] == label, "provider branch");
            let text = String::from_utf8(bytes)?;
            let chunks = text
                .split("\n\n")
                .take(2)
                .map(|s| serde_json::from_str::<Value>(s.strip_prefix("data: ").unwrap()))
                .collect::<std::result::Result<Vec<_>, _>>()?;
            ensure!(
                chunks[1]["choices"][0]["finish_reason"] == "stop",
                "completion finish"
            );
            if label == "tool-stop" {
                let args: Value = serde_json::from_str(
                    chunks[0]
                        .pointer("/choices/0/delta/tool_calls/0/function/arguments")
                        .unwrap()
                        .as_str()
                        .unwrap(),
                )?;
                ensure!(args["filePath"] == "/owned/fixture.txt", "owned path");
            } else {
                ensure!(
                    chunks[0]["choices"][0]["delta"]["content"] == "FIXTURE RESPONSE",
                    "text"
                );
            }
        }
        Ok(())
    }
}
