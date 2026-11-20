#!/usr/bin/env python3
"""Prints the two tables of src/width.rs from this Python's Unicode data.
Run by hand when Unicode moves on: python3 tools/width.py > tables.txt"""
import unicodedata as u

def ranges(pred):
    out, start = [], None
    for c in range(0x300, 0x110000):
        if pred(c):
            start = c if start is None else start
        elif start is not None:
            out.append((start, c - 1))
            start = None
    return out + ([(start, 0x10FFFF)] if start is not None else [])

def zero(c):
    return u.category(chr(c)) in ("Mn", "Me", "Cf") or 0x1160 <= c <= 0x11FF or 0x1F3FB <= c <= 0x1F3FF

def wide(c):
    return not zero(c) and u.east_asian_width(chr(c)) in ("W", "F")

for name, pred in (("ZERO", zero), ("WIDE", wide)):
    print(f"// {name}, Unicode {u.unidata_version}")
    print(" ".join(f"({a:#06x}, {b:#06x})," for a, b in ranges(pred)))
