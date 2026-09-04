use crate::state::Direction;
use std::collections::BTreeMap;

const PASTE_START: &[u8] = b"\x1b[200~";
const PASTE_END: &[u8] = b"\x1b[201~";
const MAX_PENDING: usize = 64;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Command {
    SplitHorizontal,
    SplitVertical,
    Focus(Direction),
    Close,
    NewPane,
    NewTab,
    NextTab,
    PreviousTab,
    Zoom,
    CopyMode,
    Detach,
    WorkspacePicker,
    Help,
    External(Vec<String>),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MouseEvent {
    pub code: u16,
    pub column: u16,
    pub row: u16,
    pub release: bool,
}

impl MouseEvent {
    #[must_use]
    pub const fn shift(self) -> bool {
        self.code & 4 != 0
    }
    #[must_use]
    pub const fn wheel(self) -> bool {
        self.code & 64 != 0
    }

    #[must_use]
    pub fn translated(self, column: u16, row: u16) -> Vec<u8> {
        format!(
            "\x1b[<{};{};{}{}",
            self.code,
            column,
            row,
            if self.release { 'm' } else { 'M' }
        )
        .into_bytes()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Action {
    Forward(Vec<u8>),
    Command(Command),
    Mouse(MouseEvent),
}

#[derive(Clone, Debug)]
pub struct InputRouter {
    prefix: u8,
    command_mode: bool,
    paste: bool,
    pending: Vec<u8>,
    pending_since_ms: Option<u64>,
    ambiguity_timeout_ms: u64,
    bindings: BTreeMap<u8, Command>,
}

impl InputRouter {
    #[must_use]
    pub fn new(prefix: u8, ambiguity_timeout_ms: u64) -> Self {
        Self::with_bindings(prefix, ambiguity_timeout_ms, default_bindings())
    }

    pub fn with_bindings(
        prefix: u8,
        ambiguity_timeout_ms: u64,
        bindings: BTreeMap<u8, Command>,
    ) -> Self {
        Self {
            prefix,
            command_mode: false,
            paste: false,
            pending: Vec::new(),
            pending_since_ms: None,
            ambiguity_timeout_ms,
            bindings,
        }
    }

    pub fn feed(&mut self, bytes: &[u8], now_ms: u64) -> Vec<Action> {
        let mut actions = self.flush_timeout(now_ms);
        for byte in bytes {
            self.feed_byte(*byte, now_ms, &mut actions);
        }
        coalesce(actions)
    }

    pub fn flush_timeout(&mut self, now_ms: u64) -> Vec<Action> {
        if self
            .pending_since_ms
            .is_some_and(|since| now_ms.saturating_sub(since) >= self.ambiguity_timeout_ms)
        {
            self.pending_since_ms = None;
            return self
                .pending
                .drain(..)
                .map(|byte| Action::Forward(vec![byte]))
                .collect();
        }
        Vec::new()
    }

    #[must_use]
    pub const fn has_pending_timeout(&self) -> bool {
        self.pending_since_ms.is_some()
    }

    fn feed_byte(&mut self, byte: u8, now_ms: u64, actions: &mut Vec<Action>) {
        if self.paste {
            self.pending.push(byte);
            if PASTE_END.starts_with(&self.pending) {
                if self.pending == PASTE_END {
                    actions.push(Action::Forward(std::mem::take(&mut self.pending)));
                    self.paste = false;
                    self.pending_since_ms = None;
                }
                return;
            }
            let first = self.pending.remove(0);
            actions.push(Action::Forward(vec![first]));
            return;
        }
        if self.command_mode {
            self.command_mode = false;
            if byte == self.prefix {
                actions.push(Action::Forward(vec![byte]));
            } else if let Some(command) = self.bindings.get(&byte).cloned() {
                actions.push(Action::Command(command));
            } else {
                actions.push(Action::Forward(vec![self.prefix, byte]));
            }
            return;
        }
        if self.pending.is_empty() && byte == self.prefix {
            self.command_mode = true;
            return;
        }
        if self.pending.is_empty() && byte != 0x1b {
            actions.push(Action::Forward(vec![byte]));
            return;
        }
        if self.pending.is_empty() {
            self.pending_since_ms = Some(now_ms);
        }
        self.pending.push(byte);
        if self.pending == PASTE_START {
            actions.push(Action::Forward(std::mem::take(&mut self.pending)));
            self.pending_since_ms = None;
            self.paste = true;
            return;
        }
        if let Some(mouse) = parse_mouse(&self.pending) {
            self.pending.clear();
            self.pending_since_ms = None;
            actions.push(Action::Mouse(mouse));
            return;
        }
        let possible = PASTE_START.starts_with(&self.pending)
            || b"\x1b[<".starts_with(&self.pending)
            || (self.pending.starts_with(b"\x1b[<")
                && self.pending.len() <= MAX_PENDING
                && !matches!(self.pending.last(), Some(b'M' | b'm')));
        if !possible || self.pending.len() >= MAX_PENDING {
            actions.push(Action::Forward(std::mem::take(&mut self.pending)));
            self.pending_since_ms = None;
        }
    }
}

fn command(byte: u8) -> Option<Command> {
    Some(match byte {
        b'|' => Command::SplitHorizontal,
        b'-' => Command::SplitVertical,
        b'h' => Command::Focus(Direction::Left),
        b'j' => Command::Focus(Direction::Down),
        b'k' => Command::Focus(Direction::Up),
        b'l' => Command::Focus(Direction::Right),
        b'x' => Command::Close,
        b'c' => Command::NewPane,
        b't' => Command::NewTab,
        b'n' => Command::NextTab,
        b'p' => Command::PreviousTab,
        b'z' => Command::Zoom,
        b'[' => Command::CopyMode,
        b'd' => Command::Detach,
        b's' => Command::WorkspacePicker,
        b'?' => Command::Help,
        _ => return None,
    })
}

fn default_bindings() -> BTreeMap<u8, Command> {
    (0_u8..=u8::MAX)
        .filter_map(|byte| command(byte).map(|command| (byte, command)))
        .collect()
}

fn parse_mouse(bytes: &[u8]) -> Option<MouseEvent> {
    let tail = bytes.strip_prefix(b"\x1b[<")?;
    let release = match tail.last()? {
        b'M' => false,
        b'm' => true,
        _ => return None,
    };
    let body = std::str::from_utf8(tail.get(..tail.len().checked_sub(1)?)?).ok()?;
    let mut fields = body.split(';');
    let code = fields.next()?.parse().ok()?;
    let column = fields.next()?.parse().ok()?;
    let row = fields.next()?.parse().ok()?;
    if fields.next().is_some() || column == 0 || row == 0 {
        return None;
    }
    Some(MouseEvent {
        code,
        column,
        row,
        release,
    })
}

fn coalesce(actions: Vec<Action>) -> Vec<Action> {
    let mut output = Vec::new();
    for action in actions {
        if let Action::Forward(bytes) = action {
            if let Some(Action::Forward(previous)) = output.last_mut() {
                previous.extend(bytes);
            } else {
                output.push(Action::Forward(bytes));
            }
        } else {
            output.push(action);
        }
    }
    output
}
