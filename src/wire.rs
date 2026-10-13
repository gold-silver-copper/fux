//! Requests on the wire: a typed [`Command`] written as bytes and read back
//! as the same value, so that the server receives what a client's words
//! built, and never reads words itself. Big-endian numbers; a string or a
//! list is its length then its items; an option or an enum its tag first.
use crate::command::{
    AnyRef, CaptureView, Choices, ClientAction, Command, Extent, Input, Kind, Mouse, MoveTo,
    PanePick, PaneResize, Pick, Resized, Sibling, Subject, SwapWith, Until, Wait, WsRef,
};
use crate::id::{ClientId, PaneId, TabId, WsId};
use crate::keys::mouse::MouseButton;
use crate::keys::{Direction, KeyPress};
use crate::layout::Axis;
use std::time::Duration;

/// Why bytes are not a request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// The bytes end before the value does.
    Truncated,
    /// A tag no variant has.
    Tag(&'static str, u8),
    /// A string that is not UTF-8.
    NotUtf8,
    /// A value out of what its type takes (a key name, a size of 0).
    Bad(&'static str),
    /// Bytes after the value.
    Trailing,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Truncated => f.write_str("a request ends early"),
            Error::Tag(what, tag) => write!(f, "a request's {what} has no variant {tag}"),
            Error::NotUtf8 => f.write_str("a request's text is not UTF-8"),
            Error::Bad(what) => write!(f, "a request's {what} is out of range"),
            Error::Trailing => f.write_str("bytes after a request"),
        }
    }
}

impl std::error::Error for Error {}

/// Bytes being read.
pub struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    pub fn new(bytes: &'a [u8]) -> Reader<'a> {
        Reader(bytes)
    }
    fn take<const N: usize>(&mut self) -> Result<[u8; N], Error> {
        let (head, rest) = self.0.split_first_chunk().ok_or(Error::Truncated)?;
        self.0 = rest;
        Ok(*head)
    }
    fn bytes(&mut self, len: usize) -> Result<&'a [u8], Error> {
        let (head, rest) = self.0.split_at_checked(len).ok_or(Error::Truncated)?;
        self.0 = rest;
        Ok(head)
    }
    /// The whole value: no bytes may follow.
    pub fn whole<T: Wire>(mut self) -> Result<T, Error> {
        let value = T::get(&mut self)?;
        match self.0.is_empty() {
            true => Ok(value),
            false => Err(Error::Trailing),
        }
    }
}

/// A value that goes on the wire.
pub trait Wire: Sized {
    fn put(&self, out: &mut Vec<u8>);
    fn get(r: &mut Reader<'_>) -> Result<Self, Error>;
}

/// `value`'s bytes.
pub fn encode<T: Wire>(value: &T) -> Vec<u8> {
    let mut out = Vec::new();
    value.put(&mut out);
    out
}

fn tag(out: &mut Vec<u8>, tag: u8) {
    out.push(tag);
}

impl Wire for u8 {
    fn put(&self, out: &mut Vec<u8>) {
        out.push(*self);
    }
    fn get(r: &mut Reader<'_>) -> Result<Self, Error> {
        r.take::<1>().map(u8::from_be_bytes)
    }
}

impl Wire for u16 {
    fn put(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.to_be_bytes());
    }
    fn get(r: &mut Reader<'_>) -> Result<Self, Error> {
        r.take().map(u16::from_be_bytes)
    }
}

impl Wire for u32 {
    fn put(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.to_be_bytes());
    }
    fn get(r: &mut Reader<'_>) -> Result<Self, Error> {
        r.take().map(u32::from_be_bytes)
    }
}

impl Wire for u64 {
    fn put(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.to_be_bytes());
    }
    fn get(r: &mut Reader<'_>) -> Result<Self, Error> {
        r.take().map(u64::from_be_bytes)
    }
}

impl Wire for i64 {
    fn put(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.to_be_bytes());
    }
    fn get(r: &mut Reader<'_>) -> Result<Self, Error> {
        r.take().map(i64::from_be_bytes)
    }
}

impl Wire for usize {
    fn put(&self, out: &mut Vec<u8>) {
        u64::try_from(*self).unwrap_or(u64::MAX).put(out);
    }
    fn get(r: &mut Reader<'_>) -> Result<Self, Error> {
        usize::try_from(u64::get(r)?).map_err(|_| Error::Bad("count"))
    }
}

