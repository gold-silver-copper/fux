#!/usr/bin/env python3
"""Run every local scenario with the FG fixtures' shared pty checker."""
import sys
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
from fg_selftest import main
if __name__ == '__main__':
    raise SystemExit(main(Path(__file__).resolve().parent))
