# fux-vt-gen

Generates fux-vt's grapheme segmentation and width tables, `src/unicode/tables.rs`,
and the cases of Unicode's own files its tests run (`tests/data/`:
GraphemeBreakTest, NormalizationTest, emoji-test and
emoji-variation-sequences), from the Unicode Character Database. Its own workspace; not published.

To move to a new Unicode version, change the version in
`references/fetch.sh` (its Unicode section), fetch, and generate, from the
repository root:

```sh
references/fetch.sh
cargo run --manifest-path fux-vt/gen/Cargo.toml -- references/unicode
```

`gen` reads the files as the UCD lays them out (`ucd/auxiliary/`,
`ucd/emoji/`). compare.yml fetches them at the pinned version, by SHA-256,
runs `gen` and fails if what it writes differs from what is committed.

Then pin `unicode-segmentation` and `unicode-width`, each exactly
(`=`), in fux-vt's dev-dependencies and in `fux-vt/fuzz`, to
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
Default_Ignorable combining mark, bit 14 a character the cluster width
rule looks at.

## The conformance table's catalogue

`fux-vt-gen catalogue references` reads the control functions the
references define (ECMA-48's 8.3, DEC STD 070's function pages, ctlseqs'
headings), from the documents `references/fetch.sh` fetches, through
`pdftotext -layout` (poppler), and rewrites the first four columns of
`fux-vt/tests/data/conformance.txt`. A row keeps its status, probe, tests
and note; a function new to the references comes in as `unknown`, which
`tests/table.rs` fails until it is given one. DEC STD 070 is a scan:
`DEC_FIXES` in `src/catalogue.rs` mends the headings its text layer
misreads.