impl Wire for bool {
    fn put(&self, out: &mut Vec<u8>) {
        out.push(u8::from(*self));
    }
    fn get(r: &mut Reader<'_>) -> Result<Self, Error> {
        match u8::get(r)? {
            0 => Ok(false),
            1 => Ok(true),
            other => Err(Error::Tag("flag", other)),
        }
    }
}

impl Wire for String {
    fn put(&self, out: &mut Vec<u8>) {
        u32::try_from(self.len()).unwrap_or(u32::MAX).put(out);
        out.extend_from_slice(self.as_bytes());
    }
    fn get(r: &mut Reader<'_>) -> Result<Self, Error> {
        let len = u32::get(r)? as usize;
        let bytes = r.bytes(len)?;
        String::from_utf8(bytes.to_vec()).map_err(|_| Error::NotUtf8)
    }
}

impl<T: Wire> Wire for Option<T> {
    fn put(&self, out: &mut Vec<u8>) {
        match self {
            None => tag(out, 0),
            Some(value) => {
                tag(out, 1);
                value.put(out);
            }
        }
    }
    fn get(r: &mut Reader<'_>) -> Result<Self, Error> {
        match u8::get(r)? {
            0 => Ok(None),
            1 => T::get(r).map(Some),
            other => Err(Error::Tag("option", other)),
        }
    }
}

impl<T: Wire> Wire for Vec<T> {
    fn put(&self, out: &mut Vec<u8>) {
        u32::try_from(self.len()).unwrap_or(u32::MAX).put(out);
        for item in self {
            item.put(out);
        }
    }
    fn get(r: &mut Reader<'_>) -> Result<Self, Error> {
        let count = u32::get(r)? as usize;
        // Every item takes a byte at least: a count past the bytes left is
        // a lie, not room to make.
        if count > r.0.len() {
            return Err(Error::Truncated);
        }
        (0..count).map(|_| T::get(r)).collect()
    }
}

impl<T: Wire> Wire for Box<T> {
    fn put(&self, out: &mut Vec<u8>) {
        (**self).put(out);
    }
    fn get(r: &mut Reader<'_>) -> Result<Self, Error> {
        T::get(r).map(Box::new)
    }
}

impl<A: Wire, B: Wire> Wire for (A, B) {
    fn put(&self, out: &mut Vec<u8>) {
        self.0.put(out);
        self.1.put(out);
    }
    fn get(r: &mut Reader<'_>) -> Result<Self, Error> {
        Ok((A::get(r)?, B::get(r)?))
    }
}

impl<A: Wire, B: Wire, C: Wire> Wire for (A, B, C) {
    fn put(&self, out: &mut Vec<u8>) {
        self.0.put(out);
        self.1.put(out);
        self.2.put(out);
    }
    fn get(r: &mut Reader<'_>) -> Result<Self, Error> {
        Ok((A::get(r)?, B::get(r)?, C::get(r)?))
    }
}

impl Wire for Duration {
    fn put(&self, out: &mut Vec<u8>) {
        u64::try_from(self.as_millis()).unwrap_or(u64::MAX).put(out);
    }
    fn get(r: &mut Reader<'_>) -> Result<Self, Error> {
        u64::get(r).map(Duration::from_millis)
    }
}

/// An id: its number, read back through its text (`%3`).
macro_rules! id_wire {
    ($ty:ty, $sigil:literal) => {
        impl Wire for $ty {
            fn put(&self, out: &mut Vec<u8>) {
                self.to_string().put(out);
            }
            fn get(r: &mut Reader<'_>) -> Result<Self, Error> {
                String::get(r)?.parse().map_err(|_| Error::Bad($sigil))
            }
        }
    };
}
id_wire!(PaneId, "pane id");
id_wire!(TabId, "tab id");
id_wire!(WsId, "workspace id");
id_wire!(ClientId, "client id");

impl Wire for KeyPress {
    fn put(&self, out: &mut Vec<u8>) {
        self.to_string().put(out);
    }
    fn get(r: &mut Reader<'_>) -> Result<Self, Error> {
        String::get(r)?.parse().map_err(|_| Error::Bad("key"))
    }
}

