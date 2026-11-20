#!/usr/bin/env python3
"""Writes the generated part of the corpus again: tools/referee/corpus.py

terminfo/  every output capability of `infocmp -x xterm-256color` (not the keys, k…, nor the
           shapes of replies: u6, u8, rv, xr, xm), each applied to a 12x6 screen
           full of text with the cursor at row 3, column 5, then `@` printed where the cursor is
unicode/   each string after `a` and before `|`, and again started in a row's last-but-one column
resize/    text, then sizes

The cases are kept as files and this is only how they were made: sessions/ and controls/ are
written by hand or recorded, and are not touched.
"""
import pathlib, subprocess

HERE = pathlib.Path(__file__).resolve().parent / "corpus"
TERM = "xterm-256color"


def literal(data):
    """Bytes as the inside of a Python bytes literal, with no blank at either end."""
    out = "".join(chr(b) if 33 <= b < 127 and b != 92 else {13: "\\r", 10: "\\n", 92: "\\\\"}.get(b, f"\\x{b:02x}") for b in data)
    return out


def case(folder, name, size, *lines):
    (HERE / folder).mkdir(parents=True, exist_ok=True)
    (HERE / folder / f"{name}.case").write_text("\n".join([f"size {size}", *lines]) + "\n", encoding="utf-8")


def feed(data):
    return "feed " + literal(data)


def tput(*args):
    return subprocess.run(["tput", "-T", TERM, *map(str, args)], capture_output=True, check=True).stdout


def terminfo():
    filled = b"\x1b[H" + b"\r\n".join(f"{r}0123456789{r}".encode() for r in "abcdef") + b"\x1b[3;5H"
    made = {}
    plain = "bel blink bold cbt civis clear cnorm cr cub1 cud1 cuf1 cuu1 cvvis dch1 dim dl1 ed el el1 home ht hts il1 ind invis nel rc rev ri ritm rmacs rmam rmcup rmir rmkx rmm rmso rmul sc sgr0 sitm smacs smam smcup smir smkx smso smul tbc op oc E3 rmxx smxx Se Cr BD BE PS PE fd fe rs1 rs2 is2 flash meml memu smm mc0 mgc"
    for name in plain.split():
        made[name] = tput(name)
    for name in "cub cud cuf cuu dch dl ech ich il indn rin hpa vpa".split():
        made[name] = tput(name, 2)
    made["rep"] = tput("rep", 65, 3)
    made["cup"] = tput("cup", 1, 7)
    made["csr"] = tput("csr", 1, 3)
    made["csr+lf"] = tput("csr", 1, 3) + tput("cup", 3, 0) + b"\n\n"
    made["csr+ri"] = tput("csr", 1, 3) + tput("cup", 1, 0) + tput("ri") * 2
    made["sc+rc"] = tput("sc") + tput("cup", 0, 0) + b"Z" + tput("rc")
    made["smcup+rmcup"] = tput("smcup") + b"ALT" + tput("rmcup")
    made["smir+text"] = tput("smir") + b"II" + tput("rmir")
    made["rmam+text"] = tput("rmam") + b"QQQQQQQQQQ"
    made["smacs+text"] = tput("smacs") + b"lqk" + tput("rmacs")
    made["tabs"] = tput("tbc") + tput("cup", 2, 9) + tput("hts") + tput("cup", 2, 0) + tput("ht")
    made["setaf+setab"] = tput("setaf", 1) + tput("setab", 4) + tput("setaf", 200) + tput("setab", 12)
    made["RV"] = tput("RV")
    made["XR"] = tput("XR")
    made["meml+memu"] = tput("cup", 2, 0) + tput("meml") + tput("cup", 5, 0) + b"\n\nX" + tput("memu") + b"\nY"
    made["E3"] = tput("cup", 5, 0) + b"\n\n\n" + tput("E3")
    made["sgr"] = tput("sgr", 1, 0, 1, 0, 0, 1, 0, 0, 0)
    made["Ms"] = tput("Ms", "c", "dGVzdA==")
    made["Ss"] = tput("Ss", 2)
    made["Cs"] = tput("Cs", "red")
    made["XM"] = tput("XM", 1)
    made["initc"] = tput("initc", 1, 500, 0, 0)
    acsc = tput("acsc")
    made["acsc"] = tput("clear") + tput("smacs") + acsc[0::2][:12] + b"\r\n" + acsc[0::2][12:24] + b"\r\n" + acsc[0::2][24:] + tput("rmacs")
    made["mc5+mc4"] = tput("mc5") + b"P" + tput("mc4")
    made["smglp"] = tput("smglp", 2) + b"\r\nL"
    made["smgrp"] = tput("smgrp", 8) + b"RRRRRRRRRR"
    made["smglr"] = tput("smglr", 2, 8) + b"\r\nMMMMMMMMMM"
    made["u6+u7"] = tput("u7")
    made["u8+u9"] = tput("u9")
    for name, data in made.items():
        case("terminfo", name, "12x6", feed(filled + data + b"@"))


