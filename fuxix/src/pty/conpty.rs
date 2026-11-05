//! Which ConPTY a pseudo console is made with: Microsoft's separately
//! shipped one (NuGet `Microsoft.Windows.Console.ConPTY`, MIT), pinned and
//! hash-checked, or Windows' built-in one (D-130).
//!
//! [`fetch`] puts the pinned `conpty.dll` and `OpenConsole.exe` in a folder
//! of the caller's; [`load`] chooses, once per process, before the first
//! [`super::open`]: the pinned pair from that folder if both files hash as
//! pinned, else the built-in, and says why. `open` before `load` fails, so
//! the built-in is never chosen without the caller being told.
//!
//! **What is trusted:** only bytes whose SHA-256 (Windows' own, `BCryptHash`)
//! is the pin. The package is hashed before `tar.exe` reads it, both files
//! after; `load` hashes each file through a handle that lets nobody write,
//! rename or delete it, keeps both handles for the process's life, and
//! loads the DLL by its full path with the search limited to its own folder
//! and System32. `OpenConsole.exe`, which the DLL starts beside itself for
//! every pseudo console, is that same checked file.
use super::{Error, at};
use std::fs::File;
use std::io::Read;
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;
use windows_sys::Win32::Foundation::{HANDLE, HMODULE};
use windows_sys::Win32::System::Console::{COORD, HPCON};
use windows_sys::core::HRESULT;

/// The pinned version, which names its folder.
const VERSION: &str = "1.25.260930003";

/// Where it is fetched from: https only.
const URL: &str = "https://api.nuget.org/v3-flatcontainer/microsoft.windows.console.conpty/1.25.260930003/microsoft.windows.console.conpty.1.25.260930003.nupkg";

/// The package's file name: one a user puts in the folder by hand (behind
/// a firewall) is used instead of a download, if it hashes as pinned.
const PACKAGE: &str = "microsoft.windows.console.conpty.1.25.260930003.nupkg";

/// The package's SHA-256, as nuget.org served it to spike S1 and as it was
/// checked again for this pin.
const PACKAGE_SHA256: &str = "02b07b349af66d801159bdf9e440d4a1ce78bb951f37fc8609731665afdae7ee";

/// The two files of one architecture: where each is in the package, and
/// its SHA-256.
struct Arch {
    name: &'static str,
    dll: (&'static str, &'static str),
    console: (&'static str, &'static str),
}

#[cfg(target_arch = "x86_64")]
const ARCH: Option<Arch> = Some(Arch {
    name: "x64",
    dll: (
        "runtimes/win-x64/native/conpty.dll",
        "feeef341d891643c62d30b6b07800bc70f0bc148f44cb8c3bee84aa557ae805a",
    ),
    console: (
        "build/native/runtimes/x64/OpenConsole.exe",
        "3d66b23d0a71bb8eed2b77edc8b9df9bf54ce6c8fb74c863a30e760997f80586",
    ),
});

#[cfg(target_arch = "aarch64")]
const ARCH: Option<Arch> = Some(Arch {
    name: "arm64",
    dll: (
        "runtimes/win-arm64/native/conpty.dll",
        "151a02355227710c20c9f8f1eaf68ac4668aa09cdc921adc7ec661804d8eb1bf",
    ),
    console: (
        "build/native/runtimes/arm64/OpenConsole.exe",
        "4313825a3ba634d113cc6ed3bc8ce735e3c19d360cecd985794787479469f91f",
    ),
});

/// No other architecture has the pair: the built-in is used.
#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
const ARCH: Option<Arch> = None;

const DLL: &str = "conpty.dll";
const CONSOLE: &str = "OpenConsole.exe";

type Create = unsafe extern "system" fn(COORD, HANDLE, HANDLE, u32, *mut HPCON) -> HRESULT;
type Resize = unsafe extern "system" fn(HPCON, COORD) -> HRESULT;
type Close = unsafe extern "system" fn(HPCON);
type Release = unsafe extern "system" fn(HPCON) -> HRESULT;

/// One ConPTY's functions, never mixed: all four from the pinned DLL, or
/// all from kernel32.
pub(super) struct Api {
    pub(super) create: Create,
    pub(super) resize: Resize,
    pub(super) close: Close,
    /// `ReleasePseudoConsole`, which lets the output pipe break once the
    /// program ends; kernel32 has it from build 26100 on.
    pub(super) release: Option<Release>,
    /// The pinned files, held unwritable for the process's life.
    _held: Vec<File>,
}

static API: OnceLock<Api> = OnceLock::new();