/// An enum of plain variants, as its index in `$all`.
macro_rules! plain_wire {
    ($ty:ty, $what:literal, [$($variant:expr),+ $(,)?]) => {
        impl Wire for $ty {
            fn put(&self, out: &mut Vec<u8>) {
                let all = [$($variant),+];
                let index = all.iter().position(|v| v == self).unwrap_or(0);
                out.push(u8::try_from(index).unwrap_or(0));
            }
            fn get(r: &mut Reader<'_>) -> Result<Self, Error> {
                let all = [$($variant),+];
                let index = u8::get(r)?;
                all.get(usize::from(index)).copied().ok_or(Error::Tag($what, index))
            }
        }
    };
}
plain_wire!(
    Direction,
    "direction",
    [
        Direction::Left,
        Direction::Right,
        Direction::Up,
        Direction::Down
    ]
);
plain_wire!(Axis, "axis", [Axis::Horizontal, Axis::Vertical]);
plain_wire!(Kind, "kind", [Kind::Pane, Kind::Tab, Kind::Workspace]);
plain_wire!(Sibling, "sibling", [Sibling::Next, Sibling::Previous]);
plain_wire!(
    MouseButton,
    "mouse button",
    [
        MouseButton::Left,
        MouseButton::Middle,
        MouseButton::Right,
        MouseButton::WheelUp,
        MouseButton::WheelDown,
        MouseButton::WheelLeft,
        MouseButton::WheelRight,
    ]
);
plain_wire!(
    fuxix::process::Signal,
    "signal",
    [
        fuxix::process::Signal::Hup,
        fuxix::process::Signal::Int,
        fuxix::process::Signal::Term,
        fuxix::process::Signal::Kill,
        fuxix::process::Signal::Stop,
        fuxix::process::Signal::Tstp,
        fuxix::process::Signal::Cont,
        fuxix::process::Signal::Winch,
        fuxix::process::Signal::Quit,
        fuxix::process::Signal::Usr1,
        fuxix::process::Signal::Usr2,
    ]
);

impl Wire for fux_vt::Size {
    fn put(&self, out: &mut Vec<u8>) {
        self.rows().put(out);
        self.cols().put(out);
    }
    fn get(r: &mut Reader<'_>) -> Result<Self, Error> {
        let (rows, cols) = (u16::get(r)?, u16::get(r)?);
        fux_vt::Size::new(rows, cols).map_err(|_| Error::Bad("size"))
    }
}

impl Wire for WsRef {
    fn put(&self, out: &mut Vec<u8>) {
        match self {
            WsRef::Id(id) => {
                tag(out, 0);
                id.put(out);
            }
            WsRef::Name(name) => {
                tag(out, 1);
                name.put(out);
            }
        }
    }
    fn get(r: &mut Reader<'_>) -> Result<Self, Error> {
        match u8::get(r)? {
            0 => WsId::get(r).map(WsRef::Id),
            1 => String::get(r).map(WsRef::Name),
            other => Err(Error::Tag("workspace", other)),
        }
    }
}

impl<W: Wire> Wire for AnyRef<W> {
    fn put(&self, out: &mut Vec<u8>) {
        match self {
            AnyRef::Pane(p) => {
                tag(out, 0);
                p.put(out);
            }
            AnyRef::Tab(t) => {
                tag(out, 1);
                t.put(out);
            }
            AnyRef::Workspace(w) => {
                tag(out, 2);
                w.put(out);
            }
        }
    }
    fn get(r: &mut Reader<'_>) -> Result<Self, Error> {
        match u8::get(r)? {
            0 => PaneId::get(r).map(AnyRef::Pane),
            1 => TabId::get(r).map(AnyRef::Tab),
            2 => W::get(r).map(AnyRef::Workspace),
            other => Err(Error::Tag("target", other)),
        }
    }
}

impl<T: Wire> Wire for Pick<T> {
    fn put(&self, out: &mut Vec<u8>) {
        match self {
            Pick::Step(s) => {
                tag(out, 0);
                s.put(out);
            }
            Pick::Id(id) => {
                tag(out, 1);
                id.put(out);
            }
        }
    }
    fn get(r: &mut Reader<'_>) -> Result<Self, Error> {
        match u8::get(r)? {
            0 => Sibling::get(r).map(Pick::Step),
            1 => T::get(r).map(Pick::Id),
            other => Err(Error::Tag("pick", other)),
        }
    }
}

