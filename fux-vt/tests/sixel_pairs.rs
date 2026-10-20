//! rio's sixel pairs (`tests/data/rio-sixel`, rio 59203ccd, MIT): each
//! picture, decoded through fux-vt's parser, is the size its `.rgba`
//! golden says, 64 by 64, and each pixel is the golden's: alpha exactly,
//! each colour channel within 3 of 255. The tolerance was fixed before the
//! first run: sixel colours are given in percent (DEC STD 070-9, 8.1.3),
//! and a percent is 2.55 of 255, so two decoders' rounding may differ by
//! up to 3. `ppmtosixel` frames its picture in 8-bit DCS and ST (0x90,
//! 0x9c), which in UTF-8, as fux-vt reads its input, are no controls
//! (ECMA-35, 15.4; README): they are given as `ESC P` and `ESC \`, the
//! picture's bytes as they are.

use fux_vt::{Feature, Graphics, Options, Parser, Size};
type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

const TOLERANCE: u8 = 3;
const SIDE: u32 = 64;

fn check(name: &str) -> Result {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/rio-sixel");
    let mut sixel = Vec::new();
    for &byte in &std::fs::read(dir.join(format!("{name}.sixel")))? {
        match byte {
            0x90 => sixel.extend_from_slice(b"\x1bP"),
            0x9c => sixel.extend_from_slice(b"\x1b\\"),
            _ => sixel.push(byte),
        }
    }
    let golden = std::fs::read(dir.join(format!("{name}.rgba")))?;
    let options = Options::new().with(Feature::Pictures);
    let mut p = Parser::with_options(Size::new(24, 80)?, 0, options)?;
    p.set_host_graphics(Graphics {
        cell: Some((8, 16)),
        sixel: true,
    });
    p.process(&sixel)?;
    let shown: Vec<_> = p.screen().pictures().collect();
    let [shown] = shown.as_slice() else {
        return Err(format!("{name}: {} pictures", shown.len()).into());
    };
    let picture = shown.picture;
    assert_eq!((picture.width(), picture.height()), (SIDE, SIDE), "{name}");
    let mut far = Vec::new();
    for (i, want) in golden.as_chunks::<4>().0.iter().enumerate() {
        let i = u32::try_from(i)?;
        let got = picture.pixel(i % SIDE, i / SIDE).ok_or("no pixel")?;
        let alpha = got.get(3) == want.get(3);
        let close = got
            .iter()
            .zip(want)
            .take(3)
            .all(|(a, b)| a.abs_diff(*b) <= TOLERANCE);
        if !(alpha && close) {
            far.push((i, got, *want));
        }
    }
    let first: Vec<_> = far.iter().take(4).collect();
    assert!(
        far.is_empty(),
        "{name}: {} pixels past the tolerance, first {first:?}",
        far.len()
    );
    Ok(())
}

#[test]
fn imagemagick_s_picture_decodes_to_its_golden() -> Result {
    check("testimage_im6")
}

#[test]
fn libsixel_s_picture_decodes_to_its_golden() -> Result {
    check("testimage_libsixel")
}

#[test]
fn ppmtosixel_s_picture_decodes_to_its_golden() -> Result {
    check("testimage_ppmtosixel")
}
