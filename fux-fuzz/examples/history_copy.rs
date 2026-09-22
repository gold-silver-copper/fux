//! Independent upstream expectation for copying retained, widened history.
//! Run from root: cargo run --manifest-path fux-fuzz/Cargo.toml --example history_copy --locked
fn main() {
    let mut parser = vt100::Parser::new(2, 5, 2);
    parser.process(b"abcdefgh\r\nlast");
    parser.screen_mut().set_size(2, 10);
    parser.screen_mut().set_scrollback(1);
    assert!(parser.screen().row_wrapped(0));
    let text = parser.screen().contents();
    assert_eq!(text, "abcdefgh");
    println!(
        "{}",
        serde_json::json!({"rows":2,"initial_cols":5,"resized_cols":10,"offset":1,"expected_text":text})
    );
}
