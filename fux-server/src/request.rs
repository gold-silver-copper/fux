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

use fux_wire::wire::{Budget, MAX_ALLOCATED};
use fuxth_core::{BoundedVec, Refused, Region};

/// What one request may hold at once: its decoded frame, what its answer
/// builds, and the answer's bytes. The largest answer, the default-history
/// styled capture, holds 47,302,808 bytes at its peak: its capture built
/// (25,425,880) and its 21,876,928 bytes on the wire.
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

/// `reply`'s bytes on the wire, made in `budget`'s region: refused before
/// they pass the reply's cap or the region.
pub fn encoded(
    reply: &fux_wire::protocol::Reply,
    budget: &Budget,
) -> Result<BoundedVec<u8>, fux_wire::wire::Error> {
    fux_wire::wire::encode_in(reply, budget.region())
}

/// Why a request was refused its region: another request's is open. A
/// failure's message, made once (the one `std` store here).
pub fn refused(refused: Refused) -> std::string::String {
    format!("the server holds another request's region: {refused}")
}