/// The ConPTY [`load`] chose, or the error saying it wasn't called.
pub(super) fn api() -> Result<&'static Api, Error> {
    API.get().ok_or_else(|| Error {
        call: "load",
        source: std::io::Error::other("pty::load chooses the ConPTY before the first open"),
    })
}

/// Chooses this process's ConPTY, once, before the first [`super::open`]:
/// `Ok` if it is the pinned one in `dir` ([`fetch`]), else the built-in,
/// and `Err` saying why. A second call fails with `AlreadyExists`.
///
/// What the built-in loses (spike S1): sixel pictures and synchronized
/// output are dropped, and titles, links and colours are rewritten. When
/// and how to tell the user is the caller's.
pub fn load(dir: &Path) -> Result<(), Error> {
    if API.get().is_some() {
        return Err(already());
    }
    let (api, why) = match pinned(dir) {
        Ok(api) => (api, None),
        Err(error) => (built_in(), Some(error)),
    };
    API.set(api).map_err(|_| already())?;
    why.map_or(Ok(()), Err)
}

fn already() -> Error {
    Error {
        call: "load",
        source: std::io::ErrorKind::AlreadyExists.into(),
    }
}

/// The pinned pair in `dir`, checked and loaded.
fn pinned(dir: &Path) -> Result<Api, Error> {
    let arch = ARCH.as_ref().ok_or_else(|| Error {
        call: "load",
        source: std::io::Error::other("no pinned ConPTY for this architecture"),
    })?;
    if !dir.is_absolute() {
        return Err(Error {
            call: "load",
            source: std::io::Error::other(format!("{} is not an absolute folder", dir.display())),
        });
    }
    let folder = folder(dir, arch);
    let held = verify(&folder, arch)?;
    let path = wide(&folder.join(DLL));
    use windows_sys::Win32::System::LibraryLoader::{
        LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR, LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW,
    };
    let flags = LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_SYSTEM32;
    // SAFETY: `path` is NUL-ended, a full path to the DLL just checked,
    // whose dependencies are looked for only beside it and in System32.
    let module = unsafe { LoadLibraryExW(path.as_ptr(), std::ptr::null_mut(), flags) };
    if module.is_null() {
        return Err(at("LoadLibraryExW conpty.dll")(
            std::io::Error::last_os_error(),
        ));
    }
    let export = |name: &'static [u8]| {
        function(module, name).ok_or_else(|| Error {
            call: "GetProcAddress",
            source: std::io::Error::other(String::from_utf8_lossy(name).into_owned()),
        })
    };
    let (create, resize, close, release) = (
        export(b"ConptyCreatePseudoConsole\0")?,
        export(b"ConptyResizePseudoConsole\0")?,
        export(b"ConptyClosePseudoConsole\0")?,
        export(b"ConptyReleasePseudoConsole\0")?,
    );
    // The pinned conpty.dll's exports have the signatures of kernel32's
    // functions of the same names without `Conpty`, which these types
    // spell; the module is never freed.
    // SAFETY: as above, `ConptyCreatePseudoConsole`.
    let create = unsafe { std::mem::transmute::<Raw, Create>(create) };
    // SAFETY: as above, `ConptyResizePseudoConsole`.
    let resize = unsafe { std::mem::transmute::<Raw, Resize>(resize) };
    // SAFETY: as above, `ConptyClosePseudoConsole`.
    let close = unsafe { std::mem::transmute::<Raw, Close>(close) };
    // SAFETY: as above, `ConptyReleasePseudoConsole`.
    let release = unsafe { std::mem::transmute::<Raw, Release>(release) };
    Ok(Api {
        create,
        resize,
        close,
        release: Some(release),
        _held: held,
    })
}

type Raw = unsafe extern "system" fn() -> isize;

/// `name`, NUL-ended, exported by `module`.
fn function(module: HMODULE, name: &[u8]) -> Option<Raw> {
    use windows_sys::Win32::System::LibraryLoader::GetProcAddress;
    // SAFETY: `module` is loaded and never freed; `name` is NUL-ended.
    unsafe { GetProcAddress(module, name.as_ptr()) }
}

/// kernel32's ConPTY: its own imports, and `ReleasePseudoConsole` where it
/// has it (looked up, so that fux still starts on a kernel32 without it).
fn built_in() -> Api {
    use windows_sys::Win32::System::Console::{
        ClosePseudoConsole, CreatePseudoConsole, ResizePseudoConsole,
    };
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    let kernel32 = wide(Path::new("kernel32.dll"));
    // SAFETY: the name is NUL-ended; kernel32 is loaded in every process,
    // so nothing is searched for or loaded.
    let module = unsafe { GetModuleHandleW(kernel32.as_ptr()) };
    let release = (!module.is_null())
        .then(|| function(module, b"ReleasePseudoConsole\0"))
        .flatten()
        // SAFETY: kernel32's `ReleasePseudoConsole` takes an `HPCON` and
        // gives an `HRESULT`, as `Release` spells it.
        .map(|raw| unsafe { std::mem::transmute::<Raw, Release>(raw) });
    Api {
        create: CreatePseudoConsole,
        resize: ResizePseudoConsole,
        close: ClosePseudoConsole,
        release,
        _held: Vec::new(),
    }
}

