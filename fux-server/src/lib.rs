//! fux's server: one thread holds workspaces, tabs and split panes, their
//! programs, and the clients attached to them, and answers each request
//! in `fux-wire`'s terms. It never runs grift (D-2): programs run in
//! clients.

// fux-pane's `log!` and `wired!`, in every module.
#[macro_use]
extern crate fux_pane;

// The unit tests drive sessions through fux's grift; those that need a
// POSIX shell or a Unix fact are `cfg(unix)`, and Windows tests the server
// end to end besides (`tests/windows.rs`).
pub mod conn;
pub mod copy;
mod input;
pub mod journal;
pub mod link;
pub mod outer;
pub mod record;
pub mod render;
pub mod server;
pub mod session;
pub mod socket;
pub mod takeover;
pub mod view;
pub mod wait;
pub mod workspace;

// A pane's own code is fux-pane's, held here too for a session kept in
// one process: under its old names.
pub(crate) use fux_pane::after;
pub use fux_pane::{capture, host, pane, process};

use fuxix::process::Signal;

/// The signals that stop a server and detach a client.
pub const STOPS: [Signal; 3] = [Signal::Term, Signal::Int, Signal::Hup];

/// Takes every signal a signal pipe holds.
pub(crate) fn take_signals(pipe: &fuxix::signal::Pipe) {
    while pipe.next().is_some() {}
}

/// How the tests feed a parser and read its screen, as fux-vt's own do.
#[cfg(test)]
#[path = "../../fux-vt/tests/corpus/feed.rs"]
#[expect(dead_code, reason = "the tests use some of the helpers")]
mod vt_feed;

#[cfg(test)]
fn test_reference(value: &str) -> String {
    if value.starts_with('(') {
        return value.to_owned();
    }
    if matches!(value, "true" | "false") {
        return format!("#{value}");
    }
    if value == "all" {
        return "(all)".into();
    }
    if let Ok(number) = value.parse::<i64>()
        && !value.starts_with('+')
    {
        return number.to_string();
    }
    let constructor = match value.chars().next() {
        Some('%') => "pane",
        Some('@') => "tab",
        Some('+') => "workspace",
        Some('c')
            if value
                .get(1..)
                .is_some_and(|s| s.chars().all(|c| c.is_ascii_digit())) =>
        {
            "client"
        }
        _ => return fux::script::quoted(value),
    };
    format!("({constructor} {})", fux::script::quoted(value))
}

#[cfg(test)]
fn test_data(text: &str, query: &str) -> Result<String, String> {
    let mut g: grift::Grift = grift::Grift::new().map_err(|_| "initializing data reader")?;
    let forms = g
        .read_in("observed data", text)
        .map_err(|_| format!("invalid data: {text}"))?;
    let Some((data, grift::Value::EmptyList)) = g.as_pair(&forms).map_err(|_| "data forms")? else {
        return Err(format!("expected one datum: {text}"));
    };
    g.define("data", data).map_err(|_| "binding data")?;
    let program = format!(
        "(define! field (lambda! (record name) (second (find-association name record)))) {query}"
    );
    let value = g
        .evaluate(&mut (), &program)
        .map_err(|_| format!("query {query}: {text}"))?;
    if let Some(text) = g.as_str(&value).map_err(|_| "data string")? {
        return Ok(text.to_owned());
    }
    let mut out = String::new();
    g.write(&value, &mut out).map_err(|_| "data output")?;
    // A handle as a person reads it, without the run it carries.
    if matches!(value, grift::Value::Symbol(_))
        && let Some((id, _)) = out.split_once(':')
    {
        return Ok(id.trim_start_matches('w').to_owned());
    }
    Ok(out)
}

/// Pictures for the tests.
#[cfg(test)]
mod test_picture {
    /// A picture for the tests, `width` by `height` pixels of one colour, each
    /// channel off or full, drawn as a program draws one: sixel, read by
    /// fux-vt.
    pub(crate) fn solid(
        width: u32,
        height: u32,
        [r, g, b]: [bool; 3],
    ) -> Result<std::sync::Arc<fux_vt::sixel::Picture>, String> {
        use crate::vt_feed::Feed;
        let options = fux_vt::Options::from(fux_vt::Feature::Pictures);
        let size = fux_vt::Size::new(24, 80).map_err(|e| e.to_string())?;
        let mut parser =
            fux_vt::Parser::with_options(size, 0, options).map_err(|e| e.to_string())?;
        let cell = fux_vt::CellSize::new(10, 20).ok_or("a cell size")?;
        parser.set_host_graphics(fux_vt::Graphics::Sixel(cell));
        let full = |on| if on { 100 } else { 0 };
        let bands = usize::try_from(height.div_ceil(6)).map_err(|e| e.to_string())?;
        let bands = vec![format!("!{width}~"); bands].join("-");
        let (r, g, b) = (full(r), full(g), full(b));
        let sixel = format!("\x1bP0;1q\"1;1;{width};{height}#1;2;{r};{g};{b}#1{bands}\x1b\\");
        parser
            .process(sixel.as_bytes())
            .map_err(|e| e.to_string())?;
        let shown = parser.screen().pictures().next().ok_or("no picture")?;
        Ok(std::sync::Arc::clone(shown.picture))
    }
}
