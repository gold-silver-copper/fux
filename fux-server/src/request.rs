//! A request's region: everything the server makes for one request, from
//! decoding its frame to its reply's bytes, is charged there before it is
//! made, and given back when the request is answered. The server answers
//! one request at a time, on its one thread, so its budget for requests
//! holds one request's region: a request kept past its answer shows as the
//! next one refused, out loud, never as memory.
//!
//! What a request leaves in the session (a pane's name, a launch, a clip)
//! is the session's from then on, bounded by its own limits.
fuxth_core::bounded_only!();

use fux_wire::protocol::{Asked, Frame, Reply};
use fux_wire::wire::{Budget, MAX_ALLOCATED};
use fuxth_core::{BoundedVec, Refused, Region};

/// What one request may hold at once: its decoded frame, what its answer
/// builds, and the answer's bytes, made in exactly their room. A styled
/// capture of a default history, 12,435 lines of 80 cells each in a style
/// of its own, holds 26,570,520 bytes at its peak: 13,540,703 built and
/// 13,035,337 on the wire. A reply is at most `MAX_REPLY` (32 MiB) on the
/// wire, and what it is built from about as large, so this admits any
/// reply the wire does, with room to spare.
pub const REQUEST_BYTES: usize = MAX_ALLOCATED;

/// The server's budget for requests: room for one request's region.
pub struct Requests {
    budget: Region,
    each: usize,
}

impl Requests {
    /// Room for one request's region of `each` bytes.
    pub fn new(each: usize) -> Self {
        Self {
            budget: Region::new(each.saturating_add(Region::OVERHEAD)),
            each,
        }
    }

    /// A request's budget, in a region carved whole from the server's
    /// before anything is made for it; refused while another is open.
    pub fn open(&self) -> Result<Budget, Refused> {
        self.budget
            .child(self.each)
            .map(|region| Budget::of(&region))
    }

    /// The bytes each request's region holds.
    pub fn each(&self) -> usize {
        self.each
    }
}

impl Default for Requests {
    fn default() -> Self {
        Self::new(REQUEST_BYTES)
    }
}

/// A request's frame, `kind` and `payload`, decoded in a region carved for
/// it from `requests`: the request, and its budget, which holds what it
/// made. One its region can't hold is refused, `too_long`, its reply to be
/// made once that region is given back; a frame that is no request is an
/// error.
pub fn decoded(
    requests: &Requests,
    kind: u8,
    payload: &[u8],
) -> Result<Result<(Asked, Budget), Reply>, fux_wire::protocol::Error> {
    let too_long = fux_wire::code::Code::from(fux_wire::code::Raise::TooLong);
    let mut budget = match requests.open() {
        Ok(budget) => budget,
        Err(why) => return Ok(Err(Reply::failed(too_long, refused(why)))),
    };
    match Asked::decode(kind, payload, &mut budget) {
        Ok(asked) => Ok(Ok((asked, budget))),
        Err(fux_wire::protocol::Error::Wire(fux_wire::wire::Error::Limit)) => {
            let each = requests.each();
            let why = format!("the request exceeds the {each} bytes a request may hold");
            Ok(Err(Reply::failed(too_long, why)))
        }
        Err(error) => Err(error),
    }
}

/// `reply`'s bytes on the wire, made in `budget`'s region: refused before
/// they pass the reply's cap or the region.
pub fn encoded(reply: &Reply, budget: &Budget) -> Result<BoundedVec<u8>, fux_wire::wire::Error> {
    fux_wire::wire::encode_in(reply, budget.region())
}

/// Why a request was refused its region: another request's is open. A
/// failure's message, made once (the one `std` store here).
pub fn refused(refused: Refused) -> std::string::String {
    format!("the server holds another request's region: {refused}")
}

#[cfg(test)]
mod tests {
    use super::{Requests, decoded, encoded};
    use crate::session::{Begun, Caller, Origin, testing::started};
    use fux_wire::code::{Code, Raise};
    use fux_wire::command::{AnyRef, CaptureView, Command, Input, Request, To};
    use fux_wire::id::PaneId;
    use fux_wire::protocol::{Asked, Context, Frame, Reply, Response};
    use std::time::Instant;
    type TestResult = Result<(), std::boxed::Box<dyn std::error::Error>>;