impl Wire for PanePick {
    fn put(&self, out: &mut Vec<u8>) {
        match self {
            PanePick::Step(s) => {
                tag(out, 0);
                s.put(out);
            }
            PanePick::Id(id) => {
                tag(out, 1);
                id.put(out);
            }
            PanePick::Last => tag(out, 2),
            PanePick::Toward(d) => {
                tag(out, 3);
                d.put(out);
            }
        }
    }
    fn get(r: &mut Reader<'_>) -> Result<Self, Error> {
        match u8::get(r)? {
            0 => Sibling::get(r).map(PanePick::Step),
            1 => PaneId::get(r).map(PanePick::Id),
            2 => Ok(PanePick::Last),
            3 => Direction::get(r).map(PanePick::Toward),
            other => Err(Error::Tag("pane pick", other)),
        }
    }
}

impl Wire for MoveTo {
    fn put(&self, out: &mut Vec<u8>) {
        match self {
            MoveTo::Tab(t) => {
                tag(out, 0);
                t.put(out);
            }
            MoveTo::Workspace(w) => {
                tag(out, 1);
                w.put(out);
            }
            MoveTo::NewTab => tag(out, 2),
            MoveTo::NewWorkspace => tag(out, 3),
            MoveTo::Beside(d) => {
                tag(out, 4);
                d.put(out);
            }
        }
    }
    fn get(r: &mut Reader<'_>) -> Result<Self, Error> {
        match u8::get(r)? {
            0 => TabId::get(r).map(MoveTo::Tab),
            1 => WsRef::get(r).map(MoveTo::Workspace),
            2 => Ok(MoveTo::NewTab),
            3 => Ok(MoveTo::NewWorkspace),
            4 => Direction::get(r).map(MoveTo::Beside),
            other => Err(Error::Tag("move", other)),
        }
    }
}

impl Wire for SwapWith {
    fn put(&self, out: &mut Vec<u8>) {
        match self {
            SwapWith::Pane(p) => {
                tag(out, 0);
                p.put(out);
            }
            SwapWith::Toward(d) => {
                tag(out, 1);
                d.put(out);
            }
        }
    }
    fn get(r: &mut Reader<'_>) -> Result<Self, Error> {
        match u8::get(r)? {
            0 => PaneId::get(r).map(SwapWith::Pane),
            1 => Direction::get(r).map(SwapWith::Toward),
            other => Err(Error::Tag("swap", other)),
        }
    }
}

impl Wire for Subject {
    fn put(&self, out: &mut Vec<u8>) {
        match self {
            Subject::Pane(p) => {
                tag(out, 0);
                p.put(out);
            }
            Subject::Tab(t) => {
                tag(out, 1);
                t.put(out);
            }
            Subject::Workspace(w) => {
                tag(out, 2);
                w.put(out);
            }
        }
    }
    fn get(r: &mut Reader<'_>) -> Result<Self, Error> {
        match u8::get(r)? {
            0 => Option::get(r).map(Subject::Pane),
            1 => Option::get(r).map(Subject::Tab),
            2 => Option::get(r).map(Subject::Workspace),
            other => Err(Error::Tag("subject", other)),
        }
    }
}

impl Wire for Extent {
    fn put(&self, out: &mut Vec<u8>) {
        match self {
            Extent::Cells(n) => {
                tag(out, 0);
                n.put(out);
            }
            Extent::Percent(n) => {
                tag(out, 1);
                n.put(out);
            }
        }
    }
    fn get(r: &mut Reader<'_>) -> Result<Self, Error> {
        match u8::get(r)? {
            0 => u16::get(r).map(Extent::Cells),
            1 => u8::get(r).map(Extent::Percent),
            other => Err(Error::Tag("extent", other)),
        }
    }
}

impl Wire for PaneResize {
    fn put(&self, out: &mut Vec<u8>) {
        match self {
            PaneResize::Border(d, n) => {
                tag(out, 0);
                d.put(out);
                n.put(out);
            }
            PaneResize::To { cols, rows } => {
                tag(out, 1);
                cols.put(out);
                rows.put(out);
            }
        }
    }
    fn get(r: &mut Reader<'_>) -> Result<Self, Error> {
        match u8::get(r)? {
            0 => Ok(PaneResize::Border(Direction::get(r)?, u16::get(r)?)),
            1 => Ok(PaneResize::To {
                cols: Option::get(r)?,
                rows: Option::get(r)?,
            }),
            other => Err(Error::Tag("resize", other)),
        }
    }
}

