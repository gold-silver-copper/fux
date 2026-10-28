//! A bounded comparison of placement policies: a Unicode redraw and an
//! ASCII key's changed cell. Times cover painting, not transport or display.
use fux::render::{Grid, paint_into};
use fux_vt::{Attributes, CellRef};
use std::hint::black_box;
use std::time::Instant;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cpu = fuxix::process::thread_cpu_time;
    for text in ["ascii", "unicode"] {
        for cells in [false, true] {
            let mut grid = Grid::new(40, 120)?;
            grid.cell_placement = cells;
            for i in 0..4800 {
                let glyph = if text == "unicode" && i % 3 == 0 {
                    "α"
                } else {
                    "a"
                };
                grid.cells
                    .set(i, CellRef::new(glyph, false, Attributes::default()));
            }
            for redraw in [true, false] {
                let mut shown = grid.clone();
                let mut out = Vec::new();
                let mut bytes = 0usize;
                let mut samples = Vec::new();
                let begin = cpu()?;
                for i in 0..1000 {
                    grid.cells.set(
                        0,
                        CellRef::new(
                            if i % 2 == 0 { "x" } else { "y" },
                            false,
                            Attributes::default(),
                        ),
                    );
                    out.clear();
                    let start = Instant::now();
                    paint_into(&shown, !redraw, &grid, &mut out);
                    samples.push(start.elapsed());
                    bytes = bytes.saturating_add(out.len());
                    black_box(&out);
                    std::mem::swap(&mut shown, &mut grid);
                }
                let elapsed = cpu()?.saturating_sub(begin);
                samples.sort();
                let p50 = samples.get(500).ok_or("no median")?;
                let p95 = samples.get(950).ok_or("no p95")?;
                println!(
                    "{text} cells={cells} redraw={redraw}: bytes/frame={} cpu/frame={:.2}us p50={:.2}us p95={:.2}us",
                    bytes / 1000,
                    elapsed.as_secs_f64() * 1000.0,
                    p50.as_secs_f64() * 1e6,
                    p95.as_secs_f64() * 1e6
                );
            }
        }
    }
    Ok(())
}
