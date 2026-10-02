#!/usr/bin/env bash
# Downloads the specifications fux-vt is checked against into this folder,
# beside this script. The documents themselves are not committed: ECMA, ITU,
# DEC (through bitsavers), Unicode and the others let anyone download them,
# but not obviously redistribute them. README.md says what each is for.
#
# Run it again at any time; it skips what is already here. Pass --force to
# fetch everything again.
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
force=${1:-}
# bitsavers refuses requests without a browser's user agent.
ua='Mozilla/5.0 (Macintosh; Intel Mac OS X 14_0) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.0 Safari/605.1.15'
failed=()

get() {
  local path=$here/$1 url=$2
  if [[ -s $path && $force != --force ]]; then
    return
  fi
  mkdir -p "$(dirname "$path")"
  if curl -sSfL -A "$ua" --retry 2 --max-time 300 -o "$path.part" "$url"; then
    mv "$path.part" "$path"
    echo "fetched $1"
  else
    rm -f "$path.part"
    failed+=("$1 <- $url")
  fi
}

# A site's pages under one directory, as a browsable local copy.
mirror() {
  local dir=$here/$1 url=$2
  if [[ -d $dir && $force != --force ]]; then
    return
  fi
  mkdir -p "$dir"
  # wget exits 8 when any link on the way is broken, which some are on
  # vt100.net; the copy counts if its contents page arrived.
  wget -q -e robots=off -U "$ua" --mirror --no-parent --convert-links \
    --adjust-extension --page-requisites --no-host-directories \
    --cut-dirs="$3" -P "$dir" "$url" || true
  if [[ -s $dir/contents.html ]]; then
    echo "mirrored $1"
  else
    failed+=("$1 <- $url")
  fi
}

# --- Formal standards -------------------------------------------------------
get standards/ECMA-48_5th_edition_1991.pdf \
  https://ecma-international.org/wp-content/uploads/ECMA-48_5th_edition_june_1991.pdf
get standards/ECMA-35_6th_edition_1994.pdf \
  https://ecma-international.org/wp-content/uploads/ECMA-35_6th_edition_december_1994.pdf
get standards/ECMA-43_3rd_edition_1991.pdf \
  https://ecma-international.org/wp-content/uploads/ECMA-43_3rd_edition_december_1991.pdf
get standards/ECMA-6_6th_edition_1991.pdf \
  https://ecma-international.org/wp-content/uploads/ECMA-6_6th_edition_december_1991.pdf
get standards/ITU-T_T.416_1993.pdf \
  'https://www.itu.int/rec/dologin_pub.asp?lang=e&id=T-REC-T.416-199303-I!!PDF-E&type=items'

# --- DEC ----------------------------------------------------------------------
bitsavers=https://bitsavers.org/pdf/dec
get dec/DEC_STD_070_Video_Systems_Reference_Manual_1991.pdf \
  $bitsavers/standards/EL-SM070-00_DEC_STD_070_Video_Systems_Reference_Manual_Dec91.pdf
get dec/VT520_VT525_Programmer_Information_1994.pdf \
  $bitsavers/terminal/vt5xx/EK-VT520-RM_VT520_VT525_Programmer_Information_Jul94.pdf
get dec/VT510_520_Spec.pdf \
  $bitsavers/terminal/vt5xx/VT510_520_Spec.pdf
get dec/VT330_VT340_Text_Programming_1988.pdf \
  $bitsavers/terminal/vt340/EK-VT3XX-TP-002_VT330_VT340_Text_Programming_198805.pdf
get dec/VT220_Programmer_Pocket_Guide_1984.pdf \
  $bitsavers/terminal/vt220/EK-VT220_HR-002_VT220_Programmer_Pocket_Guide_198407.pdf
get dec/VT220_Technical_Manual_1984.pdf \
  $bitsavers/terminal/vt220/EK-VT220-TM-001_VT220_Technical_Manual_Nov84.pdf
get dec/VT100_User_Guide_1979.pdf \
  $bitsavers/terminal/vt100/EK-VT100-UG-002_VT100_User_Guide_Jan79.pdf
get dec/VT100_Technical_Manual_1982.pdf \
  $bitsavers/terminal/vt100/EK-VT100-TM-003_VT100_Technical_Manual_Jul82.pdf
