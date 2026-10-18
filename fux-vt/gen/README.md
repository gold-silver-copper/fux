# fux-vt-gen

Generates fux-vt's grapheme segmentation and width tables, `src/unicode/tables.rs`,
and the conformance cases its tests run, `tests/data/GraphemeBreakTest.txt`,
from the Unicode Character Database. Its own workspace; not published.

To move to a new Unicode version, from the repository root:

```sh
v=17.0.0
d=$(mktemp -d)
for f in auxiliary/GraphemeBreakProperty.txt auxiliary/GraphemeBreakTest.txt \
         emoji/emoji-data.txt emoji/emoji-variation-sequences.txt \
         DerivedCoreProperties.txt EastAsianWidth.txt HangulSyllableType.txt \
         PropList.txt UnicodeData.txt; do
  curl -sfSL -o "$d/$(basename $f)" "https://www.unicode.org/Public/$v/ucd/$f"
done
cargo run --manifest-path fux-vt/gen/Cargo.toml -- "$d"
```

Then pin `unicode-segmentation`, in fux-vt's dev-dependencies and in
`fux-vt/fuzz`, and `unicode-width` in fux-vt's dev-dependencies, to
releases of the same Unicode version (and check `src/unicode/width.rs`
against unicode-width's `width_in_str` for rules it added): `unicode::tests`
checks every conformance case, every code point and many clusters against
them, and that the versions agree.

The tables hold sixteen bits a code point in two stages: `BLOCK` maps each run of
`1 << SHIFT` code points to one of the distinct runs in `PROPERTIES` (the
generator picks the `SHIFT` that makes them smallest). Bits 0–3 are
Grapheme_Cluster_Break, bit 4 Extended_Pictographic, bits 5–6
Indic_Conjunct_Break, bits 7–8 the width alone (3: a control), bit 9
emoji presentation, bits 10–11 the start of an emoji and a text
presentation sequence, bit 12 Emoji_Modifier_Base, bit 13 a
Default_Ignorable combining mark.
