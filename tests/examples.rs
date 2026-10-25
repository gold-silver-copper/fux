//! The programs in `examples/`, run on a private server: each prints what
//! its header says (`# expect:`, a line each) and exits with its status
//! (`# status:`, else 0).
mod support;
use support::*;

#[test]
fn every_example_prints_what_its_header_says() -> Outcome {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("examples");
    let mut files: Vec<_> = std::fs::read_dir(&dir)
        .map_err(|e| e.to_string())?
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|path| path.extension().is_some_and(|e| e == "fux"))
        .collect();
    files.sort();
    if files.len() < 3 {
        return Err(format!("{} examples", files.len()));
    }
    let server = Server::start("")?;
    for file in &files {
        let text = std::fs::read_to_string(file).map_err(|e| e.to_string())?;
        let header = |key: &str| -> Vec<String> {
            (text.lines())
                .filter_map(|l| l.strip_prefix(&format!("# {key}: ")).map(str::to_owned))
                .collect()
        };
        let expected: String = (header("expect").iter())
            .map(|l| format!("{l}\n"))
            .collect();
        let status: i32 = header("status")
            .first()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let path = file.to_string_lossy();
        let ran = server.fux(&["--file", &path])?;
        if (ran.status, ran.stdout.as_str()) != (status, expected.as_str()) {
            return Err(format!(
                "{path}: exited {} printing {:?} ({}), expected {status} printing {expected:?}",
                ran.status, ran.stdout, ran.stderr
            ));
        }
    }
    Ok(())
}