/// Where the pinned pair of `arch` lives under `dir`.
fn folder(dir: &Path, arch: &Arch) -> PathBuf {
    dir.join(format!("{VERSION}-{}", arch.name))
}

/// Both files in `folder`, each opened so that nobody may write, rename or
/// delete it, and hashed through that handle: the handles, if both are the
/// pin.
fn verify(folder: &Path, arch: &Arch) -> Result<Vec<File>, Error> {
    [(DLL, arch.dll.1), (CONSOLE, arch.console.1)]
        .into_iter()
        .map(|(name, pin)| {
            let path = folder.join(name);
            let mut file = std::fs::OpenOptions::new()
                .read(true)
                .share_mode(windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ)
                .open(&path)
                .map_err(at("opening the pinned ConPTY"))
                .map_err(|e| named(e, &path))?;
            let mut bytes = Vec::new();
            file.read_to_end(&mut bytes)
                .map_err(at("reading the pinned ConPTY"))?;
            check(&bytes, pin, &path)?;
            Ok(file)
        })
        .collect()
}

/// `error`, saying which file.
fn named(error: Error, path: &Path) -> Error {
    Error {
        call: error.call,
        source: std::io::Error::new(
            error.source.kind(),
            format!("{}: {}", path.display(), error.source),
        ),
    }
}

/// `bytes`' SHA-256 is `pin` (lowercase hex), or an error naming `path`.
fn check(bytes: &[u8], pin: &str, path: &Path) -> Result<(), Error> {
    let digest = sha256(bytes).map_err(at("BCryptHash"))?;
    let hex: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    match hex == pin {
        true => Ok(()),
        false => Err(Error {
            call: "SHA-256",
            source: std::io::Error::other(format!(
                "{} hashes {hex}, not the pinned {pin}",
                path.display()
            )),
        }),
    }
}

/// Windows' own SHA-256 of `bytes` (D-82: no crate, no hand-written hash).
fn sha256(bytes: &[u8]) -> std::io::Result<[u8; 32]> {
    use windows_sys::Win32::Foundation::RtlNtStatusToDosError;
    use windows_sys::Win32::Security::Cryptography::{BCRYPT_SHA256_ALG_HANDLE, BCryptHash};
    let mut digest = [0u8; 32];
    let count = u32::try_from(bytes.len()).map_err(std::io::Error::other)?;
    // SAFETY: the pseudo-handle needs no opening; `bytes` is readable for
    // `count` bytes and `digest` writable for its 32; no secret.
    let status = unsafe {
        BCryptHash(
            BCRYPT_SHA256_ALG_HANDLE,
            std::ptr::null(),
            0,
            bytes.as_ptr(),
            count,
            digest.as_mut_ptr(),
            32,
        )
    };
    match status {
        0 => Ok(digest),
        // SAFETY: it reads a number and touches no memory.
        _ => Err(std::io::Error::from_raw_os_error(
            unsafe { RtlNtStatusToDosError(status) }.cast_signed(),
        )),
    }
}

/// Makes sure `dir` holds the pinned `conpty.dll` and `OpenConsole.exe`
/// ([`load`] reads them from there): at once if it does; else from the
/// pinned package, a copy of it in `dir` if one was put there by hand, or
/// else downloaded over https by System32's `curl.exe` (at most 60 s), and
/// unpacked by System32's `tar.exe`, each with no console window. The
/// package is hashed before it is unpacked, both files after, and the
/// folder is put in place by one rename only once all three are the pin,
/// so a reader finds no folder or a whole one. Two fetches at once both
/// download; the second rename fails and finds the first's folder.
pub fn fetch(dir: &Path) -> Result<(), Error> {
    let arch = ARCH.as_ref().ok_or_else(|| Error {
        call: "fetch",
        source: std::io::Error::other("no pinned ConPTY for this architecture"),
    })?;
    let done = folder(dir, arch);
    if verify(&done, arch).is_ok() {
        return Ok(());
    }
    std::fs::create_dir_all(dir).map_err(at("making the ConPTY folder"))?;
    sweep(dir);
    let stage = stage(dir)?;
    let fetched = unpack(dir, &stage, arch).and_then(|ready| {
        match std::fs::rename(&ready, &done) {
            Ok(()) => Ok(()),
            // Another fetch's rename came first: its folder is as good.
            Err(_) if verify(&done, arch).is_ok() => Ok(()),
            Err(error) => Err(at("installing the ConPTY folder")(error)),
        }
    });
    // A stage left behind is swept by a later fetch.
    let _ = std::fs::remove_dir_all(&stage);
    fetched
}

