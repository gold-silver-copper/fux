//! A region's stores hold no more than its budget, the peaks of their
//! growth included, and give all of it back when they drop: counted by
//! fux-vt's counting allocator, on the thread that does the work.
use fux_vt_peak::ThreadPeak;
use fuxth_core::{BoundedString, BoundedVec, Refused, Region};

#[global_allocator]
static HEAP: ThreadPeak = ThreadPeak;

// Written as an owner's module is: no std store can be named here.
fuxth_core::bounded_only!();

const BUDGET: usize = 1 << 20;

/// What the heap holds above the budget at most: the root region's own
/// counter. Every store's buffer is reserved from the region before it is
/// allocated, and a child region's counter too.
const OVERHEAD: usize = Region::OVERHEAD;

/// Runs `fill` in a fresh region of `BUDGET` bytes, and gives the most the
/// heap held at once above where it started; the region is whole again
/// after `fill`, and the heap back where it started once it drops.
fn peak_of(fill: impl FnOnce(&Region) -> Result<(), Refused>) -> Result<usize, Refused> {
    let base = HEAP.live();
    HEAP.reset_peak();
    let region = Region::new(BUDGET);
    fill(&region)?;
    assert_eq!(region.remaining(), BUDGET, "its stores gave all of it back");
    drop(region);
    assert_eq!(HEAP.live(), base, "nothing outlives the region");
    Ok(HEAP.peak().saturating_sub(base))
}

fn full(refused: Option<Refused>) -> bool {
    matches!(refused, Some(Refused::Full { .. }))
}

#[test]
fn each_store_holds_no_more_than_its_region_and_gives_it_all_back() -> Result<(), Refused> {
    let vec = peak_of(|region| {
        let mut words = BoundedVec::<u64>::new(region);
        assert!(full((0..).find_map(|i| words.push(i).err())));
        Ok(())
    })?;
    let string = peak_of(|region| {
        let mut text = BoundedString::new(region);
        assert!(full((0..).find_map(|_| text.push_str("fuxth ").err())));
        assert!(text.held() <= BUDGET);
        Ok(())
    })?;
    for peak in [vec, string] {
        assert!(peak <= BUDGET + OVERHEAD, "{peak} bytes at the peak");
        assert!(peak >= BUDGET / 4, "the budget is used: {peak} bytes");
    }
    Ok(())
}

#[test]
fn a_child_region_bounds_its_stores_within_its_parent() -> Result<(), Refused> {
    let peak = peak_of(|server| {
        let request = server.child(BUDGET / 2)?;
        let mut reply = BoundedVec::<u8>::new(&request);
        let mut words = BoundedVec::<u64>::new(server);
        let mut log = BoundedString::new(server);
        let mut refused = [false; 3];
        for i in 0u64.. {
            refused[0] |= full(reply.extend_from_slice(&[7; 1000]).err());
            refused[1] |= full(words.push(i).err());
            refused[2] |= full(log.push('ü').err());
            if refused == [true; 3] {
                break;
            }
        }
        assert!(reply.held() <= BUDGET / 2);
        assert!(server.child(BUDGET / 2).is_err(), "the server is spent");
        Ok(())
    })?;
    assert!(peak <= BUDGET + OVERHEAD, "{peak} bytes at the peak");
    Ok(())
}