impl Wire for Resized {
    fn put(&self, out: &mut Vec<u8>) {
        match self {
            Resized::Tabs(t) => {
                tag(out, 0);
                t.put(out);
            }
            Resized::Client(c) => {
                tag(out, 1);
                c.put(out);
            }
        }
    }
    fn get(r: &mut Reader<'_>) -> Result<Self, Error> {
        match u8::get(r)? {
            0 => AnyRef::get(r).map(Resized::Tabs),
            1 => ClientId::get(r).map(Resized::Client),
            other => Err(Error::Tag("resized", other)),
        }
    }
}

impl Wire for Input {
    fn put(&self, out: &mut Vec<u8>) {
        match self {
            Input::Key(k) => {
                tag(out, 0);
                k.put(out);
            }
            Input::Text(t) => {
                tag(out, 1);
                t.put(out);
            }
            Input::Paste(t) => {
                tag(out, 2);
                t.put(out);
            }
        }
    }
    fn get(r: &mut Reader<'_>) -> Result<Self, Error> {
        match u8::get(r)? {
            0 => KeyPress::get(r).map(Input::Key),
            1 => String::get(r).map(Input::Text),
            2 => String::get(r).map(Input::Paste),
            other => Err(Error::Tag("input", other)),
        }
    }
}

impl Wire for CaptureView {
    fn put(&self, out: &mut Vec<u8>) {
        let flags = [
            self.full,
            self.join,
            self.styles,
            self.main,
            self.alt,
            self.last_output,
            self.marks,
        ];
        for flag in flags {
            flag.put(out);
        }
        self.rows.put(out);
        self.cols.put(out);
    }
    fn get(r: &mut Reader<'_>) -> Result<Self, Error> {
        Ok(CaptureView {
            full: bool::get(r)?,
            join: bool::get(r)?,
            styles: bool::get(r)?,
            main: bool::get(r)?,
            alt: bool::get(r)?,
            last_output: bool::get(r)?,
            marks: bool::get(r)?,
            rows: Option::get(r)?,
            cols: Option::get(r)?,
        })
    }
}

impl Wire for Until {
    fn put(&self, out: &mut Vec<u8>) {
        match self {
            Until::Text(t) => {
                tag(out, 0);
                t.put(out);
            }
            Until::Gone(t) => {
                tag(out, 1);
                t.put(out);
            }
            Until::Idle(d) => {
                tag(out, 2);
                d.put(out);
            }
            Until::Change(n) => {
                tag(out, 3);
                n.put(out);
            }
            Until::Exit => tag(out, 4),
            Until::Done => tag(out, 5),
            Until::Bell => tag(out, 6),
            Until::Settled => tag(out, 7),
        }
    }
    fn get(r: &mut Reader<'_>) -> Result<Self, Error> {
        match u8::get(r)? {
            0 => String::get(r).map(Until::Text),
            1 => String::get(r).map(Until::Gone),
            2 => Duration::get(r).map(Until::Idle),
            3 => Option::get(r).map(Until::Change),
            4 => Ok(Until::Exit),
            5 => Ok(Until::Done),
            6 => Ok(Until::Bell),
            7 => Ok(Until::Settled),
            other => Err(Error::Tag("condition", other)),
        }
    }
}

impl Wire for Wait {
    fn put(&self, out: &mut Vec<u8>) {
        self.until.put(out);
        self.timeout.put(out);
        for flag in [self.print, self.after, self.marks, self.all] {
            flag.put(out);
        }
        self.echo.put(out);
        self.lines.put(out);
    }
    fn get(r: &mut Reader<'_>) -> Result<Self, Error> {
        Ok(Wait {
            until: Vec::get(r)?,
            timeout: Duration::get(r)?,
            print: bool::get(r)?,
            after: bool::get(r)?,
            marks: bool::get(r)?,
            all: bool::get(r)?,
            echo: Vec::get(r)?,
            lines: u32::get(r)?,
        })
    }
}