/// A new private folder in `dir` to fetch into.
fn stage(dir: &Path) -> Result<PathBuf, Error> {
    use std::sync::atomic::{AtomicU32, Ordering};
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let number = NEXT.fetch_add(1, Ordering::Relaxed);
    let stage = dir.join(format!(".fetch-{}-{number}", std::process::id()));
    std::fs::create_dir(&stage).map_err(at("making a fetch folder"))?;
    Ok(stage)
}

/// Removes the fetch folders a fetch that died left, over an hour old.
fn sweep(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let stale = entry
            .metadata()
            .and_then(|meta| meta.modified())
            .ok()
            .and_then(|modified| modified.elapsed().ok())
            .is_some_and(|age| age > Duration::from_secs(3600));
        if stale && entry.file_name().to_string_lossy().starts_with(".fetch-") {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

/// The package (a copy in `dir`, or downloaded into `stage`), checked,
/// and its two files unpacked, checked, into a folder in `stage`: that
/// folder.
fn unpack(dir: &Path, stage: &Path, arch: &Arch) -> Result<PathBuf, Error> {
    let system = system_directory().map_err(at("GetSystemDirectoryW"))?;
    let dropped = dir.join(PACKAGE);
    let package = match dropped.is_file() {
        true => dropped,
        false => {
            let package = stage.join(PACKAGE);
            let mut curl = std::process::Command::new(system.join("curl.exe"));
            curl.args(["--fail", "--silent", "--show-error", "--location"])
                .args(["--proto", "=https", "--proto-redir", "=https"])
                .args(["--connect-timeout", "5", "--max-time", "60"])
                .args(["--max-filesize", "33554432", "--output"])
                .arg(&package)
                .arg(URL);
            run(curl, "curl.exe")?;
            package
        }
    };
    let bytes = std::fs::read(&package).map_err(at("reading the ConPTY package"))?;
    check(&bytes, PACKAGE_SHA256, &package)?;
    drop(bytes);
    let mut tar = std::process::Command::new(system.join("tar.exe"));
    tar.arg("-xf")
        .arg(&package)
        .arg("-C")
        .arg(stage)
        .args([arch.dll.0, arch.console.0]);
    run(tar, "tar.exe")?;
    let ready = folder(stage, arch);
    std::fs::create_dir(&ready).map_err(at("making the ConPTY folder"))?;
    for (member, name) in [(arch.dll.0, DLL), (arch.console.0, CONSOLE)] {
        std::fs::rename(stage.join(member), ready.join(name))
            .map_err(at("moving an unpacked ConPTY file"))?;
    }
    verify(&ready, arch)?;
    Ok(ready)
}

/// Runs `command`, a System32 tool, with no console window and nothing to
/// read: its error output is the error if it fails.
fn run(mut command: std::process::Command, name: &'static str) -> Result<(), Error> {
    use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;
    let output = command
        .creation_flags(CREATE_NO_WINDOW)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .output()
        .map_err(at(name))?;
    match output.status.success() {
        true => Ok(()),
        false => Err(Error {
            call: name,
            source: std::io::Error::other(format!(
                "{}: {}",
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            )),
        }),
    }
}

/// System32, as the system says (never the PATH).
fn system_directory() -> std::io::Result<PathBuf> {
    use std::os::windows::ffi::OsStringExt;
    use windows_sys::Win32::System::SystemInformation::GetSystemDirectoryW;
    let mut buffer = vec![0u16; 512];
    let room = u32::try_from(buffer.len()).unwrap_or(0);
    // SAFETY: `buffer` is writable for `room` UTF-16 units.
    let length = unsafe { GetSystemDirectoryW(buffer.as_mut_ptr(), room) };
    let length = usize::try_from(length).unwrap_or(usize::MAX);
    match buffer.get(..length) {
        Some(name) if length > 0 => Ok(PathBuf::from(std::ffi::OsString::from_wide(name))),
        _ => Err(std::io::Error::last_os_error()),
    }
}

/// `path` as a NUL-ended UTF-16 string.
fn wide(path: &Path) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    path.as_os_str().encode_wide().chain([0]).collect()
}
