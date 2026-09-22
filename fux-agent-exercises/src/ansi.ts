/**
 * A small ANSI screen decoder for verification evidence.
 *
 * `fux.frame` returns a complete self-contained repaint. Decoding it into rows
 * lets a verifier assert what a human would actually see, instead of matching
 * escape-laden text. This handles only what fux paints: absolute cursor
 * positioning, erases, and printable runs. It is not a terminal emulator and
 * makes no attempt at wrapping, scroll regions or character sets.
 */

export interface DecodedScreen {
  rows: string[];
  /** Rows joined by newlines, with trailing blanks trimmed per row. */
  text: string;
}

export function stripAnsi(input: string): string {
  return input
    .replace(/\u001b\][^\u0007\u001b]*(?:\u0007|\u001b\\)/g, "")
    .replace(/\u001b\[[0-9;?]*[ -/]*[@-~]/g, "")
    .replace(/\u001b[@-Z\\-_]/g, "");
}

export function decodeScreen(paint: string, rows: number, cols: number): DecodedScreen {
  const grid: string[][] = Array.from({ length: rows }, () => Array.from({ length: cols }, () => " "));
  let row = 0;
  let col = 0;
  let index = 0;

  const clearRegion = (fromRow: number, fromCol: number, toRow: number, toCol: number) => {
    for (let r = fromRow; r <= toRow && r < rows; r += 1) {
      const start = r === fromRow ? fromCol : 0;
      const end = r === toRow ? toCol : cols - 1;
      for (let c = start; c <= end && c < cols; c += 1) {
        if (r >= 0 && c >= 0) grid[r][c] = " ";
      }
    }
  };

  while (index < paint.length) {
    const char = paint[index];
    if (char === "\u001b") {
      const next = paint[index + 1];
      if (next === "[") {
        const match = /^\u001b\[([0-9;?]*)([ -/]*)([@-~])/.exec(paint.slice(index));
        if (!match) {
          index += 1;
          continue;
        }
        const [whole, rawParams, , final] = match;
        const params = rawParams.startsWith("?")
          ? []
          : rawParams.split(";").map((part) => (part === "" ? 0 : Number(part)));
        if (final === "H" || final === "f") {
          row = Math.max(0, (params[0] || 1) - 1);
          col = Math.max(0, (params[1] || 1) - 1);
        } else if (final === "J" && !rawParams.startsWith("?")) {
          const mode = params[0] || 0;
          if (mode === 0) clearRegion(row, col, rows - 1, cols - 1);
          else if (mode === 1) clearRegion(0, 0, row, col);
          else clearRegion(0, 0, rows - 1, cols - 1);
        } else if (final === "K" && !rawParams.startsWith("?")) {
          const mode = params[0] || 0;
          if (mode === 0) clearRegion(row, col, row, cols - 1);
          else if (mode === 1) clearRegion(row, 0, row, col);
          else clearRegion(row, 0, row, cols - 1);
        }
        index += whole.length;
        continue;
      }
      if (next === "]") {
        const end = /\u0007|\u001b\\/.exec(paint.slice(index + 2));
        index = end ? index + 2 + end.index + end[0].length : paint.length;
        continue;
      }
      // ESC H homes the cursor in fux's paints; other two-byte forms are ignored.
      if (next === "H") {
        row = 0;
        col = 0;
      }
      index += 2;
      continue;
    }
    if (char === "\r") {
      col = 0;
      index += 1;
      continue;
    }
    if (char === "\n") {
      row = Math.min(rows - 1, row + 1);
      index += 1;
      continue;
    }
    if (char === "\b") {
      col = Math.max(0, col - 1);
      index += 1;
      continue;
    }
    if (char < " ") {
      index += 1;
      continue;
    }
    if (row < rows && col < cols) grid[row][col] = char;
    col += 1;
    index += 1;
  }

  const decoded = grid.map((line) => line.join("").replace(/\s+$/, ""));
  return { rows: decoded, text: decoded.join("\n") };
}
