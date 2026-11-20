#!/usr/bin/env python3
"""Writes the table of src/width.rs from Unicode's data files.

  tools/unicode.py fetch DIR    download the five files of the pinned release into DIR
  tools/unicode.py table DIR    print the table (paste it over the one in src/width.rs)
  tools/unicode.py test DIR     print tests/GraphemeBreakTest.txt, as kept in this repository

Run by hand when Unicode moves on: change RELEASE and the checksums here, fetch, paste, test.
Nothing is downloaded or read when fux is built or tested.
"""
import hashlib, pathlib, sys, urllib.request

RELEASE = "17.0.0"
FILES = {
    "ucd/auxiliary/GraphemeBreakProperty.txt": "d6b51d1d2ae5c33b451b7ed994b48f1f4dc62b2272a5831e7fd418514a6bae89",
    "ucd/auxiliary/GraphemeBreakTest.txt": "e2d134d2c52919bace503ebb6a551c1855fe1a1faec18478c78fff254a1793ec",
    "ucd/emoji/emoji-data.txt": "2cb2bb9455cda83e8481541ecf5b6dfda66a3bb89efa3fa7c5297eccf607b72b",
    "ucd/DerivedCoreProperties.txt": "24c7fed1195c482faaefd5c1e7eb821c5ee1fb6de07ecdbaa64b56a99da22c08",
    "ucd/EastAsianWidth.txt": "ea7ce50f3444a050333448dffef1cadd9325af55cbb764b4a2280faf52170a33",
}
# The order is src/width.rs's: a character's kind is the low four bits of its table entry.
KINDS = "Other CR LF Control Extend ZWJ Regional_Indicator Prepend SpacingMark L V T LV LVT".split()
CONSONANT, LINKER, EXTEND, PICTURE, WIDE, EMOJI, ROOM = 0x10, 0x20, 0x30, 0x40, 0x80, 0x100, 0x200
SYLLABLES = range(0xAC00, 0xD7A4)  # LV and LVT, computed in src/width.rs


def read(folder, name):
    data = (pathlib.Path(folder) / name.split("/")[-1]).read_bytes()
    if hashlib.sha256(data).hexdigest() != FILES[name]:
        raise SystemExit(f"{name}: not the file of Unicode {RELEASE} this tool is pinned to")
    return data.decode()


def properties(text, wanted):
    """(first, last, value) for each line `first..last ; [property ;] value # category …` that
    `wanted` keeps; it is given the fields after the first, and the general category last."""
    for line in text.split("\n"):
        data, _, comment = line.partition("#")
        fields = [field.strip() for field in data.split(";")] + comment.split()[:1]
        if len(fields) >= 3 and (value := wanted(fields[1:])):
            first, _, last = fields[0].partition("..")
            yield int(first, 16), int(last or first, 16), value


def table(folder):
    bits = [0] * 0x110000

    def mark(name, wanted):
        for first, last, value in properties(read(folder, name), wanted):
            for c in range(first, last + 1):
                bits[c] |= value

    mark("ucd/auxiliary/GraphemeBreakProperty.txt", lambda f: KINDS.index(f[0]))
    # A mark that is no nonspacing or enclosing mark and no format character (the halfwidth
    # voiced marks, some Indic vowel signs) has a column of its own in every terminal.
    mark("ucd/auxiliary/GraphemeBreakProperty.txt", lambda f: f[0] == "Extend" and f[-1] not in ("Mn", "Me", "Cf") and ROOM)
    mark("ucd/DerivedCoreProperties.txt", lambda f: f[0] == "InCB" and {"Consonant": CONSONANT, "Linker": LINKER, "Extend": EXTEND}[f[1]])
    mark("ucd/emoji/emoji-data.txt", lambda f: {"Extended_Pictographic": PICTURE, "Emoji_Presentation": WIDE, "Emoji": EMOJI}.get(f[0]))
    mark("ucd/EastAsianWidth.txt", lambda f: f[0] in ("W", "F") and WIDE)
    # A regional indicator is half a flag: one column, so that a flag is two wherever it is drawn.
    for c in range(0x110000):
        if bits[c] & 0xF == KINDS.index("Regional_Indicator"):
            bits[c] &= ~WIDE
    for c in SYLLABLES:
        if bits[c] != (KINDS.index("LVT" if (c - 0xAC00) % 28 else "LV") | WIDE):
            raise SystemExit(f"U+{c:04X} is not the syllable src/width.rs computes")
        bits[c] = 0
    starts = [(c, bits[c]) for c in range(0x80, 0x110000) if c == 0x80 or bits[c] != bits[c - 1]]
    print(f"// A table written by tools/unicode.py from Unicode {RELEASE}: where a run of characters of one kind starts, and the kind.")
    print("#[rustfmt::skip]")
    print(f"const RUNS: [(u32, u16); {len(starts)}] = [")
    for i in range(0, len(starts), 8):
        print("    " + " ".join(f"({c:#x}, {b})," for c, b in starts[i : i + 8]))
    print("];")


if __name__ == "__main__":
    if len(sys.argv) != 3 or sys.argv[1] not in ("fetch", "table", "test"):
        raise SystemExit(__doc__)
    if sys.argv[1] == "fetch":
        pathlib.Path(sys.argv[2]).mkdir(parents=True, exist_ok=True)
        for name in FILES:
            data = urllib.request.urlopen(f"https://www.unicode.org/Public/{RELEASE}/{name}").read()
            (pathlib.Path(sys.argv[2]) / name.split("/")[-1]).write_bytes(data)
            read(sys.argv[2], name)
    elif sys.argv[1] == "table":
        table(sys.argv[2])
    else:
        sys.stdout.write(read(sys.argv[2], "ucd/auxiliary/GraphemeBreakTest.txt"))
