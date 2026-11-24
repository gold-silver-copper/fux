//! What the opens that follow no link open, and what they refuse.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]

use super::*;
use std::io::{Read, Seek, SeekFrom};

#[test]
fn a_file_is_opened_to_write_by_its_own_name_and_nothing_else_is() {
    let dir = std::env::temp_dir().join(format!("fuxix-write-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("folder")).unwrap();
    let text = |name: &str| {
        let mut text = String::new();
        open(dir.join(name), false).unwrap().read_to_string(&mut text).unwrap();
        text
    };
    // The file is as long as it was, and is written where the caller writes: at its end, here.
    std::fs::write(dir.join("file"), "kept").unwrap();
    let mut file = open_regular_write(dir.join("file")).unwrap();
    file.seek(SeekFrom::End(0)).unwrap();
    file.write_all(b", added to").unwrap();
    assert_eq!(text("file"), "kept, added to");
    // A link of the name is not followed, whatever it leads to: its file is as it was.
    std::os::unix::fs::symlink("file", dir.join("link")).unwrap();
    assert!(open_regular_write(dir.join("link")).is_err());
    assert_eq!(text("file"), "kept, added to");
    // A FIFO is not waited at with nobody reading it, and not written into with somebody; a folder
    // is no file; and nothing is made under a name that has nothing.
    let reader = fifo(dir.join("fifo")).unwrap();
    assert!(open_regular_write(dir.join("fifo")).is_err());
    assert_eq!((&reader).read(&mut [0; 8]).map_err(|e| e.kind()), Err(io::ErrorKind::WouldBlock));
    drop(reader);
    assert!(open_regular_write(dir.join("fifo")).is_err());
    assert!(open_regular_write(dir.join("folder")).is_err());
    assert!(open_regular_write(dir.join("none")).is_err() && !dir.join("none").exists());
    std::fs::remove_dir_all(&dir).unwrap();
}
