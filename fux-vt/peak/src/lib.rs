//! fux-vt's bounds tests' counting allocator (`THREATS.md` 3.4): the
//! system's, counting the bytes live and their peak for each thread apart.
//! A test binary makes it its `#[global_allocator]`; a probe resets the
//! peak, does its work, and reads both on the thread that did it, so what
//! the test harness's other threads allocate meanwhile (a "running for over
//! 60 seconds" message, a finished test's report) is never counted in it.
//! It is a crate of its own because fux-vt forbids `unsafe`, its tests
//! included.
//!
//! A `realloc` counts its new block beside the old until the old is freed,
//! so a growth's transient shows in the peak. A block freed on another
//! thread than the one that made it counts as freed there (at most down to
//! nothing), not where it was made: fux-vt makes no threads, so a probe's
//! blocks are made and freed on its own.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

thread_local! {
    /// The bytes this thread has made live and not freed.
    static LIVE: Cell<usize> = const { Cell::new(0) };
    /// The most `LIVE` has been since the last reset.
    static PEAK: Cell<usize> = const { Cell::new(0) };
}

/// The system's allocator, counting what each thread holds.
#[derive(Clone, Copy, Debug, Default)]
pub struct ThreadPeak;

impl ThreadPeak {
    /// The bytes this thread holds now.
    pub fn live(&self) -> usize {
        LIVE.try_with(Cell::get).unwrap_or(0)
    }
    /// The most bytes this thread has held at once since its last reset.
    pub fn peak(&self) -> usize {
        PEAK.try_with(Cell::get).unwrap_or(0)
    }
    /// Starts this thread's peak again from what it holds now.
    pub fn reset_peak(&self) {
        let _ = PEAK.try_with(|peak| peak.set(self.live()));
    }
}

/// Counts `bytes` more live on this thread, and its peak.
fn made(bytes: usize) {
    let _ = LIVE.try_with(|live| {
        let now = live.get().saturating_add(bytes);
        live.set(now);
        let _ = PEAK.try_with(|peak| peak.set(peak.get().max(now)));
    });
}

/// Counts `bytes` fewer live on this thread.
fn freed(bytes: usize) {
    let _ = LIVE.try_with(|live| live.set(live.get().saturating_sub(bytes)));
}

// SAFETY: every call is passed to `System` as it came, with the same
// layouts; the counting around it touches only this thread's own cells,
// which are constant-initialized and need no allocation.
unsafe impl GlobalAlloc for ThreadPeak {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: the caller's promises for `layout` are `System`'s.
        let block = unsafe { System.alloc(layout) };
        if !block.is_null() {
            made(layout.size());
        }
        block
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: as for `alloc`.
        let block = unsafe { System.alloc_zeroed(layout) };
        if !block.is_null() {
            made(layout.size());
        }
        block
    }

    unsafe fn dealloc(&self, block: *mut u8, layout: Layout) {
        // SAFETY: `block` was made by this allocator, so by `System`, with
        // `layout`, as the caller promises.
        unsafe { System.dealloc(block, layout) };
        freed(layout.size());
    }

    unsafe fn realloc(&self, block: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        // The new block beside the old, until the old is let go.
        made(size);
        // SAFETY: as for `dealloc`, and `size` is the caller's promise.
        let moved = unsafe { System.realloc(block, layout, size) };
        if moved.is_null() {
            freed(size);
        } else {
            freed(layout.size());
        }
        moved
    }
}

#[cfg(test)]
mod tests {
    use super::ThreadPeak;

    #[global_allocator]
    static ALLOCATOR: ThreadPeak = ThreadPeak;

    /// What this thread makes counts, what another makes doesn't, a reset
    /// peak starts from what is live, and a growth counts both blocks.
    #[test]
    fn each_thread_counts_its_own() -> std::thread::Result<()> {
        // What spawning a thread makes on this one, and the test's own.
        const SLACK: usize = 16 << 10;
        let base = ALLOCATOR.live();
        ALLOCATOR.reset_peak();
        let block = std::hint::black_box(vec![1u8; 1 << 20]);
        assert!(ALLOCATOR.live().saturating_sub(base) >= 1 << 20);
        std::thread::spawn(|| std::hint::black_box(vec![1u8; 4 << 20]).len()).join()?;
        assert!(ALLOCATOR.peak().saturating_sub(base) < (1 << 20) + SLACK);
        drop(block);
        assert!(ALLOCATOR.live().saturating_sub(base) < SLACK);
        let base = ALLOCATOR.live();
        ALLOCATOR.reset_peak();
        let mut grown = std::hint::black_box(Vec::<u8>::with_capacity(1 << 20));
        grown.reserve_exact(2 << 20);
        assert!(ALLOCATOR.peak().saturating_sub(base) >= (1 << 20) + (2 << 20));
        Ok(())
    }
}
