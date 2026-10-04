# fux-vt-gen

Generates fux-vt's grapheme segmentation tables, `src/unicode/tables.rs`,
and the conformance cases its tests run, `tests/data/GraphemeBreakTest.txt`,
from the Unicode Character Database. Its own workspace; not published.

To move to a new Unicode version, from the repository root:

```sh
v=17.0.0
d=$(mktemp -d)
for f in auxiliary/GraphemeBreakProperty.txt auxiliary/GraphemeBreakTest.txt \
         emoji/emoji-data.txt DerivedCoreProperties.txt; do
  curl -sfSL -o "$d/$(basename $f)" "https://www.unicode.org/Public/$v/ucd/$f"
done
cargo run --manifest-path fux-vt/gen/Cargo.toml -- "$d"
```

Then pin `unicode-segmentation`, in fux-vt's dev-dependencies and in
`fux-vt/fuzz`, to a release of the same Unicode version: `unicode::tests`
checks every conformance case and every assigned code point against it,
and that the versions agree.

The tables hold a byte a code point in two stages: `BLOCK` maps each run of
`1 << SHIFT` code points to one of the distinct runs in `PROPERTIES` (the
generator picks the `SHIFT` that makes them smallest). Bits 0–3 are
Grapheme_Cluster_Break, bit 4 Extended_Pictographic, bits 5–6
Indic_Conjunct_Break.
