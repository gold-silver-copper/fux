//! Private files and folders on Windows: owner-only descriptors, as the
//! pipes have (`socket`), and owners checked.
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
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
    let security = crate::socket::Security::private_folder()?;
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
/// or a junction), owned as this process's own objects are, by its token's
/// default owner, which another user can't make a folder's, and closed to
/// everyone else, as Unix's mode 0700 is (`socket::only_ours`). Told from
/// the folder opened, so what was checked is what is there.
pub fn is_private_dir(path: &Path) -> std::io::Result<bool> {
    use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
    use windows_sys::Win32::Security::Authorization::SE_FILE_OBJECT;
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, CreateFileW, FILE_ATTRIBUTE_DIRECTORY,
        FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
        FILE_READ_ATTRIBUTES, FILE_SHARE_READ, FILE_SHARE_WRITE, GetFileInformationByHandle,
        OPEN_EXISTING, READ_CONTROL,
    };
    let name = wide(path)?;
    // SAFETY: `name` is NUL-ended; opened to read its attributes and
    // descriptor only, a reparse point itself and not what it names.
    let raw = unsafe {
        CreateFileW(
            name.as_ptr(),
            READ_CONTROL | FILE_READ_ATTRIBUTES,
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
    let mut facts = BY_HANDLE_FILE_INFORMATION::default();
    // SAFETY: the handle is open; `facts` is written once.
    if unsafe { GetFileInformationByHandle(directory.as_raw_handle(), &mut facts) } == 0 {
        return Err(std::io::Error::last_os_error());
    }
    let attributes = facts.dwFileAttributes;
    if attributes & FILE_ATTRIBUTE_DIRECTORY == 0 || attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
    {
        return Ok(false);
    }
    let owned = crate::socket::owned_as_ours(&directory, SE_FILE_OBJECT)?;
    Ok(owned && crate::socket::only_ours(&directory)?)
}

/// Where a user's config lives: `XDG_CONFIG_HOME`, else `%APPDATA%`. Not
/// `HOME`, which Git Bash sets to a path of its own.
pub fn config_dir() -> Option<std::path::PathBuf> {
    let set = |name: &str| std::env::var_os(name).filter(|d| !d.is_empty());
    set("XDG_CONFIG_HOME")
        .or_else(|| set("APPDATA"))
        .map(Into::into)
}

/// A new file for this process alone, open to read and write, named after
/// `name`: it goes with its last handle (`FILE_FLAG_DELETE_ON_CLOSE`).
pub fn anonymous(name: &str) -> std::io::Result<std::fs::File> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ATTRIBUTE_TEMPORARY, FILE_FLAG_DELETE_ON_CLOSE, FILE_SHARE_DELETE, FILE_SHARE_READ,
        FILE_SHARE_WRITE,
    };
    let path = scratch()?.join(format!("{name}-{}", std::process::id()));
    (std::fs::OpenOptions::new().read(true).write(true))
        .create_new(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .attributes(FILE_ATTRIBUTE_TEMPORARY)
        .custom_flags(FILE_FLAG_DELETE_ON_CLOSE)
        .open(&path)
}

/// A directory for scratch files: the user's `%TEMP%`.
pub fn scratch() -> std::io::Result<std::path::PathBuf> {
    let dir = std::env::temp_dir();
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}
