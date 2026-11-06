//! Where another process works, read from its process parameters (spike
//! S5, D-137): `NtQueryInformationProcess` for its PEB, then
//! `ReadProcessMemory` of `ProcessParameters.CurrentDirectory`, the
//! offsets every reader uses (Process Explorer, psutil, WezTerm), as
//! `winternl.h` marks the field reserved.
//!
//! - **Read twice:** a directory read while the shell changes it can tear;
//!   two reads that differ are a failure, never either one.
//! - **The full stored length:** a long path's `Length` can exceed its
//!   recorded `MaximumLength`; it is what holds the path.
//! - **A 32-bit process** (WOW64) has its own 32-bit PEB and layout.
//! - **Refused, not guessed:** a process that can't be opened (an elevated
//!   one, to a normal reader) or read is an error, as Unix refuses an
//!   unknown directory.
use super::Pid;
use crate::errno::{Errno, Result};
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::PathBuf;
use windows_sys::Win32::Foundation::HANDLE;

/// `PROCESS_BASIC_INFORMATION`, as 64-bit Windows lays it out.
#[repr(C)]
#[derive(Default)]
struct BasicInfo {
    exit_status: isize,
    peb: usize,
    affinity: usize,
    base_priority: isize,
    pid: usize,
    parent: usize,
}

/// Reads `buffer.len()` bytes at `address` in `process`, whole.
fn read(process: HANDLE, address: usize, buffer: &mut [u8]) -> Result<()> {
    use windows_sys::Win32::System::Diagnostics::Debug::ReadProcessMemory;
    let mut read = 0usize;
    let from = std::ptr::without_provenance(address);
    // SAFETY: `process` is open with `PROCESS_VM_READ`; `buffer` is
    // writable for its length; the address is only a number in the other
    // process, which the system checks.
    let done = unsafe {
        ReadProcessMemory(
            process,
            from,
            buffer.as_mut_ptr().cast(),
            buffer.len(),
            &mut read,
        )
    };
    match (done, read == buffer.len()) {
        (0, _) => Err(Errno::last()),
        (_, false) => Err(Errno::IO),
        (_, true) => Ok(()),
    }
}

/// `N` bytes at `address` in `process`.
fn bytes<const N: usize>(process: HANDLE, address: usize) -> Result<[u8; N]> {
    let mut out = [0u8; N];
    read(process, address, &mut out)?;
    Ok(out)
}

/// Asks the system about `process`, `class`, into `info`.
fn query<T>(process: HANDLE, class: i32, info: &mut T) -> Result<()> {
    use windows_sys::Wdk::System::Threading::NtQueryInformationProcess;
    let size = u32::try_from(std::mem::size_of::<T>()).map_err(|_| Errno::INVAL)?;
    // SAFETY: `process` is open for querying; `info` is writable for
    // `size` bytes, as `class` writes.
    let status = unsafe {
        NtQueryInformationProcess(
            process,
            class,
            (&raw mut *info).cast(),
            size,
            std::ptr::null_mut(),
        )
    };
    match status {
        0 => Ok(()),
        _ => Err(Errno::ACCES),
    }
}

/// The directory string's length, and where its characters are.
fn directory_string(process: HANDLE) -> Result<(usize, usize)> {
    use windows_sys::Wdk::System::Threading::{ProcessBasicInformation, ProcessWow64Information};
    let mut peb32 = 0usize;
    query(process, ProcessWow64Information, &mut peb32)?;
    let (length, buffer) = if peb32 != 0 {
        // A 32-bit process: `ProcessParameters` at 0x10 of its PEB, then
        // `CurrentDirectory.DosPath` at 0x24 of those, its `Buffer` 4
        // bytes past its two lengths.
        let parameters = u32::from_le_bytes(bytes(process, peb32.wrapping_add(0x10))?);
        let parameters = usize::try_from(parameters).map_err(|_| Errno::INVAL)?;
        let string: [u8; 8] = bytes(process, parameters.wrapping_add(0x24))?;
        let [l0, l1, _, _, b0, b1, b2, b3] = string;
        let buffer = u32::from_le_bytes([b0, b1, b2, b3]);
        (u16::from_le_bytes([l0, l1]), u64::from(buffer))
    } else {
        // `ProcessParameters` at 0x20 of the PEB, `CurrentDirectory.DosPath`
        // at 0x38 of those, its `Buffer` 8 bytes in.
        let mut info = BasicInfo::default();
        query(process, ProcessBasicInformation, &mut info)?;
        if info.peb == 0 {
            return Err(Errno::SRCH);
        }
        let parameters = u64::from_le_bytes(bytes(process, info.peb.wrapping_add(0x20))?);
        let parameters = usize::try_from(parameters).map_err(|_| Errno::INVAL)?;
        if parameters == 0 {
            return Err(Errno::SRCH);
        }
        let string: [u8; 16] = bytes(process, parameters.wrapping_add(0x38))?;
        let [l0, l1, _, _, _, _, _, _, b0, b1, b2, b3, b4, b5, b6, b7] = string;
        let buffer = u64::from_le_bytes([b0, b1, b2, b3, b4, b5, b6, b7]);
        (u16::from_le_bytes([l0, l1]), buffer)
    };
    let buffer = usize::try_from(buffer).map_err(|_| Errno::INVAL)?;
    if length == 0 || length % 2 != 0 || buffer == 0 {
        return Err(Errno::INVAL);
    }
    Ok((usize::from(length), buffer))
}

/// One read of the directory's characters, `budget` admitting them.
fn once<A: crate::Allocation>(
    process: HANDLE,
    budget: &mut A,
) -> std::result::Result<Result<Vec<u8>>, A::Error> {
    let (length, buffer) = match directory_string(process) {
        Ok(found) => found,
        Err(errno) => return Ok(Err(errno)),
    };
    let mut characters = Vec::new();
    budget.reserve(&mut characters, length)?;
    characters.resize(length, 0);
    Ok(read(process, buffer, &mut characters).map(|()| characters))
}

/// The directory process `pid` works in, read twice from its process
/// parameters ([module docs](self)): `Err` if it can't be opened or read,
/// or the two reads differ; its characters admitted under `budget`.
pub fn cwd_observed<A: crate::Allocation>(
    pid: Pid,
    budget: &mut A,
) -> std::result::Result<Result<Option<PathBuf>>, A::Error> {
    use std::os::windows::ffi::OsStringExt;
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_VM_READ,
    };
    let access = PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_VM_READ;
    // SAFETY: it takes numbers and touches no memory.
    let raw = unsafe { OpenProcess(access, 0, pid.as_raw()) };
    if raw.is_null() {
        return Ok(Err(Errno::last()));
    }
    // SAFETY: `raw` was just opened, and nothing else owns it.
    let process = unsafe { OwnedHandle::from_raw_handle(raw) };
    let first = match once(process.as_raw_handle(), budget)? {
        Ok(first) => first,
        Err(errno) => return Ok(Err(errno)),
    };
    let second = match once(process.as_raw_handle(), budget)? {
        Ok(second) => second,
        Err(errno) => return Ok(Err(errno)),
    };
    if first != second {
        return Ok(Err(Errno::AGAIN));
    }
    let mut units: Vec<u16> = (first.as_chunks::<2>().0.iter())
        .map(|pair| u16::from_le_bytes(*pair))
        .collect();
    // Stored with a `\` after it, but a drive's root, which keeps it.
    let backslash = u16::from(b'\\');
    if units.len() > 3 && units.last() == Some(&backslash) {
        units.pop();
    }
    Ok(Ok(Some(PathBuf::from(std::ffi::OsString::from_wide(
        &units,
    )))))
}
