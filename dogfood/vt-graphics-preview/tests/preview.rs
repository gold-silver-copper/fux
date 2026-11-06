//! What the previewer promises: a PPM it encodes comes back pixel for pixel
//! from fux-vt at the cells its size needs, and the test-bed's report says
//! which pictures were kept, let go, clipped, cut and dropped.

use vt_graphics_preview::{Setup, demo, encode_sixel, parse_ppm, preview};

const SETUP: Setup = Setup {
    rows: 12,
    cols: 40,
    cell: (10, 20),
    history_bytes: 65536,
};

#[test]
fn an_encoded_ppm_is_kept_pixel_for_pixel_at_its_cells() -> Result<(), String> {
    // 25x41 pixels in 250 colours: 3x3 cells of 10x20, the last partly filled.
    let mut ppm = String::from("P3\n# a test\n25 41\n255\n");
    for y in 0..41u32 {
        for x in 0..25u32 {
            ppm.push_str(&format!("{} {} {}\n", x * 10, (y % 5) * 60, (x + y) % 2 * 255));
        }
    }
    let image = parse_ppm(ppm.as_bytes())?;
    let report = preview(&encode_sixel(&image), SETUP)?;
    assert_eq!(
        (report.placed, report.lost_or_cut),
        (1, 0),
        "{}",
        report.text
    );
    let shown = report
        .parser
        .screen()
        .pictures()
        .next()
        .ok_or("no picture")?;
    assert_eq!((shown.row, shown.col, shown.cols, shown.rows), (0, 0, 3, 3));
    assert_eq!((shown.picture.width(), shown.picture.height()), (25, 41));
    for (i, rgb) in image.rgb.iter().enumerate() {
        let (x, y) = (i as u32 % 25, i as u32 / 25);
        let got = shown.picture.pixel(x, y).ok_or("no pixel")?;
        // Colours go through sixel's percent registers: within 2 of 255.
        for (a, b) in got.iter().zip(rgb) {
            assert!(a.abs_diff(*b) <= 2, "pixel {x},{y}: {got:?} vs {rgb:?}");
        }
    }
    Ok(())
}

#[test]
fn the_test_bed_report_tells_kept_let_go_clipped_cut_and_dropped() -> Result<(), String> {
    let report = preview(&demo(SETUP.rows, SETUP.cols, SETUP.cell), SETUP)?;
    let t = &report.text;
    // Kept, written over, cut at the edge, cut past 4096; CAN ends one unseen.
    assert_eq!(
        (report.placed, report.lost_or_cut, report.let_go),
        (3, 1, 1),
        "{t}"
    );
    assert!(t.contains("picture #2 let go"), "{t}");
    assert!(t.contains("clipped to 3 of 6 columns"), "{t}");
    assert!(t.contains("clipped to 40 of 410 columns"), "{t}");
    assert!(t.contains("dropped: 1 kitty graphics"), "{t}");
    assert!(t.contains("dropped: 1 iTerm2 image"), "{t}");
    // Without history the first picture scrolls off the top and goes.
    let bare = preview(
        &demo(12, 40, (10, 20)),
        Setup {
            history_bytes: 0,
            ..SETUP
        },
    )?;
    assert_eq!(bare.shown.len(), 1, "{}", bare.text);
    Ok(())
}
