#![expect(clippy::unwrap_used, reason = "a test fails by panicking")]
use vt_web_backend::Backend;

fn feed(backend: &mut Backend, bytes: &[u8]) -> String {
    let mut out = String::new();
    backend.feed(bytes, &mut out).unwrap();
    out
}

/// The first update is the whole screen; after that only the rows that
/// changed, with their styles, and the cursor.
#[test]
fn sends_only_changed_rows_with_styles() {
    let mut backend = Backend::new(3, 10, 0).unwrap();
    assert_eq!(
        feed(&mut backend, b"hello\r\nworld"),
        concat!(
            r#"{"seq":1,"size":[3,10],"rows":[{"y":0,"runs":[{"t":"hello","w":5}]},"#,
            r#"{"y":1,"runs":[{"t":"world","w":5}]},{"y":2,"runs":[]}],"cursor":{"y":1,"x":5,"visible":true}}"#,
            "\n"
        )
    );
    assert_eq!(
        feed(&mut backend, b"\x1b[1;1Hj\x1b[1;31m\xe4\xb8\x96\x1b[m"),
        concat!(
            r#"{"seq":2,"rows":[{"y":0,"runs":[{"t":"j","w":1},{"t":"世","w":2,"fg":1,"b":true},{"t":"lo","w":2}]}],"#,
            r#""cursor":{"y":0,"x":3,"visible":true}}"#,
            "\n"
        )
    );
    // Nothing visible changed: nothing sent.
    assert_eq!(feed(&mut backend, b""), "");
    assert_eq!(
        feed(&mut backend, b"\x1b[6n"),
        "{\"reply\":\"\\u001b[1;4R\"}\n"
    );
}

/// A scroll is sent as one, with only the new row; a synchronized frame is
/// held until it ends.
#[test]
fn sends_a_scroll_and_holds_a_frame() {
    let mut backend = Backend::new(3, 10, 4096).unwrap();
    feed(&mut backend, b"one\r\ntwo\r\nthree");
    assert_eq!(
        feed(&mut backend, b"\r\nfour"),
        concat!(
            r#"{"seq":2,"scroll":1,"rows":[{"y":2,"runs":[{"t":"four","w":4}]}],"#,
            r#""cursor":{"y":2,"x":4,"visible":true}}"#,
            "\n"
        )
    );
    assert_eq!(feed(&mut backend, b"\x1b[?2026h\x1b[2;1Hxx\x1b[?25l"), "");
    assert_eq!(
        feed(&mut backend, b"\x1b[?2026l"),
        concat!(
            r#"{"seq":3,"rows":[{"y":1,"runs":[{"t":"xxree","w":5}]}],"#,
            r#""cursor":{"y":1,"x":2,"visible":false}}"#,
            "\n"
        )
    );
}
