//! Preserve main's plain/styled-wide history RSS workload and measurement points.
use anyhow::{Context, Result, ensure};
use fux_xtask::support::{
    attachment,
    attachment_measure::{drain_all, wait_text},
    local::Root,
};
use serde_json::{Value, json};
use std::{fs, time::Duration};

pub fn run(args: Vec<String>) -> Result<()> {
    let mut args = args.into_iter();
    let binary = fs::canonicalize(
        args.next()
            .context("measure-memory BINARY [--scrollback N] [--rows N] [--columns N]")?,
    )?;
    let mut scrollback: u32 = 10000;
    let mut rows: u16 = 24;
    let mut columns: u16 = 80;
    while let Some(arg) = args.next() {
        let value = args.next().context("missing option value")?;
        match arg.as_str() {
            "--scrollback" => scrollback = value.parse()?,
            "--rows" => rows = value.parse()?,
            "--columns" => columns = value.parse()?,
            _ => anyhow::bail!("unknown measure-memory option: {arg}"),
        }
    }
    ensure!(
        scrollback > 0 && scrollback <= 1_000_000,
        "scrollback must be 1..=1000000"
    );
    ensure!(
        rows > 0 && columns > 12,
        "rows must be positive and columns exceed 12"
    );
    let root = Root::new("fux-memory-", &["/bin/sh".into()])?;
    fs::write(
        root.path().join("config/fux/config.toml"),
        format!(
            "default-command = {{ argv = [\"/bin/sh\"] }}\n[history]\nscrollback-lines = {scrollback}\n"
        ),
    )?;
    let mut server = root.server(&binary)?;
    let result: Result<Value> = (|| {
        let viewer = crate::measure_frames::attach(
            &root.path().join("fux/default.attach.sock"),
            rows,
            columns,
        )?;
        let mut viewers = [viewer];
        let pid = server.child.id();
        drain_all(&mut viewers, Duration::from_millis(500))?;
        let rss_start = crate::measure::rss(pid)?;
        let lines = scrollback + u32::from(rows);
        let width = columns - 12;
        let plain = format!(
            "i=0; while [ $i -lt {lines} ]; do printf '%0{width}d\\\\n' $i; i=$((i+1)); done; printf PLAIN''DONE\\\\n\n"
        );
        attachment::send(
            &mut viewers[0].peer,
            &json!({"type":"input","bytes":plain.as_bytes()}),
        )?;
        wait_text(
            &mut viewers,
            |text| text.contains("PLAINDONE"),
            Duration::from_secs(120),
        )?;
        drain_all(&mut viewers, Duration::from_millis(500))?;
        let rss_plain = crate::measure::rss(pid)?;
        attachment::send(
            &mut viewers[0].peer,
            &json!({"type":"control","request":{"command":"split","id":1,"axis":"horizontal","final_retain_ms":60000}}),
        )?;
        wait_text(&mut viewers, |_| true, Duration::from_secs(5))?;
        drain_all(&mut viewers, Duration::from_secs(1))?;
        let rss_split = crate::measure::rss(pid)?;
        let wide = "日".repeat(usize::from(width / 4));
        let styled = format!(
            "i=0; while [ $i -lt {lines} ]; do printf '\\\\033[1;31m{wide}\\\\033[m%d\\\\n' $i; i=$((i+1)); done; printf WIDE''DONE\\\\n\n"
        );
        attachment::send(
            &mut viewers[0].peer,
            &json!({"type":"input","bytes":styled.as_bytes()}),
        )?;
        wait_text(
            &mut viewers,
            |text| text.contains("WIDEDONE"),
            Duration::from_secs(120),
        )?;
        drain_all(&mut viewers, Duration::from_millis(500))?;
        let rss_wide = crate::measure::rss(pid)?;
        Ok(
            json!({"binary":binary,"scrollback":scrollback,"size":format!("{rows}x{columns}"),
            "rss_start_kib":rss_start,"rss_after_plain_kib":rss_plain,
            "bytes_per_plain_row":per_row(rss_plain,rss_start,scrollback),
            "rss_after_split_kib":rss_split,"rss_after_wide_kib":rss_wide,
            "bytes_per_wide_row":per_row(rss_wide,rss_split,scrollback)}),
        )
    })();
    let cleanup = server.finish();
    let result = result?;
    cleanup?;
    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}

fn per_row(after: u64, before: u64, scrollback: u32) -> i128 {
    // RSS can decrease. Match Python floor division rather than truncating toward zero.
    ((i128::from(after) - i128::from(before)) * 1024).div_euclid(i128::from(scrollback))
}

#[cfg(test)]
mod tests {
    #[test]
    fn negative_rss_deltas_use_floor_division() {
        assert_eq!(super::per_row(10, 11, 10000), -1);
        assert_eq!(super::per_row(11, 10, 10000), 0);
    }
}
