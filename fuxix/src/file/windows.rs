//! Private files and folders on Windows: owner-only descriptors, as the
//! pipes have (`socket`), and owners checked.
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{FromRawHandle, OwnedHandle};
use std::path::Path;

fn wide(path: &Path) -> std::io::Result<Vec<u16>> {
    let units: Vec<u16> = path.as_os_str().encode_wide().collect();
    if units.contains(&0) {
        let why = "a NUL in a path";
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, why));
    }
    Ok(units.into_iter().chain([0]).collect())
}

/// [`super::private_dir`] on Windows.
pub(super) fn make_private_dir(path: &Path) -> std::io::Result<()> {
    use windows_sys::Win32::Storage::FileSystem::CreateDirectoryW;
    let security = crate::socket::Security::owner_only_as("OICI")?;
    let attributes = security.attributes();
    let name = wide(path)?;
    // SAFETY: `name` is NUL-ended; `attributes` and the descriptor it
    // points at live through the call.
    match unsafe { CreateDirectoryW(name.as_ptr(), &attributes) } {
        0 => Err(std::io::Error::last_os_error()),
        _ => Ok(()),
    }
}

/// Whether the directory `path` is a real one (no reparse point: a link
/// or a junction) owned as this process's own objects are: by its token's
/// default owner, which another user can't make a folder's.
pub fn private_dir_owned(path: &Path) -> std::io::Result<bool> {
    use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
    use windows_sys::Win32::Security::Authorization::SE_FILE_OBJECT;
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT,
        FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ,
        FILE_SHARE_WRITE, GetFileAttributesW, INVALID_FILE_ATTRIBUTES, OPEN_EXISTING, READ_CONTROL,
    };
    let name = wide(path)?;
    // SAFETY: `name` is NUL-ended; it only reads.
    let attributes = unsafe { GetFileAttributesW(name.as_ptr()) };
    if attributes == INVALID_FILE_ATTRIBUTES {
        return Err(std::io::Error::last_os_error());
    }
    let real = attributes & FILE_ATTRIBUTE_DIRECTORY != 0;
    if !real || attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Ok(false);
    }
    // SAFETY: `name` is NUL-ended; opened to read its descriptor only,
    // the reparse point itself if one was put there since.
    let raw = unsafe {
        CreateFileW(
            name.as_ptr(),
            READ_CONTROL,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
            std::ptr::null_mut(),
        )
    };
    if raw == INVALID_HANDLE_VALUE {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: `raw` was just opened, and nothing else owns it.
    let directory = unsafe { OwnedHandle::from_raw_handle(raw) };
    crate::socket::owned_as_ours(&directory, SE_FILE_OBJECT).map_err(std::io::Error::from)
}