get dec/VT100_Programming_Reference_Card_1982.pdf \
  $bitsavers/terminal/vt100/EK-VT100-RC_002_VT100_Programming_Reference_Card_1982.pdf
# vt100.net's transcriptions: searchable, and linked section by section.
mirror dec/vt100.net/vt510-rm https://vt100.net/docs/vt510-rm/contents.html 2
mirror dec/vt100.net/vt220-rm https://vt100.net/docs/vt220-rm/contents.html 2
mirror dec/vt100.net/vt100-ug https://vt100.net/docs/vt100-ug/contents.html 2
# The state diagram is inline SVG in the page.
get dec/vt100.net/dec_ansi_parser.html https://vt100.net/emu/dec_ansi_parser

# --- xterm and terminfo -------------------------------------------------------
get xterm/ctlseqs.html https://invisible-island.net/xterm/ctlseqs/ctlseqs.html
get xterm/ctlseqs.pdf https://invisible-island.net/xterm/ctlseqs/ctlseqs.pdf
get xterm/terminfo.src.gz https://invisible-island.net/datafiles/current/terminfo.src.gz
if [[ -s $here/xterm/terminfo.src.gz && ( ! -s $here/xterm/terminfo.src || $force == --force ) ]]; then
  gunzip -c "$here/xterm/terminfo.src.gz" > "$here/xterm/terminfo.src"
fi
get xterm/vttest.tar.gz https://invisible-island.net/datafiles/release/vttest.tar.gz
if [[ -s $here/xterm/vttest.tar.gz && ( ! -d $here/xterm/vttest || $force == --force ) ]]; then
  rm -rf "$here/xterm/vttest" && mkdir -p "$here/xterm/vttest"
  tar -xzf "$here/xterm/vttest.tar.gz" -C "$here/xterm/vttest" --strip-components=1
fi
if [[ ! -d $here/xterm/esctest2 ]]; then
  git clone -q --depth 1 https://github.com/ThomasDickey/esctest2 "$here/xterm/esctest2" \
    && echo "cloned xterm/esctest2" || failed+=("xterm/esctest2")
fi

# --- Unicode, at the version fux-vt's tables follow (17.0.0) -------------------
ucd=https://www.unicode.org/Public/17.0.0/ucd
get unicode/UAX29_Text_Segmentation.html https://www.unicode.org/reports/tr29/tr29-47.html
get unicode/UAX11_East_Asian_Width.html https://www.unicode.org/reports/tr11/tr11-44.html
get unicode/UTS51_Emoji.html https://www.unicode.org/reports/tr51/tr51-29.html
get unicode/ucd/EastAsianWidth.txt $ucd/EastAsianWidth.txt
get unicode/ucd/DerivedCoreProperties.txt $ucd/DerivedCoreProperties.txt
get unicode/ucd/auxiliary/GraphemeBreakProperty.txt $ucd/auxiliary/GraphemeBreakProperty.txt
get unicode/ucd/auxiliary/GraphemeBreakTest.txt $ucd/auxiliary/GraphemeBreakTest.txt
get unicode/ucd/emoji/emoji-data.txt $ucd/emoji/emoji-data.txt

# --- Modern extensions, each its own de facto specification -------------------
get modern/kitty_keyboard_protocol.html https://sw.kovidgoyal.net/kitty/keyboard-protocol/
get modern/osc8_hyperlinks.md https://gist.github.com/egmontkob/eb114294efbcd5adb1944c9f3cb5feda/raw
get modern/mode_2026_synchronized_output.md \
  https://raw.githubusercontent.com/contour-terminal/vt-extensions/master/synchronized-output.md
get modern/mode_2027_grapheme_clusters.tex \
  https://raw.githubusercontent.com/contour-terminal/terminal-unicode-core/master/spec/terminal-unicode-core.tex
get modern/mode_2048_in_band_resize.md https://gist.github.com/rockorager/e695fb2924d36b2bcf1fff4a3704bd83/raw

if (( ${#failed[@]} )); then
  printf 'could not fetch:\n' >&2
  printf '  %s\n' "${failed[@]}" >&2
  exit 1
fi
echo "references: all present in $here"