UNICODE = {
    "flag": "\U0001F1EF\U0001F1F5",
    "flag-lone": "\U0001F1EF",
    "flags-three": "\U0001F1EF\U0001F1F5\U0001F1FA",
    "family": "\U0001F468‍\U0001F469‍\U0001F467",
    "family-four": "\U0001F468‍\U0001F469‍\U0001F467‍\U0001F466",
    "skin-tone": "\U0001F44D\U0001F3FD",
    "hangul-jamo": "각",
    "hangul-syllable": "한",
    "devanagari": "क्ष",
    "devanagari-joiner": "क्‍ष",
    "vs16": "❤️",
    "vs15": "\U0001F600︎",
    "keycap": "1️⃣",
    "joiner-after-letter": "x‍",
    "e-acute": "é",
    "three-marks": "é̂̃",
    "cjk": "一",
    "fullwidth": "Ａ",
    "ambiguous": "§①α",
    "zero-width-space": "b​c",
    "prepend": "؀x",
    "thai": "กำ",
    "tamil": "நி",
    "mark-after-letter": "́",
}


def unicode():
    for name, string in UNICODE.items():
        escaped = string.encode("unicode_escape").decode()
        case("unicode", name, "16x2", "text a" + escaped + "|")
        case("unicode", name + "-at-end", "16x2", "feed \\x1b[1;15H", "text a" + escaped + "|")
    case("unicode", "mark-first", "16x2", "text \\u0301|")
    case("unicode", "mark-after-move", "16x2", "feed a\\x1b[1;2H", "text \\u0301|")


def resize():
    line = bytes(range(48, 48 + 75)).replace(b"\\", b"/") + b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRS"
    labels = b"\r\n".join(f"R{n}".encode() for n in range(1, 7))
    wide = "一二三四五六七八".encode("unicode_escape").decode()
    case("resize", "narrow-and-back", "120x10", feed(line + b"\r\nend"), "resize 40x10", "resize 120x10")
    case("resize", "narrow", "120x10", feed(line + b"\r\nend"), "resize 40x10")
    case("resize", "widen", "40x10", feed(line[:100] + b"\r\nend"), "resize 120x10")
    case("resize", "shorter", "20x6", feed(labels), "resize 20x3")
    case("resize", "shorter-and-back", "20x6", feed(labels), "resize 20x3", "resize 20x6")
    case("resize", "taller", "20x3", feed(labels), "resize 20x6")
    case("resize", "narrow-cursor-mid-line", "40x6", feed(line[:30] + b"\x1b[1;25H"), "resize 10x6")
    case("resize", "narrow-wide-characters", "16x6", "text " + wide, "resize 7x6")
    case("resize", "narrow-wide-and-back", "16x6", "text " + wide, "resize 7x6", "resize 16x6")
    case("resize", "narrow-full-screen", "20x4", feed(b"\r\n".join(line[n : n + 20] for n in (0, 20, 40, 60))), "resize 10x4")
    case("resize", "alternate-narrow", "20x6", feed(b"main text here\x1b[?1049h" + line[:20] + b"\r\n" + line[20:35]), "resize 10x6")
    case("resize", "alternate-leave-after", "20x6", feed(b"main text that is long\x1b[?1049halt"), "resize 10x6", feed(b"\x1b[?1049l"))
    case("resize", "region-then-resize", "20x6", feed(labels + b"\x1b[2;4r"), "resize 20x4", feed(b"\x1b[4;1H\nZ"))
    case("resize", "hard-lines-narrow", "20x6", feed(b"one\r\ntwo two two two\r\nthree"), "resize 8x6")


terminfo()
unicode()
resize()
