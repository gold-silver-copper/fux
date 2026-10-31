//! The lists of fux's interface kept in `docs/` (`requests.txt`,
//! `codes.txt` here; `words.txt` and `records.txt` in fux's tests), each
//! written from the table the code itself uses, so that a change to one
//! shows as a diff of its list, and a test fails while the list is stale.

/// The variable that writes the lists afresh instead of checking them.
pub const BLESS: &str = "FUX_BLESS";

/// Checks that `docs/NAME` is `text`, else says how to write it afresh: with
/// `FUX_BLESS` set, writes it instead. `test` is the command whose run
/// writes it (`cargo test -p fux-wire lists`).
pub fn check(name: &str, text: &str, test: &str) -> Result<(), String> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../docs")
        .join(name);
    if std::env::var_os(BLESS).is_some() {
        return std::fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()));
    }
    let written = std::fs::read_to_string(&path).unwrap_or_default();
    match written == text {
        true => Ok(()),
        false => Err(format!(
            "docs/{name} does not match the code: write it afresh with `{BLESS}=1 {test}` \
             and commit the diff"
        )),
    }
}

/// `head`, each of `lines`, and a line end after each.
pub fn file(head: &str, lines: impl IntoIterator<Item = String>) -> String {
    let mut out = format!("# {head}\n");
    for line in lines {
        out.push_str(&line);
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{check, file};

    const TEST: &str = "cargo test -p fux-wire lists";

    /// docs/requests.txt lists every request a client sends and every
    /// type it holds on the wire, each once, with its fields in their
    /// places on the wire, as the codec declares them.
    #[test]
    fn requests_list_is_current() -> Result<(), String> {
        use crate::wire::{Reach, Shapes};
        let mut shapes = Shapes::default();
        crate::command::Request::reach(&mut shapes);
        crate::protocol::Attach::reach(&mut shapes);
        let head = format!(
            "The requests a client sends its server, and every type they hold, each once \
             as first reached, with its fields in their order on the wire (fux-wire's \
             codec). Generated: FUX_BLESS=1 {TEST}"
        );
        check("requests.txt", &file(&head, shapes.lines), TEST)
    }

    /// docs/codes.txt lists every error code with its exit status, and
    /// whether a `try` catches it.
    #[test]
    fn codes_list_is_current() -> Result<(), String> {
        let lines = crate::code::Code::all().map(|code| {
            let caught = if code.raised() { "try" } else { "uncaught" };
            format!("{code} {} {caught}", code.status())
        });
        let head = format!(
            "Every error code, its exit status, and whether `try` catches it \
             (fux-wire's code table, then fuxth's). Generated: FUX_BLESS=1 {TEST}"
        );
        check("codes.txt", &file(&head, lines), TEST)
    }
}
