"""Rasterize captured ANSI-decoded cells, not a desktop screenshot.

Requires Pillow only for this optional verification tool:
  python3 -m venv /tmp/fux-render
  /tmp/fux-render/bin/pip install pillow
  /tmp/fux-render/bin/python verification/render-interactions.py verification/interactions
"""
import json
import re
import sys
from pathlib import Path
from PIL import Image, ImageDraw, ImageFont

root = Path(sys.argv[1])
names = sys.argv[2:] or ["interaction-tabs", "interaction-workspaces", "interaction-menu",
         "interaction-confirm", "interaction-selection", "help", "narrow-help", "tiny"]
captures = [(name, json.loads((root / f"{name}.json").read_text())) for name in names]
font_path = "/System/Library/Fonts/Menlo.ttc"
font = ImageFont.truetype(font_path, 13)
bold = ImageFont.truetype(font_path, 13, index=1)
unicode_font = ImageFont.truetype("/Library/Fonts/Arial Unicode.ttf", 13)
cw, ch, pad, heading = 9, 18, 16, 28
panel_width = max(c["cols"] for _, c in captures) * cw + pad * 2
heights = [max(c["rows"] for _, c in captures[i:i+2]) * ch + pad * 2 + heading
           for i in range(0, len(captures), 2)]
image = Image.new("RGB", (panel_width * 2, sum(heights)), "#222222")
draw = ImageDraw.Draw(image)
palette = ["#111111", "#cc5555", "#55aa55", "#cccc55", "#5555cc", "#aa55aa", "#55aaaa", "#cccccc",
           "#555555", "#ff7777", "#77ff77", "#ffff77", "#7777ff", "#ff77ff", "#77ffff", "#eeeeee"]

def color(value, foreground):
    match = re.fullmatch(r"Idx\((\d+)\)", value)
    if match and int(match[1]) < 16:
        return palette[int(match[1])]
    return "#dddddd" if foreground else "#111111"

y_top = 0
for index, (name, capture) in enumerate(captures):
    if index and index % 2 == 0:
        y_top += heights[index // 2 - 1]
    left = index % 2 * panel_width + pad
    draw.text((left, y_top + pad), name, font=bold, fill="white")
    for y, row in enumerate(capture["cells"]):
        for x, cell in enumerate(row):
            if cell["wide_continuation"]:
                continue
            fg, bg = color(cell["fg"], True), color(cell["bg"], False)
            if cell["inverse"]:
                fg, bg = bg, fg
            if cell["dim"]:
                fg = "#888888"
            wide = x + 1 < len(row) and row[x + 1]["wide_continuation"]
            px, py = left + x * cw, y_top + pad + heading + y * ch
            draw.rectangle((px, py, px + cw * (2 if wide else 1) - 1, py + ch - 1), fill=bg)
            face = unicode_font if wide or any(ord(c) >= 0x2500 for c in cell["text"]) else bold if cell["bold"] else font
            draw.text((px, py), cell["text"], font=face, fill=fg)
image.save(root / "renders.png")