impl Wire for Mouse {
    fn put(&self, out: &mut Vec<u8>) {
        match self {
            Mouse::Click(b) => {
                tag(out, 0);
                b.put(out);
            }
            Mouse::Press(b) => {
                tag(out, 1);
                b.put(out);
            }
            Mouse::Release(b) => {
                tag(out, 2);
                b.put(out);
            }
            Mouse::Wheel(b) => {
                tag(out, 3);
                b.put(out);
            }
            Mouse::Move => tag(out, 4),
        }
    }
    fn get(r: &mut Reader<'_>) -> Result<Self, Error> {
        match u8::get(r)? {
            0 => MouseButton::get(r).map(Mouse::Click),
            1 => MouseButton::get(r).map(Mouse::Press),
            2 => MouseButton::get(r).map(Mouse::Release),
            3 => MouseButton::get(r).map(Mouse::Wheel),
            4 => Ok(Mouse::Move),
            other => Err(Error::Tag("mouse", other)),
        }
    }
}

impl Wire for ClientAction {
    fn put(&self, out: &mut Vec<u8>) {
        match self {
            ClientAction::Detach => tag(out, 0),
            ClientAction::Capture { json } => {
                tag(out, 1);
                json.put(out);
            }
            ClientAction::Choices(choices) => {
                tag(out, 2);
                match choices {
                    Choices::Tabs => tag(out, 0),
                    Choices::Workspaces => tag(out, 1),
                    Choices::Panes(pane) => {
                        tag(out, 2);
                        pane.put(out);
                    }
                }
            }
            ClientAction::Run(code) => {
                tag(out, 3);
                code.put(out);
            }
            ClientAction::Zoom => tag(out, 10),
            ClientAction::SelectPane(p) => {
                tag(out, 11);
                p.put(out);
            }
            ClientAction::SelectTab(p) => {
                tag(out, 12);
                p.put(out);
            }
            ClientAction::SelectWorkspace(p) => {
                tag(out, 13);
                p.put(out);
            }
        }
    }
    fn get(r: &mut Reader<'_>) -> Result<Self, Error> {
        match u8::get(r)? {
            0 => Ok(ClientAction::Detach),
            1 => Ok(ClientAction::Capture {
                json: bool::get(r)?,
            }),
            2 => Ok(ClientAction::Choices(match u8::get(r)? {
                0 => Choices::Tabs,
                1 => Choices::Workspaces,
                2 => Choices::Panes(PaneId::get(r)?),
                other => return Err(Error::Tag("choices", other)),
            })),
            3 => String::get(r).map(ClientAction::Run),
            10 => Ok(ClientAction::Zoom),
            11 => PanePick::get(r).map(ClientAction::SelectPane),
            12 => Pick::get(r).map(ClientAction::SelectTab),
            13 => Pick::get(r).map(ClientAction::SelectWorkspace),
            other => Err(Error::Tag("client action", other)),
        }
    }
}