    /// What a request holds past its region: the server's budget and its
    /// region's counters, and what is made beside the reply's values (its
    /// box, a failure's message).
    const FIXED: usize = 4096;

    /// A request's `frame`, as the server answers it: decoded in its
    /// region, begun, and its reply's bytes made there; their length, or
    /// the failure it was answered.
    fn answer(
        session: &mut crate::session::Session,
        frame: &[u8],
    ) -> Result<Result<usize, Reply>, std::boxed::Box<dyn std::error::Error>> {
        let (kind, payload) = frame
            .get(4..)
            .and_then(<[u8]>::split_first)
            .ok_or("frame")?;
        let (asked, mut budget) = match decoded(&session.requests, *kind, payload)? {
            Ok(decoded) => decoded,
            Err(refused) => return Ok(Err(refused)),
        };
        let origin = Origin::Cli(Caller::default());
        let Begun::Done(reply) = session.begin(asked.request, &origin, &mut budget, Instant::now())
        else {
            return Err("a request that waits".into());
        };
        if let Response::Failed { .. } = reply.response {
            return Ok(Err(reply));
        }
        let left = budget.region().remaining();
        match encoded(&reply, &budget) {
            Ok(bytes) => Ok(Ok(bytes.len())),
            // The server ends the connection, saying why.
            Err(_) => Ok(Err(Reply::failed(
                fux_wire::code::Code::from(fux_wire::code::Raise::TooLong),
                format!("its bytes past the {left} its region had left"),
            ))),
        }
    }

    /// The default-history styled capture's shape, a line's every cell in
    /// a style of its own, and one large request decoded, each at a small
    /// region: its peak stays under the region and a fixed overhead, and
    /// once answered, or refused, the heap is where it started.
    #[test]
    fn a_request_holds_no_more_than_its_region_and_a_fixed_overhead() -> TestResult {
        let allocator = fux_vt_peak::ThreadPeak;
        let mut session = started()?;
        let pane = PaneId::of(1);
        for line in 0..300 {
            let cells = (0..80).map(|cell| format!("\x1b[38;5;{}mx", (line + cell) % 256));
            let text = format!("{}\x1b[m\r\n", cells.collect::<std::string::String>());
            session.output(pane, text.as_bytes(), Instant::now());
        }
        let capture = Command::CapturePane {
            target: pane,
            history: Some(usize::MAX),
            view: CaptureView {
                styles: true,
                ..CaptureView::default()
            },
        };
        let keys = Command::SendKeys {
            to: To::Now(AnyRef::Pane(PaneId::of(99))),
            inputs: std::iter::repeat_n(Input::Text("".into()), 190_000).collect(),
        };
        // A workload, the region it runs in, and whether it fits there.
        for (command, region, fits) in [
            (&capture, 1 << 20, true),
            (&capture, 512 << 10, false),
            (&keys, 8 << 20, true),
            (&keys, 1 << 20, false),
        ] {
            let asked = Asked {
                after: None,
                request: Request::Do(command.clone()),
                context: Context::default(),
            };
            // Its frame, as the connection holds it, read.
            let frame = asked.encode()?;
            session.requests = Requests::new(region);
            // The first grows the buffers every settle reuses.
            drop(answer(&mut session, &frame)?);
            let base = allocator.live();
            allocator.reset_peak();
            let answered = answer(&mut session, &frame)?;
            let peak = allocator.peak().saturating_sub(base);
            let refused = match answered.as_ref().map_err(|reply| &reply.response) {
                Err(Response::Failed { code, .. }) => Some(*code),
                _ => None,
            };
            drop(answered);
            let after = allocator.live();
            let what = format!("{command:.40?} in {region}: {refused:?}, {peak} at the peak");
            let expected = match (command, fits) {
                (Command::CapturePane { .. }, true) => None,
                // The keys decode, then find no pane.
                (Command::SendKeys { .. }, true) => Some(Code::NoPane),
                (_, false) => Some(Code::from(Raise::TooLong)),
                _ => return Err(what.into()),
            };
            assert_eq!(refused, expected, "{what}");
            assert!(peak <= region + FIXED, "{what}");
            // The region is used, not met by refusing everything.
            assert!(!fits || peak >= region / 4, "{what}");
            assert_eq!(after, base, "{what}: the heap after");
            println!("{what}");
        }
        Ok(())
    }
}
