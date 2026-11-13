#!/usr/bin/env python3
"""List Claude Code transcripts that invoked a skill (default: shepherd-nodes).

Usage: python3 -I find_runs.py [skill-name] [--ledger <reviews.md>]

Prints one line per transcript: path, size, first and last timestamps, how many
times the skill was invoked, and whether the ledger already lists the file (and
up to which timestamp it was reviewed).
"""
import json
import os
import re
import sys

args = sys.argv[1:]
ledger_path = None
if "--ledger" in args:
    i = args.index("--ledger")
    ledger_path = args[i + 1]
    del args[i : i + 2]
skill = args[0] if args else "shepherd-nodes"
marker = f"<command-name>/{skill}</command-name>"

reviewed = {}
if ledger_path and os.path.exists(ledger_path):
    for line in open(ledger_path, encoding="utf-8"):
        m = re.match(r"^- `([0-9a-f-]{36})` reviewed through (\S+)", line)
        if m:
            reviewed[m.group(1)] = m.group(2)

root = os.path.expanduser("~/.claude/projects")
rows = []
for d in os.listdir(root):
    full = os.path.join(root, d)
    if not os.path.isdir(full):
        continue
    for name in os.listdir(full):
        if not name.endswith(".jsonl"):
            continue
        path = os.path.join(full, name)
        first = last = None
        invocations = []
        with open(path, encoding="utf-8", errors="replace") as f:
            for line in f:
                ts = None
                if '"timestamp"' in line:
                    m = re.search(r'"timestamp":"([^"]+)"', line)
                    ts = m.group(1) if m else None
                    if ts:
                        first = first or ts
                        last = ts
                if marker in line and '"type":"user"' in line:
                    try:
                        o = json.loads(line)
                    except json.JSONDecodeError:
                        continue
                    if o.get("isSidechain"):
                        continue
                    c = o.get("message", {}).get("content")
                    text = c if isinstance(c, str) else " ".join(
                        x.get("text", "") for x in c or [] if isinstance(x, dict)
                    )
                    if not text.lstrip().startswith("<command-message>"):
                        continue  # the marker quoted in tool output, not an invocation
                    a = re.search(r"<command-args>(.*?)</command-args>", text, re.S)
                    invocations.append((ts, (a.group(1) if a else "").strip()[:120]))
        if invocations:
            sid = name[:-6]
            rows.append((first, path, os.path.getsize(path), last, invocations, reviewed.get(sid)))

for first, path, size, last, inv, rev in sorted(rows):
    status = f"reviewed through {rev}" if rev else "NOT REVIEWED"
    if rev and last and last > rev:
        status += " (has newer activity)"
    print(f"{path}\n  {size/1e6:.1f} MB  {first} .. {last}  {status}")
    for ts, a in inv:
        print(f"  invoked {ts}: {a!r}")
