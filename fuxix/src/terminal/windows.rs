//! A pseudo console's size on Windows. Windows keeps no size in pixels
//! and no foreground group for a console (D-137): fux-vt's in-band
//! answers give the pixels, and the group is always none.
use super::{Pixels, Size};
use crate::errno::Result;
use crate::process::Pid;
use crate::pty::Master;

/// The size `master`'s pseudo console was last set to ([`set_window_size`]
/// or [`crate::pty::open`]), with no pixels: Windows keeps none.
pub fn window_size(master: &Master) -> Option<(Size, Option<Pixels>)> {
    Some((master.size(), None))
}

/// Resizes `master`'s pseudo console to `size`; `pixels` is not kept, as
/// Windows has nowhere to keep it. The program learns of it as a console
/// program does (a window-size event, and `GetConsoleScreenBufferInfo`).
/// A size over 32767 is refused with `INVAL`.
pub fn set_window_size(master: &Master, size: Size, pixels: Option<Pixels>) -> Result<()> {
    let _ = pixels;
    master.resize(size)
}

/// The foreground process group of `master`'s pseudo console: always none,
/// as a Windows console has no process groups to put in front (D-137).
pub fn foreground_group(master: &Master) -> Result<Option<Pid>> {
    let _ = master;
    Ok(None)
}
