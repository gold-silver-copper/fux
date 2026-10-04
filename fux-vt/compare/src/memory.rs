//! The memory this process holds, as the system and its C allocator count
//! it: for `footprint`, which reads it before and after making an engine
//! in a child process of its own.
//!
//! - macOS: `task_info(TASK_VM_INFO)`, its `phys_footprint` (what Activity
//!   Monitor calls Memory: dirty memory of the process's own, resident or
//!   compressed) and `resident_size`; and `malloc_zone_statistics` over
//!   every zone, the bytes malloc has handed out and not taken back. Both
//!   are libSystem's, declared here: no crate is added for them.
//! - Linux: `/proc/self/status`, `RssAnon` as the footprint and `VmRSS`;
//!   the allocator is not read.
//!
//! Rust's allocator here is the system's, so the allocator's count covers
//! the Rust engines, libvterm (C), and whatever Zig's code takes from
//! malloc; memory mapped directly (Ghostty's pages) shows in the
//! footprint alone.

/// One reading, in bytes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Reading {
    pub footprint: u64,
    pub resident: u64,
    /// Bytes allocated by malloc and in use, if it can be read here.
    pub malloc: Option<u64>,
}

#[cfg(target_os = "macos")]
pub fn read() -> Result<Reading, String> {
    let (footprint, resident) = mach::vm_info()?;
    Ok(Reading {
        footprint,
        resident,
        malloc: Some(mach::malloc_in_use()),
    })
}

#[cfg(target_os = "linux")]
pub fn read() -> Result<Reading, String> {
    let status = std::fs::read_to_string("/proc/self/status")
        .map_err(|e| format!("/proc/self/status: {e}"))?;
    let field = |name: &str| -> Result<u64, String> {
        status
            .lines()
            .find_map(|line| line.strip_prefix(name))
            .and_then(|rest| {
                rest.trim()
                    .trim_end_matches("kB")
                    .trim()
                    .parse::<u64>()
                    .ok()
            })
            .map(|kib| kib.saturating_mul(1024))
            .ok_or(format!("/proc/self/status: no {name}"))
    };
    Ok(Reading {
        footprint: field("RssAnon:")?,
        resident: field("VmRSS:")?,
        malloc: None,
    })
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub fn read() -> Result<Reading, String> {
    Err("memory is read on macOS and Linux only".into())
}

#[cfg(target_os = "macos")]
#[expect(
    unsafe_code,
    reason = "the process's memory is read through libSystem's C interface, and only here"
)]
mod mach {
    use std::ffi::c_void;

    /// `TASK_VM_INFO` (mach/task_info.h).
    const TASK_VM_INFO: i32 = 22;
    /// Room for `task_vm_info` of any revision so far (rev 7 is under 100
    /// words); the kernel fills what it has and says how much.
    const WORDS: usize = 128;
    /// `task_vm_info` is packed to 4 bytes (`#pragma pack(4)`): its fields
    /// by the 32-bit word they start at. `resident_size` follows
    /// `virtual_size` and two `integer_t`s; `phys_footprint`, the first
    /// field of rev 1, follows nineteen 8-byte fields and those two.
    const RESIDENT_SIZE: usize = 4;
    const PHYS_FOOTPRINT: usize = 36;

    /// `malloc_statistics_t` (malloc/malloc.h).
    #[repr(C)]
    #[derive(Default)]
    struct MallocStatistics {
        blocks_in_use: u32,
        size_in_use: usize,
        max_size_in_use: usize,
        size_allocated: usize,
    }

    unsafe extern "C" {
        /// What `mach_task_self()` reads: this task's port.
        static mach_task_self_: u32;
        fn task_info(task: u32, flavor: i32, info: *mut i32, count: *mut u32) -> i32;
        fn malloc_zone_statistics(zone: *mut c_void, stats: *mut MallocStatistics);
    }

    /// This task's physical footprint and resident size.
    pub fn vm_info() -> Result<(u64, u64), String> {
        let mut words = [0u32; WORDS];
        let mut count = u32::try_from(WORDS).map_err(|e| e.to_string())?;
        // SAFETY: `mach_task_self_` is set by libSystem before main and
        // never changes; `words` has room for `count` 32-bit words, which
        // is all the kernel writes, and `count` is a valid u32 it updates.
        let status = unsafe {
            task_info(
                mach_task_self_,
                TASK_VM_INFO,
                words.as_mut_ptr().cast::<i32>(),
                &raw mut count,
            )
        };
        if status != 0 {
            return Err(format!("task_info(TASK_VM_INFO) failed: {status}"));
        }
        let filled = usize::try_from(count).map_err(|e| e.to_string())?;
        if filled < PHYS_FOOTPRINT.saturating_add(2) {
            return Err(format!(
                "task_info(TASK_VM_INFO) gave {filled} words, without phys_footprint"
            ));
        }
        // An 8-byte field from two words, little end first, as the kernel
        // lays it out on every Mac.
        let at = |word: usize| -> u64 {
            let low = words.get(word).copied().unwrap_or(0);
            let high = words.get(word.saturating_add(1)).copied().unwrap_or(0);
            u64::from(low) | (u64::from(high) << 32)
        };
        Ok((at(PHYS_FOOTPRINT), at(RESIDENT_SIZE)))
    }

    /// The bytes malloc has handed out and not taken back, every zone.
    pub fn malloc_in_use() -> u64 {
        let mut stats = MallocStatistics::default();
        // SAFETY: a null zone asks for every zone summed; `stats` is a
        // valid `malloc_statistics_t` it fills.
        unsafe { malloc_zone_statistics(std::ptr::null_mut(), &raw mut stats) };
        u64::try_from(stats.size_in_use).unwrap_or(u64::MAX)
    }

    #[cfg(test)]
    mod tests {
        /// The footprint grows by about what is written into new memory,
        /// and malloc counts what it hands out: the offsets are right. Large
        /// enough that what other tests allocate meanwhile is lost in it.
        #[test]
        fn a_new_buffer_shows_in_both_counts() -> Result<(), String> {
            let before = super::vm_info()?;
            let malloc_before = super::malloc_in_use();
            let size = 256usize << 20;
            let buffer = vec![1u8; size];
            let after = super::vm_info()?;
            let malloc_after = super::malloc_in_use();
            let grown = |a: u64, b: u64| a.saturating_sub(b);
            let size = u64::try_from(size).map_err(|e| e.to_string())?;
            assert!(
                grown(after.0, before.0) >= size / 10 * 9,
                "{before:?} {after:?}"
            );
            assert!(
                grown(after.0, before.0) <= size / 10 * 11,
                "{before:?} {after:?}"
            );
            assert!(
                grown(after.1, before.1) >= size / 10 * 9,
                "{before:?} {after:?}"
            );
            assert!(grown(malloc_after, malloc_before) >= size / 10 * 9);
            assert!(buffer.iter().all(|b| *b == 1));
            Ok(())
        }
    }
}