impl Wire for Command {
    fn put(&self, out: &mut Vec<u8>) {
        match self {
            Command::Ls { json, target, get } => {
                tag(out, 0);
                json.put(out);
                target.put(out);
                get.put(out);
            }
            Command::KillServer { force } => {
                tag(out, 1);
                force.put(out);
            }
            Command::NewWorkspace {
                name,
                cmd,
                size,
                cwd,
                env,
                ids,
                json,
            } => {
                tag(out, 2);
                name.put(out);
                cmd.put(out);
                size.put(out);
                cwd.put(out);
                env.put(out);
                ids.put(out);
                json.put(out);
            }
            Command::NewTab {
                target,
                name,
                cmd,
                size,
                cwd,
                env,
                ids,
                json,
            } => {
                tag(out, 3);
                target.put(out);
                name.put(out);
                cmd.put(out);
                size.put(out);
                cwd.put(out);
                env.put(out);
                ids.put(out);
                json.put(out);
            }
            Command::Split {
                axis,
                target,
                cmd,
                cwd,
                env,
                run,
                ids,
                json,
            } => {
                tag(out, 4);
                axis.put(out);
                target.put(out);
                cmd.put(out);
                cwd.put(out);
                env.put(out);
                run.put(out);
                ids.put(out);
                json.put(out);
            }
            Command::Run {
                workspace,
                name,
                size,
                cwd,
                env,
                argv,
                json,
                log,
                ids,
                then,
            } => {
                tag(out, 5);
                workspace.put(out);
                name.put(out);
                size.put(out);
                cwd.put(out);
                env.put(out);
                argv.put(out);
                json.put(out);
                log.put(out);
                ids.put(out);
                then.put(out);
            }
            Command::Respawn {
                target,
                kill,
                env,
                argv,
                then,
            } => {
                tag(out, 6);
                target.put(out);
                kill.put(out);
                env.put(out);
                argv.put(out);
                then.put(out);
            }
            Command::Notify { target, text } => {
                tag(out, 7);
                target.put(out);
                text.put(out);
            }
            Command::Resize { what, size } => {
                tag(out, 8);
                what.put(out);
                size.put(out);
            }
            Command::Wait { targets, wait } => {
                tag(out, 9);
                targets.put(out);
                wait.put(out);
            }
            Command::SendMouse {
                target,
                if_revision,
                mouse,
                row,
                col,
                then,
            } => {
                tag(out, 10);
                target.put(out);
                if_revision.put(out);
                mouse.put(out);
                row.put(out);
                col.put(out);
                then.put(out);
            }
            Command::KillPane { target } => {
                tag(out, 11);
                target.put(out);
            }
            Command::Signal {
                target,
                signal,
                then,
            } => {
                tag(out, 12);
                target.put(out);
                signal.put(out);
                then.put(out);
            }
            Command::KillTab { target } => {
                tag(out, 13);
                target.put(out);
            }
            Command::KillWorkspace { target } => {
                tag(out, 14);
                target.put(out);
            }
            Command::Rename { target, name } => {
                tag(out, 15);
                target.put(out);
                name.put(out);
            }
            Command::MovePane { target, to } => {
                tag(out, 16);
                target.put(out);
                to.put(out);
            }
            Command::SwapPane { target, with } => {
                tag(out, 17);
                target.put(out);
                with.put(out);
            }
            Command::ResizePane { target, by } => {
                tag(out, 18);
                target.put(out);
                by.put(out);
            }
            Command::SendKeys {
                targets,
                client,
                if_revision,
                inputs,
                repeat,
                then,
            } => {
                tag(out, 19);
                targets.put(out);
                client.put(out);
                if_revision.put(out);
                inputs.put(out);
                repeat.put(out);
                then.put(out);
            }
            Command::Events { targets, json } => {
                tag(out, 20);
                targets.put(out);
                json.put(out);
            }
            Command::DumpLayout { target } => {
                tag(out, 21);
                target.put(out);
            }
            Command::Grant {
                workspace,
                socket,
                revoke,
            } => {
                tag(out, 22);
                workspace.put(out);
                socket.put(out);
                revoke.put(out);
            }
            Command::Log { target, file } => {
                tag(out, 23);
                target.put(out);
                file.put(out);
            }
            Command::ClearHistory { target } => {
                tag(out, 24);
                target.put(out);
            }
            Command::CapturePane {
                target,
                history,
                view,
                json,
                settle,
            } => {
                tag(out, 25);
                target.put(out);
                history.put(out);
                view.put(out);
                json.put(out);
                settle.put(out);
            }
            Command::Reorder { subject, toward } => {
                tag(out, 26);
                subject.put(out);
                toward.put(out);
            }
            Command::Set { option, value } => {
                tag(out, 27);
                option.put(out);
                value.put(out);
            }
            Command::Resolve { text, kind } => {
                tag(out, 28);
                text.put(out);
                kind.put(out);
            }
            Command::ListBuffers => tag(out, 29),
            Command::ShowBuffer { index } => {
                tag(out, 30);
                index.put(out);
            }
            Command::PasteBuffer { index, target } => {
                tag(out, 31);
                index.put(out);
                target.put(out);
            }
            Command::Client { client, action } => {
                tag(out, 32);
                client.put(out);
                action.put(out);
            }
        }
    }

