"""quotes_check.py LOG TRANSCRIPT: every line of TRANSCRIPT that starts with '> ' (a quoted output line)
must appear verbatim in some recorded call's stdout or stderr in LOG (the shim's NDJSON). Prints the misses."""
import json, sys
calls = [json.loads(l) for l in open(sys.argv[1])]
seen = "\n".join(c["out"] + c["err"] for c in calls)
quotes = [l[2:].rstrip("\n") for l in open(sys.argv[2]) if l.startswith("> ")]
missing = [q for q in quotes if q not in seen]
for q in missing: print("not in the call log:", q)
print(f"{len(quotes) - len(missing)}/{len(quotes)} quoted lines found in {len(calls)} recorded calls")
sys.exit(1 if missing else 0)
