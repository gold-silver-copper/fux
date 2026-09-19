#![allow(
    clippy::unwrap_used,
    reason = "integration-test helpers; clippy.toml only relaxes #[test] bodies"
)]

use fux::remote::{
    client,
    descriptor::{Descriptor, Endpoint},
};
use serde_json::{Value, json};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::time::Duration;

fn response(bytes: Vec<u8>) -> (Descriptor, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let thread = std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        socket
            .set_write_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut request = Vec::new();
        let mut byte = [0];
        while !request.ends_with(b"\r\n\r\n") && request.len() < 8192 {
            socket.read_exact(&mut byte).unwrap();
            request.push(byte[0]);
        }
        let headers = String::from_utf8(request).unwrap();
        let length: usize = headers
            .lines()
            .filter_map(|line| line.split_once(':'))
            .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
            .unwrap()
            .1
            .trim()
            .parse()
            .unwrap();
        socket.read_exact(&mut vec![0; length]).unwrap();
        let _ = socket.write_all(&bytes);
    });
    (
        Descriptor {
            instance: "stream-fixture".into(),
            pid: std::process::id(),
            http: Endpoint {
                host: "127.0.0.1".into(),
                port,
            },
            attach: None,
            token: "private".into(),
        },
        thread,
    )
}

#[test]
fn chunk_boundaries_do_not_split_event_records() {
    let item = b"data: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"cursor\":7}}\n\n";
    let mut bytes = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n".to_vec();
    for piece in item.chunks(3) {
        bytes.extend_from_slice(format!("{:x}\r\n", piece.len()).as_bytes());
        bytes.extend_from_slice(piece);
        bytes.extend_from_slice(b"\r\n");
    }
    bytes.extend_from_slice(b"0\r\n\r\n");
    let (descriptor, peer) = response(bytes);
    let mut items = Vec::new();
    client::stream(&descriptor, "fux/events+watch", json!({}), |item| {
        items.push(item);
        true
    })
    .unwrap();
    peer.join().unwrap();
    assert_eq!(items, vec![json!({"cursor":7})]);
}

#[test]
fn oversized_chunk_length_is_refused_before_allocation() {
    let bytes = format!(
        "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n{:x}\r\n",
        usize::MAX
    )
    .into_bytes();
    let (descriptor, peer) = response(bytes);
    let mut delivered = Vec::<Value>::new();
    let result = client::stream(&descriptor, "fux/events+watch", json!({}), |item| {
        delivered.push(item);
        true
    });
    peer.join().unwrap();
    assert!(matches!(result, Err(client::ClientError::Malformed(_))));
    assert!(delivered.is_empty());
}

#[test]
fn oversized_unterminated_header_is_refused() {
    let mut bytes = b"HTTP/1.1 200 OK\r\nX-Header: ".to_vec();
    bytes.extend(std::iter::repeat_n(b'x', 40 * 1024));
    let (descriptor, peer) = response(bytes);
    let result = client::stream(&descriptor, "fux/events+watch", json!({}), |_| true);
    peer.join().unwrap();
    assert!(matches!(result, Err(client::ClientError::Malformed(_))));
}
