#!/usr/bin/env python3
"""Refreshes the pinned xterm-256color output bytes in corpus/terminfo/ with local tput.
Edit Unicode, resize, control and session cases directly in corpus/.
"""
import pathlib, subprocess

HERE = pathlib.Path(__file__).resolve().parent / "corpus" / "terminfo"
TERM = "xterm-256color"


def literal(data):
    """Bytes as the inside of a Python bytes literal, with no blank at either end."""
    return "".join(chr(b) if 33 <= b < 127 and b != 92 else {13: "\\r", 10: "\\n", 92: "\\\\"}.get(b, f"\\x{b:02x}") for b in data)


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
    HERE.mkdir(parents=True, exist_ok=True)
    for name, data in made.items():
        contents = "size 12x6\nfeed " + literal(filled + data + b"@") + "\n"
        (HERE / f"{name}.case").write_text(contents, encoding="utf-8")


terminfo()
