//! No-account capture fixture; it supplies output but makes no agent-state claim.
fn main() {
    if std::env::args().nth(1).as_deref() == Some("--version") {
        println!("zor lifecycle fixture 1");
        return;
    }
    println!("LIFECYCLE_FIXTURE_READY");
    std::thread::sleep(std::time::Duration::from_secs(60));
}
