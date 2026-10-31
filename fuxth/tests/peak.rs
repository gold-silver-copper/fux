//! A run holds no more than its region, beyond the code its program was
//! compiled to and a fixed overhead: counted by fux-vt's counting
//! allocator, on the thread that runs it. Its region gives all of it back
//! as the run drops, and a step the region refuses changes nothing.
use fux_vt_peak::ThreadPeak;
use fuxth::{
    AppValue, Args, BoundedVec, Code, Compiler, Done, Error, Failure, Host, Limits, Machine,
    Outcome, Program, Region, STDLIB, Value, Vocabulary, token,
};

#[global_allocator]
static HEAP: ThreadPeak = ThreadPeak;

// Written as an owner's module is: no std store can be named here.
fuxth::bounded_only!();

/// The run's region: small, so the loops below fill and refill it.
const BUDGET: usize = 256 << 10;

/// What a run holds on the heap beside its region and its program's code:
/// the region's own counter, and a refusal's message as a `try` catches
/// it. Every value, the stack, the calls, the `let` names and the
/// program's box are the region's.
const OVERHEAD: usize = Region::OVERHEAD + 1024;

#[derive(Clone, Debug, PartialEq)]
struct Nothing;

impl AppValue for Nothing {
    fn kind(&self) -> &'static str {
        "nothing"
    }
}

/// One word, `ask` (-- n), which waits for the host.
struct Asker;

impl Host for Asker {
    type Value = Nothing;
    type Request = ();
    fn call(
        &mut self,
        _: usize,
        _: Args<'_, Nothing>,
        _: &mut BoundedVec<Value<Nothing>>,
    ) -> Result<Done<()>, Failure> {
        Ok(Done::Later(()))
    }
    fn print(&mut self, _: &str) -> Result<(), Failure> {
        Ok(())
    }
}

/// The standard library, compiled once in a region of its own, as a host
/// keeps it for every run.
fn library() -> Result<Compiler<Nothing>, Error> {
    let mut words = Vocabulary::default();
    words.add("ask", "-- integer")?;
    let mut compiler = Compiler::new(words, &Region::new(1 << 20));
    compiler.add(&token::source("stdlib.fux", STDLIB)?)?;
    compiler.prepare();
    Ok(compiler)
}

/// `source` compiled after `library`, to run in `region`.
fn compiled(
    library: &Compiler<Nothing>,
    source: &str,
    region: &Region,
) -> Result<Program<Nothing>, Error> {
    let mut compiler = library.clone();
    compiler.make_in(region);
    compiler.add(&token::source("peak", source)?)?;
    compiler.finish()
}

/// What the heap holds above `base` beside what `region` holds: the code
/// a program was compiled to.
fn code(base: usize, region: &Region) -> usize {
    let held = region.budget().saturating_sub(region.remaining());
    HEAP.live().saturating_sub(base).saturating_sub(held)
}

#[test]
fn a_run_building_and_dropping_values_holds_no_more_than_its_region() -> Result<(), Error> {
    let library = library()?;
    // Strings and lists, grown in place and joined, in calls, made and
    // dropped 2,000 times; and every 400th time, a string grown till the
    // region refuses it, which a `try` catches.
    let source = "define build let n str x n copies let s 0 n range n push len drop
        list s s end concat len drop end
        define fill str too_long try str x begin str y push false until catch drop drop end end
        0 begin dup 100 mod 50 add build dup 400 mod 0 eq if fill then
        list 1 2 3 end 4 push drop 1 add dup 2000 eq until drop";
    // The library's words this program reaches are checked once, for
    // every program after it: not this run's.
    compiled(&library, source, &Region::new(BUDGET))?;
    let base = HEAP.live();
    let region = Region::new(BUDGET);
    let program = compiled(&library, source, &region)?;
    let code = code(base, &region);
    HEAP.reset_peak();
    let mut machine = Machine::new(program, Limits::default())?;
    assert_eq!(machine.run(&mut Asker, u64::MAX)?, Outcome::Finished);
    let peak = HEAP.peak().saturating_sub(base);
    assert!(
        peak <= BUDGET + code + OVERHEAD,
        "{peak} bytes at the peak: {code} of code"
    );
    assert!(peak >= BUDGET / 4, "the region is used: {peak} bytes");
    assert!(machine.steps() > 100_000, "it did the work");
    drop(machine);
    assert_eq!(region.remaining(), BUDGET, "the run gave all of it back");
    drop(region);
    assert_eq!(HEAP.live(), base, "nothing outlives the run");
    Ok(())
}

#[test]
fn a_build_its_region_refuses_makes_nothing_and_leaves_the_region_whole() -> Result<(), Error> {
    let library = library()?;
    // A list of 2,000 strings, kept, then their text joined: more than the
    // region holds in all.
    let source =
        "str x 128 copies let s list 0 2000 range each drop s end end let l ask drop l concat";
    compiled(&library, source, &Region::new(BUDGET))?;
    let base = HEAP.live();
    let region = Region::new(BUDGET);
    let mut machine = Machine::new(compiled(&library, source, &region)?, Limits::default())?;
    assert_eq!(machine.run(&mut Asker, u64::MAX)?, Outcome::Suspended(()));
    machine.import(Value::Int(0))?;
    let left = region.remaining();
    let live = HEAP.live();
    HEAP.reset_peak();
    let refused = machine.run(&mut Asker, u64::MAX).err().map(|e| e.code);
    assert_eq!(refused, Some(Code::TooLong));
    assert_eq!(region.remaining(), left, "the region is as it was");
    assert!(HEAP.peak().saturating_sub(live) < 4096, "nothing was made: its failure alone");
    drop(machine);
    assert_eq!(region.remaining(), BUDGET);
    drop(region);
    assert_eq!(HEAP.live(), base);
    Ok(())
}
