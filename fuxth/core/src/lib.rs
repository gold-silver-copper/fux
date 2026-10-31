//! The part of fuxth that data shares without the interpreter: a failure's
//! code, how long and deep a value may be, and the budgets that bound
//! memory: grants that count it, and regions whose stores reserve it before
//! they allocate. `fuxth` gives all of it again; a host's wire and server
//! use this crate, and never the machine.
#![forbid(clippy::print_stderr)]
mod bounded;
mod code;
mod grant;
mod region;

pub use bounded::{Bounded, BoundedMap, BoundedString, BoundedVec};
pub use code::{Code, Failure};
pub use grant::{Grant, Render};
pub use region::{Refused, Region, Reservation};

/// The most elements one string or list a program builds may hold.
pub const LONGEST: usize = 1_000_000;

/// How deep lists may nest: a list of lists of … at most this many. Every
/// walk over a value (comparing, printing, dropping it) goes no deeper.
pub const NESTING: u8 = 64;

/// What `bounded_only!` makes std's growable stores in its module: a type
/// nothing can be made of, so a store there can only be one of a region's
/// ([`BoundedVec`], [`BoundedString`], [`BoundedMap`]).
pub struct Unbounded<T: ?Sized>(std::marker::PhantomData<T>);

/// Written at the top of an owner's module, refuses std's growable stores
/// there by type: `Vec`, `String` and std's collections name [`Unbounded`],
/// which nothing can be made of. Making one there (`Vec::new()`) fails to
/// compile, a field or a signature that names one can't be given a std
/// store, and a `use` of a collection names it twice. What it doesn't see
/// is a store whose type is never written (`format!`, `collect`,
/// `to_string`); a written `std::vec::Vec` is a visible exception, for
/// review.
#[macro_export]
macro_rules! bounded_only {
    () => {
        type Vec<T> = $crate::Unbounded<T>;
        type String = $crate::Unbounded<str>;
        type VecDeque<T> = $crate::Unbounded<T>;
        type BinaryHeap<T> = $crate::Unbounded<T>;
        type HashMap<K, V> = $crate::Unbounded<(K, V)>;
        type HashSet<T> = $crate::Unbounded<T>;
        type BTreeMap<K, V> = $crate::Unbounded<(K, V)>;
        type BTreeSet<T> = $crate::Unbounded<T>;
    };
}
