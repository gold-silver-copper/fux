use super::*;
use crate::testing::Outcome;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);

/// A fresh directory under the system temporary directory; the socket
/// directory inside it is created by the code under test unless a case
/// prepares it.
fn scratch() -> Result<PathBuf, Box<dyn std::error::Error>> {
    let directory = std::env::temp_dir().join(format!(
        "fux-transport-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir(&directory)?;
    Ok(directory.canonicalize()?)
}

fn mode(path: &Path) -> Result<u32, io::Error> {
    Ok(fs::symlink_metadata(path)?.permissions().mode() & 0o7777)
}

#[test]
fn socket_paths_are_validated_with_their_source_named() {
    let limit = max_path_bytes();
    #[cfg(target_os = "macos")]
    assert_eq!(limit, 103);
    for (value, needle) in [
        ("", "empty"),
        ("http://127.0.0.1:15702", "looks like a URL"),
        ("unix:///tmp/fux.sock", "looks like a URL"),
        ("relative/fux.sock", "absolute"),
        ("/tmp/a\0b", "NUL"),
    ] {
        let error = checked(value, "FUX_SOCKET").err().unwrap_or_default();
        assert!(
            error.contains("FUX_SOCKET") && error.contains(needle),
            "{error}"
        );
    }
    // The limit counts encoded bytes: a multibyte name reaches it in fewer
    // characters, and exactly the limit is still accepted.
    let fits = format!("/tmp/{}", "é".repeat((limit - 5) / 2));
    let fits = format!("{fits}{}", "x".repeat(limit - fits.len()));
    assert_eq!(fits.len(), limit);
    assert!(checked(&fits, "--socket").is_ok());
    let over = format!("{fits}x");
    let error = checked(&over, "--socket").err().unwrap_or_default();
    assert!(error.contains(&format!("{limit}-byte limit")), "{error}");
    assert!(error.contains(&over), "{error}");
    // The message counts the socket path, not the variable it came from: an
    // over-long XDG_RUNTIME_DIR is reported as the socket path being too long,
    // with the variable named as its source (hunt 7's small note).
    let long_dir = format!("/tmp/{}", "d".repeat(limit));
    let error = socket_path_from("XDG_RUNTIME_DIR", &long_dir)
        .err()
        .unwrap_or_default();
    let socket = format!("{long_dir}/fux/{DEFAULT_NAME}");
    assert!(
        error.contains(&format!("the socket path is {} bytes", socket.len())),
        "{error}"
    );
    assert!(error.contains("from XDG_RUNTIME_DIR"), "{error}");
    assert_eq!(
        socket_path(Some("/tmp/x/fux.sock")).ok(),
        Some(PathBuf::from("/tmp/x/fux.sock"))
    );
}

#[test]
fn binding_creates_a_private_directory_and_socket_and_cleanup_removes_it() -> Outcome {
    let root = scratch()?;
    let socket = root.join("s").join("fux.sock");
    let (endpoint, listener) = bind_socket(&socket)?;
    assert_eq!(mode(&root.join("s"))?, 0o700);
    assert_eq!(mode(&socket)?, 0o600);
    assert!(fs::symlink_metadata(&socket)?.file_type().is_socket());
    check_client_socket(&socket)?;
    let client = UnixStream::connect(&socket)?;
    listener.accept()?;
    drop(client);
    drop(listener);
    drop(endpoint);
    assert!(fs::symlink_metadata(&socket).is_err());
    // The lockfile stays: removing it would let two starts lock different files.
    assert!(root.join("s").join("fux.sock.lock").exists());
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn unsafe_locations_are_refused_and_left_unchanged() -> Outcome {
    let root = scratch()?;
    // A socket directory other users can enter.
    let open = root.join("open");
    fs::create_dir(&open)?;
    fs::set_permissions(&open, fs::Permissions::from_mode(0o755))?;
    let error = bind_socket(&open.join("fux.sock"))
        .err()
        .unwrap_or_default();
    assert!(
        error.contains("mode 0700") && error.contains("0755"),
        "{error}"
    );
    assert_eq!(mode(&open)?, 0o755);
    assert!(fs::read_dir(&open)?.next().is_none());
    // A symbolic link in place of the socket directory.
    let real = root.join("real");
    fs::create_dir(&real)?;
    fs::set_permissions(&real, fs::Permissions::from_mode(0o700))?;
    let link = root.join("link");
    std::os::unix::fs::symlink(&real, &link)?;
    let error = bind_socket(&link.join("fux.sock"))
        .err()
        .unwrap_or_default();
    assert!(error.contains("symbolic link"), "{error}");
    assert!(fs::read_dir(&real)?.next().is_none());
    // A regular file, and a symbolic link, where the socket would go.
    let file = real.join("fux.sock");
    fs::write(&file, "keep")?;
    let error = bind_socket(&file).err().unwrap_or_default();
    assert!(error.contains("not a socket"), "{error}");
    assert_eq!(fs::read_to_string(&file)?, "keep");
    let pointer = real.join("pointer.sock");
    std::os::unix::fs::symlink(&file, &pointer)?;
    let error = bind_socket(&pointer).err().unwrap_or_default();
    assert!(error.contains("not a socket"), "{error}");
    assert!(fs::symlink_metadata(&pointer)?.file_type().is_symlink());
    // Nothing was created beside them, not even a lockfile.
    assert_eq!(fs::read_dir(&real)?.count(), 2);
    // A client refuses the same shapes rather than connecting.
    assert!(check_client_socket(&file).is_err());
    assert!(check_client_socket(&open.join("fux.sock")).is_err());
    // A directory beneath one that other users can rewrite without the
    // sticky bit is not private, however its own mode reads.
    let shared = root.join("shared");
    fs::create_dir(&shared)?;
    fs::set_permissions(&shared, fs::Permissions::from_mode(0o777))?;
    let error = bind_socket(&shared.join("s").join("fux.sock"))
        .err()
        .unwrap_or_default();
    assert!(error.contains("another user"), "{error}");
    assert!(fs::read_dir(&shared)?.next().is_none());
    fs::set_permissions(&shared, fs::Permissions::from_mode(0o1777))?;
    let (endpoint, _listener) = bind_socket(&shared.join("s").join("fux.sock"))?;
    drop(endpoint);
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn a_stale_socket_is_replaced_and_a_live_or_locked_one_is_not() -> Outcome {
    let root = scratch()?;
    let directory = root.join("s");
    fs::create_dir(&directory)?;
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))?;
    let socket = directory.join("fux.sock");
    // Stale: a socket file with nothing listening, as SIGKILL leaves it.
    drop(UnixListener::bind(&socket)?);
    assert!(fs::symlink_metadata(&socket).is_ok());
    let (endpoint, listener) = bind_socket(&socket)?;
    // Locked: a second fux start on the same path.
    let error = bind_socket(&socket).err().unwrap_or_default();
    assert!(error.contains("already using"), "{error}");
    let client = UnixStream::connect(&socket)?;
    listener.accept()?;
    drop(client);
    drop(endpoint);
    drop(listener);
    // Live without the lock (a server that predates it, or anything else
    // listening): refused, and the listener keeps working.
    let other = UnixListener::bind(&socket)?;
    let error = bind_socket(&socket).err().unwrap_or_default();
    assert!(error.contains("already listening"), "{error}");
    let client = UnixStream::connect(&socket)?;
    other.accept()?;
    drop(client);
    drop(other);
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn concurrent_starts_on_one_path_have_exactly_one_winner() -> Outcome {
    for _ in 0..20 {
        let root = scratch()?;
        let socket = root.join("s").join("fux.sock");
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(4));
        let starts: Vec<_> = (0..4)
            .map(|_| {
                let socket = socket.clone();
                let barrier = barrier.clone();
                thread::spawn(move || {
                    barrier.wait();
                    bind_socket(&socket)
                })
            })
            .collect();
        let results: Vec<_> = starts
            .into_iter()
            .map(|start| start.join().map_err(|_| "start panicked"))
            .collect::<Result<_, _>>()?;
        let winners = results.iter().filter(|result| result.is_ok()).count();
        assert_eq!(
            winners,
            1,
            "{:?}",
            results.iter().map(|r| r.as_ref().err()).collect::<Vec<_>>()
        );
        for result in &results {
            if let Err(error) = result {
                assert!(error.contains("already using"), "{error}");
            }
        }
        drop(results);
        fs::remove_dir_all(root)?;
    }
    Ok(())
}

#[test]
fn cleanup_leaves_a_socket_that_replaced_its_own() -> Outcome {
    let root = scratch()?;
    let socket = root.join("s").join("fux.sock");
    let (endpoint, listener) = bind_socket(&socket)?;
    drop(listener);
    fs::remove_file(&socket)?;
    let replacement = UnixListener::bind(&socket)?;
    drop(endpoint);
    assert!(fs::symlink_metadata(&socket)?.file_type().is_socket());
    drop(replacement);
    fs::remove_dir_all(root)?;
    Ok(())
}
