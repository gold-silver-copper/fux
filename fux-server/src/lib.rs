//! fux's server: one thread holds workspaces, tabs and split panes, their
//! programs, and the clients attached to them, and answers each request
//! in `fux-wire`'s terms. It never runs fuxth (D-2): programs run in
//! clients.

/// Writes to the server's log, its stderr: `log!("…", args)` writes the
/// line after `fux server: `. A log that cannot be written, a full disk,
/// say, loses the line and stops nothing.
macro_rules! log {
    ($($arg:tt)*) => {{
        let _ = std::io::Write::write_fmt(
            &mut std::io::stderr(),
            format_args!("fux server: {}\n", format_args!($($arg)*)),
        );
    }};
}

/// `wired!(Ty { Unit, Tuple(a, b), Named { x, y } })` puts an enum of the
/// server's on the wire, as fux-wire puts its own (`wire_enum!` there): a
/// tag, its variant's place in the list, then its fields in order; and
/// `wired!(struct Ty { a, b })`, a struct's fields in order. For what the
/// server says to its own panes' processes (`link`), never to a client.
macro_rules! wired {
    (struct $ty:path { $($f:ident),+ $(,)? }) => {
        impl fux_wire::wire::Wire for $ty {
            fn put(&self, out: &mut impl fux_wire::wire::Sink) {
                $(fux_wire::wire::Wire::put(&self.$f, out);)+
            }
            fn get(r: &mut fux_wire::wire::Reader<'_>) -> Result<Self, fux_wire::wire::Error> {
                Ok(Self { $($f: fux_wire::wire::Wire::get(r)?),+ })
            }
        }
    };
    ($ty:ident { $($var:ident $(( $($p:ident),+ ))? $({ $($f:ident),* })?),* $(,)? }) => {
        const _: () = {
            #[repr(u8)]
            enum Tag {
                $($var),*
            }
            impl fux_wire::wire::Wire for $ty {
                fn put(&self, out: &mut impl fux_wire::wire::Sink) {
                    match self {$(
                        Self::$var $(( $($p),+ ))? $({ $($f),* })? => {
                            out.push(Tag::$var as u8);
                            $($(fux_wire::wire::Wire::put($p, out);)+)?
                            $($(fux_wire::wire::Wire::put($f, out);)*)?
                        }
                    )*}
                }
                fn get(
                    r: &mut fux_wire::wire::Reader<'_>,
                ) -> Result<Self, fux_wire::wire::Error> {
                    let tag = <u8 as fux_wire::wire::Wire>::get(r)?;
                    $(if tag == Tag::$var as u8 {
                        $($(let $p = fux_wire::wire::Wire::get(r)?;)+)?
                        $($(let $f = fux_wire::wire::Wire::get(r)?;)*)?
                        return Ok(Self::$var $(( $($p),+ ))? $({ $($f),* })?);
                    })*
                    Err(fux_wire::wire::Error::Tag(stringify!($ty), tag))
                }
            }
        };
    };
}

// The unit tests (`cfg(all(test, unix))` throughout) drive sessions through
// fux's fuxth, whose client is Unix-only till WINDOWS.md's PR 8; Windows
// tests the server end to end instead (`tests/windows.rs`).
pub mod capture;
pub mod conn;
pub mod copy;
pub mod host;
mod input;
pub mod journal;
pub mod link;
pub mod outer;
pub mod pane;
pub mod process;
pub mod record;
pub mod render;
pub mod server;
pub mod session;
pub mod socket;
pub mod view;
pub mod wait;
pub mod workspace;

use fuxix::process::Signal;
use std::io::{PipeReader, Read};
use std::time::{Duration, Instant};

/// `wait` after `from`. A time too far off for an `Instant` is taken as
/// `from`, so that a deadline fires at once rather than never.
pub(crate) fn after(from: Instant, wait: Duration) -> Instant {
    from.checked_add(wait).unwrap_or(from)
}

/// The signals that stop a server and detach a client.
pub const STOPS: [Signal; 3] = [Signal::Term, Signal::Int, Signal::Hup];

/// Reads whatever a pipe holds.
pub fn drain(mut pipe: &PipeReader) {
    let mut buffer = [0u8; 256];
    while matches!(pipe.read(&mut buffer), Ok(n) if n > 0) {}
}

/// Takes every signal a signal pipe holds.
pub(crate) fn take_signals(pipe: &fuxix::signal::Pipe) {
    while pipe.next().is_some() {}
}

/// How the tests feed a parser and read its screen, as fux-vt's own do.
#[cfg(all(test, unix))]
#[path = "../../fux-vt/tests/corpus/feed.rs"]
#[expect(dead_code, reason = "the tests use some of the helpers")]
mod vt_feed;

/// Pictures for the tests.
#[cfg(all(test, unix))]
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
    let forms = g.list_items(&forms).map_err(|_| "data forms")?;
    let [data] = forms.as_slice() else {
        return Err(format!("expected one datum: {text}"));
    };
    g.define("data", data.clone()).map_err(|_| "binding data")?;
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
    Ok(out)
}