    fn get(r: &mut Reader<'_>) -> Result<Self, Error> {
        Ok(match u8::get(r)? {
            0 => Command::Ls {
                json: bool::get(r)?,
                target: Option::get(r)?,
                get: Option::get(r)?,
            },
            1 => Command::KillServer {
                force: bool::get(r)?,
            },
            2 => Command::NewWorkspace {
                name: Option::get(r)?,
                cmd: Vec::get(r)?,
                size: Option::get(r)?,
                cwd: Option::get(r)?,
                env: Vec::get(r)?,
                ids: Vec::get(r)?,
                json: bool::get(r)?,
            },
            3 => Command::NewTab {
                target: Option::get(r)?,
                name: Option::get(r)?,
                cmd: Vec::get(r)?,
                size: Option::get(r)?,
                cwd: Option::get(r)?,
                env: Vec::get(r)?,
                ids: Vec::get(r)?,
                json: bool::get(r)?,
            },
            4 => Command::Split {
                axis: Axis::get(r)?,
                target: Option::get(r)?,
                cmd: Vec::get(r)?,
                cwd: Option::get(r)?,
                env: Vec::get(r)?,
                run: bool::get(r)?,
                ids: Vec::get(r)?,
                json: bool::get(r)?,
            },
            5 => Command::Run {
                workspace: Option::get(r)?,
                name: Option::get(r)?,
                size: Option::get(r)?,
                cwd: Option::get(r)?,
                env: Vec::get(r)?,
                argv: Vec::get(r)?,
                json: bool::get(r)?,
                log: Option::get(r)?,
                ids: Vec::get(r)?,
                then: Option::get(r)?,
            },
            6 => Command::Respawn {
                target: Option::get(r)?,
                kill: bool::get(r)?,
                env: Vec::get(r)?,
                argv: Vec::get(r)?,
                then: Option::get(r)?,
            },
            7 => Command::Notify {
                target: Option::get(r)?,
                text: String::get(r)?,
            },
            8 => Command::Resize {
                what: Resized::get(r)?,
                size: Option::get(r)?,
            },
            9 => Command::Wait {
                targets: Vec::get(r)?,
                wait: Wait::get(r)?,
            },
            10 => Command::SendMouse {
                target: Option::get(r)?,
                if_revision: Option::get(r)?,
                mouse: Mouse::get(r)?,
                row: u16::get(r)?,
                col: u16::get(r)?,
                then: Option::get(r)?,
            },
            11 => Command::KillPane {
                target: Option::get(r)?,
            },
            12 => Command::Signal {
                target: Option::get(r)?,
                signal: fuxix::process::Signal::get(r)?,
                then: Option::get(r)?,
            },
            13 => Command::KillTab {
                target: Option::get(r)?,
            },
            14 => Command::KillWorkspace {
                target: Option::get(r)?,
            },
            15 => Command::Rename {
                target: AnyRef::get(r)?,
                name: String::get(r)?,
            },
            16 => Command::MovePane {
                target: Option::get(r)?,
                to: MoveTo::get(r)?,
            },
            17 => Command::SwapPane {
                target: Option::get(r)?,
                with: SwapWith::get(r)?,
            },
            18 => Command::ResizePane {
                target: Option::get(r)?,
                by: PaneResize::get(r)?,
            },
            19 => Command::SendKeys {
                targets: Vec::get(r)?,
                client: Option::get(r)?,
                if_revision: Option::get(r)?,
                inputs: Vec::get(r)?,
                repeat: u16::get(r)?,
                then: Option::get(r)?,
            },
            20 => Command::Events {
                targets: Vec::get(r)?,
                json: bool::get(r)?,
            },
            21 => Command::DumpLayout {
                target: Option::get(r)?,
            },
            22 => Command::Grant {
                workspace: Option::get(r)?,
                socket: String::get(r)?,
                revoke: bool::get(r)?,
            },
            23 => Command::Log {
                target: Option::get(r)?,
                file: Option::get(r)?,
            },
            24 => Command::ClearHistory {
                target: Option::get(r)?,
            },
            25 => Command::CapturePane {
                target: Option::get(r)?,
                history: Option::get(r)?,
                view: CaptureView::get(r)?,
                json: bool::get(r)?,
                settle: Option::get(r)?,
            },
            26 => Command::Reorder {
                subject: Subject::get(r)?,
                toward: Sibling::get(r)?,
            },
            27 => Command::Set {
                option: String::get(r)?,
                value: String::get(r)?,
            },
            28 => Command::Resolve {
                text: String::get(r)?,
                kind: Kind::get(r)?,
            },
            29 => Command::ListBuffers,
            30 => Command::ShowBuffer {
                index: usize::get(r)?,
            },
            31 => Command::PasteBuffer {
                index: usize::get(r)?,
                target: Option::get(r)?,
            },
            32 => Command::Client {
                client: Option::get(r)?,
                action: ClientAction::get(r)?,
            },
            other => return Err(Error::Tag("command", other)),
        })
    }
}
