pub fn extract(
    rows: &[Vec<Option<&str>>],
    wrapped: &[bool],
    start: (usize, usize),
    end: (usize, usize),
) -> String {
    let (start, end) = if start <= end {
        (start, end)
    } else {
        (end, start)
    };
    let mut output = String::new();
    for row in start.0..=end.0 {
        let Some(cells) = rows.get(row) else {
            break;
        };
        let first = if row == start.0 { start.1 } else { 0 };
        let last = if row == end.0 {
            end.1
        } else {
            cells.len().saturating_sub(1)
        };
        let mut line = String::new();
        for column in first..=last {
            if let Some(Some(text)) = cells.get(column) {
                line.push_str(text);
            }
        }
        output.push_str(line.trim_end_matches(' '));
        if row != end.0 && !wrapped.get(row).copied().unwrap_or(false) {
            output.push('\n');
        }
    }
    output
}
