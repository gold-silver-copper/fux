//! A client's connection's own process (D-123): what a client can make
//! grow is held here, never in the server. It decodes what the client
//! sends, each frame as the one its stage expects, and sends the server
//! the frames whole; it holds what is to go to the client until the client
//! reads it; and an attached client's screen as it shows it, which each
//! screen the server composes is painted against. A client that floods
//! it, or stops reading, ends only its connection. It ends with its
//! server: a server that crashes leaves it nothing to serve.
use crate::link::{End, Expect, FromConn, Paint, Read, ToConn};
use crate::render::Grid;
use fux_vt::bytes::ByteQueue;
use fux_wire::protocol::{Asked, Attach, AttachedFrame, Decoder, Frame, Hello, ToAttached};
use std::os::unix::net::UnixStream;

/// The most it reads from its client before it looks at its server.
const READ: usize = 256 * 1024;

/// What it decodes next of its client's.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Want {
    Hello,
    Frames(Expect),
    /// A request is under way: its client's next waits for its answer,
    /// written to the client whole (`answered`).
    Answer,
    /// Nothing more till the server says.
    Nothing,
}

/// An attached client's screen as it shows it, and its pictures.
#[derive(Default)]
struct Screen {
    shown: Grid,
    /// Whether `shown` is what the client's terminal shows.
    known: bool,
    pictures: Vec<(u64, (u16, u16, u16, u16))>,
    /// The sixel data of each picture shown, by its identity.
    data: std::collections::BTreeMap<u64, Vec<u8>>,
}

impl Screen {
    /// Paints `paint` onto the client's terminal, as it differs from what
    /// it shows: a picture gone or moved has its cells painted again,
    /// blank, which takes its pixels away; one whose cells this paint
    /// paints over, or new, is sent after the text.
    fn paint(&mut self, paint: Paint, out: &mut ByteQueue) {
        let Paint {
            before,
            grid: spare,
            pictures,
            forget,
        } = paint;
        if forget {
            self.known = false;
        }
        if !before.is_empty() {
            fux_wire::protocol::split_into(&before, ToAttached::Paint, out);
        }
        let known = self.known;
        for shown in &pictures {
            if let Some(data) = &shown.data {
                self.data.insert(shown.id, data.clone());
            }
        }
        let now: Vec<(u64, (u16, u16, u16, u16))> =
            pictures.iter().map(|p| (p.id, p.area())).collect();
        for (_, area) in (self.pictures.iter()).filter(|p| known && !now.contains(p)) {
            self.shown.forget_area(*area);
        }
        let send: Vec<bool> = (now.iter())
            .map(|p| {
                !known
                    || spare.clears(&self.shown)
                    || !self.pictures.contains(p)
                    || !spare.same_in(&self.shown, p.1)
            })
            .collect();
        let mut bytes = Vec::new();
        let changed = crate::render::paint_into(&self.shown, known, &spare, &mut bytes);
        self.shown = spare;
        if changed {
            fux_wire::protocol::split_into(&bytes, ToAttached::Paint, out);
            self.known = true;
        }
        let mut drawn = Vec::new();
        for (picture, _) in now.iter().zip(&send).filter(|(_, send)| **send) {
            if let Some(data) = self.data.get(&picture.0) {
                drawn.extend_from_slice(data);
            }
        }
        if !drawn.is_empty() {
            fux_wire::protocol::split_into(&drawn, ToAttached::Paint, out);
        }
        self.data
            .retain(|id, _| now.iter().any(|(shown, _)| shown == id));
        self.pictures = now;
    }

    /// Its terminal's cursor overrides given back as its own, at detach;
    /// one never set is left as it was.
    fn restore(&self, out: &mut ByteQueue) {
        let mut cursor = Vec::new();
        let modes = &self.shown.modes;
        crate::render::cursor_shape(
            modes.cursor_shape,
            fux_vt::CursorShape::Initial,
            &mut cursor,
        );
        crate::render::cursor_colour(modes.cursor_colour, None, &mut cursor);
        if !cursor.is_empty() {
            fux_wire::protocol::split_into(&cursor, ToAttached::Paint, out);
        }
    }
}

