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
    use crate::wire::Shape;

    const TEST: &str = "cargo test -p fux-wire lists";

    /// `ty`'s variants (a struct's one), each a line: `TY::VARIANT` (a
    /// struct, its own name) and its fields, `name: Type`, in their places
    /// on the wire.
    fn shapes<T: Shape>(ty: &str) -> impl Iterator<Item = String> {
        T::SHAPE.iter().map(move |(variant, fields)| {
            let fields: Vec<String> = fields
                .iter()
                .map(|(name, ty)| format!("{name}: {}", ty.replace("crate::", "")))
                .collect();
            let name = match *variant == ty {
                true => ty.to_owned(),
                false => format!("{ty}::{variant}"),
            };
            format!("{name} {}", fields.join(", "))
                .trim_end()
                .to_owned()
        })
    }

    /// docs/requests.txt lists every request a client sends, each with its
    /// fields, as the codec declares them.
    #[test]
    fn requests_list_is_current() -> Result<(), String> {
        use crate::command::{ClientAction, Command, Request, Wait};
        use crate::protocol::Attach;
        let lines = (shapes::<Request>("Request"))
            .chain(shapes::<Command>("Command"))
            .chain(shapes::<ClientAction>("ClientAction"))
            .chain(shapes::<Wait>("Wait"))
            .chain(shapes::<Attach>("Attach"));
        let head = format!(
            "The requests a client sends its server, each with its fields in their order \
             on the wire (fux-wire's codec). Generated: FUX_BLESS=1 {TEST}"
        );
        check("requests.txt", &file(&head, lines), TEST)
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