/// The next whole frame `decoder` holds of those `want` expects, sent
/// again as it came: none if more bytes are needed, or none is expected.
fn next_frame(decoder: &mut Decoder, want: Want) -> Result<Option<Vec<u8>>, String> {
    fn again<'a, F: Frame<'a>>(frame: Option<F>) -> Option<Vec<u8>> {
        frame.map(|frame| frame.encode())
    }
    let frame = match want {
        Want::Hello => again(decoder.frame::<Hello>().map_err(|e| e.to_string())?),
        Want::Frames(Expect::Attach) => {
            again(decoder.frame::<Attach>().map_err(|e| e.to_string())?)
        }
        Want::Frames(Expect::Asked) => again(decoder.frame::<Asked>().map_err(|e| e.to_string())?),
        Want::Frames(Expect::Attached) => again(
            decoder
                .frame::<AttachedFrame>()
                .map_err(|e| e.to_string())?,
        ),
        Want::Answer | Want::Nothing => None,
    };
    Ok(frame)
}

/// The loop of a client's connection's own process (`fux __conn`): its
/// client on `client`, its server on `server`, until either goes.
pub fn serve(client: UnixStream, server: UnixStream) -> Result<(), String> {
    use fuxix::poll::{Events, PollFd};
    client
        .set_nonblocking(true)
        .map_err(|e| format!("its client: {e}"))?;
    let mut link = End::new(server).map_err(|e| format!("its server: {e}"))?;
    let mut decoder = Decoder::default();
    let mut out = ByteQueue::default();
    let mut screen = Screen::default();
    let mut want = Want::Hello;
    let (mut answered, mut unflushed, mut shut, mut closing) = (false, false, false, false);
    // Its client shut its side, and its server is told so once what it
    // sent before is taken (`Shut`); its server gone, once its client has
    // what the server sent it.
    let (mut ended, mut told_shut, mut alone) = (false, false, false);
    let mut buffer = vec![0u8; READ];
    let mut poll = fuxix::poll::Poll::default();
    loop {
        // What its client sent, as far as its stage lets it be taken.
        loop {
            match next_frame(&mut decoder, want) {
                Ok(Some(frame)) => {
                    link.send(&FromConn::Frames(frame));
                    want = match want {
                        Want::Hello => Want::Nothing,
                        Want::Frames(Expect::Asked) => {
                            answered = false;
                            Want::Answer
                        }
                        Want::Frames(Expect::Attach) => Want::Nothing,
                        other @ (Want::Frames(Expect::Attached) | Want::Answer | Want::Nothing) => {
                            other
                        }
                    };
                }
                Ok(None) => break,
                Err(why) => {
                    link.send(&FromConn::Bad(why));
                    (want, shut) = (Want::Nothing, true);
                    break;
                }
            }
        }
        // Its client sends no more: said once what it sent is taken, or
        // can't be, a frame left whole for its stage or a part of one.
        let taken = decoder.buffered() == 0 || !decoder.ready().unwrap_or(true);
        if ended
            && !told_shut
            && (taken && !matches!(want, Want::Nothing | Want::Answer) || want == Want::Answer)
        {
            link.send(&FromConn::Shut);
            told_shut = true;
        }
        // What waits for its client, as far as it takes it.
        while !out.is_empty() {
            match fuxix::io::write(&client, out.as_slice()) {
                Ok(0) | Err(fuxix::Errno::AGAIN) => break,
                Ok(n) => out.take(n),
                Err(fuxix::Errno::INTR) => {}
                // Gone: its server sees its link close.
                Err(_) => return Ok(()),
            }
        }
        if out.is_empty() {
            out.shrink(64 << 10);
            if std::mem::take(&mut unflushed) {
                link.send(&FromConn::Flushed);
            }
            if want == Want::Answer && answered {
                want = Want::Frames(Expect::Asked);
                continue;
            }
            if closing || alone {
                return Ok(());
            }
        }
        if !alone && link.flush().is_err() {
            alone = true;
        }
        if alone && out.is_empty() {
            return Ok(());
        }
        let reads = !shut && !ended && !matches!(want, Want::Answer | Want::Nothing);
        let mut fds = poll.list();
        let mut theirs = Events::NONE;
        if reads {
            theirs |= Events::IN;
        }
        if !out.is_empty() {
            theirs |= Events::OUT;
        }
        fds.push(PollFd::new(&client, theirs));
        let ours = match (alone, link.out.is_empty()) {
            (true, _) => Events::NONE,
            (false, true) => Events::IN,
            (false, false) => Events::IN | Events::OUT,
        };
        fds.push(PollFd::new(&link.stream, ours));
        match fds.poll(None) {
            Ok(_) | Err(fuxix::Errno::INTR) => {}
            Err(error) => return Err(format!("poll: {error}")),
        }
        let ready: Vec<Events> = fds.revents().collect();
        if reads && ready.first().is_some_and(|f| !f.is_empty()) {
            let mut read = 0usize;
            while read < READ {
                match fuxix::io::read(&client, &mut buffer) {
                    Ok(n) if n > 0 => {
                        read = read.saturating_add(n);
                        decoder.push(buffer.get(..n).unwrap_or_default());
                    }
                    Err(fuxix::Errno::INTR) => {}
                    Err(fuxix::Errno::AGAIN) => break,
                    Ok(_) | Err(_) => {
                        ended = true;
                        break;
                    }
                }
            }
        }
        if !alone && ready.get(1).is_some_and(|f| !f.is_empty()) {
            // Its server gone, so is it, once its client has what the
            // server sent it: a crashed server's clients have nothing left
            // to be served.
            if link.read(&mut buffer) == Read::Closed {
                alone = true;
            }
            loop {
                let message = match link.next_message::<ToConn>() {
                    Ok(Some(message)) => message,
                    Ok(None) => break,
                    Err(error) => return Err(format!("its server sent a bad frame: {error}")),
                };
                match message {
                    ToConn::Bytes(bytes) => {
                        out.push(&bytes);
                        unflushed = true;
                    }
                    ToConn::Expect(expect) => want = Want::Frames(expect),
                    ToConn::Answered => answered = true,
                    ToConn::Paint(paint) => {
                        screen.paint(*paint, &mut out);
                        unflushed = true;
                    }
                    ToConn::Detached => screen.restore(&mut out),
                    ToConn::Close => (closing, want) = (true, Want::Nothing),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Screen;
    use fux_vt::bytes::ByteQueue;
    use fux_wire::protocol::{Decoder, ToAttached};

    /// What a client was sent: its paints' bytes.
    fn painted(out: &ByteQueue) -> Vec<u8> {
        let mut decoder = Decoder::default();
        decoder.push(out.as_slice());
        let mut painted = Vec::new();
        while let Ok(Some(ToAttached::Paint(bytes))) = decoder.frame() {
            painted.extend_from_slice(&bytes);
        }
        painted
    }

    /// A client let go gives its terminal back only the cursor colour a
    /// pane set (OSC 112), and the cursor's shape as the terminal's own
    /// (DECSCUSR 0, then xterm's 7) only if a pane set one: by ctlseqs, 0 is
    /// a blinking block, which a terminal fux never told otherwise was not
    /// asked to be.
    #[test]
    fn detaching_gives_back_only_the_cursor_a_pane_set() {
        for (shape, colour, given) in [
            (fux_vt::CursorShape::Initial, None, ""),
            (
                fux_vt::CursorShape::Initial,
                Some([0, 255, 0].into()),
                "\x1b]112\x1b\\",
            ),
            (fux_vt::CursorShape::SteadyBar, None, "\x1b[0 q\x1b[7 q"),
        ] {
            let mut screen = Screen::default();
            screen.shown.modes.cursor_shape = shape;
            screen.shown.modes.cursor_colour = colour;
            let mut out = ByteQueue::default();
            screen.restore(&mut out);
            assert_eq!(String::from_utf8_lossy(&painted(&out)), given, "{shape:?}");
        }
    }
}
