#!/usr/bin/env python3
"""List Claude Code transcripts that invoked a skill (default: shepherd-nodes).

Usage: python3 -I find_runs.py [skill-name] [--ledger <reviews.md>]
       python3 -I find_runs.py [skill-name] --ledger <reviews.md> --mark <transcript> "<title>"

Prints one line per transcript: path, size, first and last timestamps, how many
times the skill was invoked, and its review status:
  NOT REVIEWED       not in the ledger
  STALE              reviewed by a version older than `reviews_ok_from` in
                     ../SKILL.md: review it again, in full (no --since)
  reviewed through   up to date; "(has newer activity)" means review only
                     what came after that time (--since)

--mark writes the transcript's ledger line (its last timestamp, the current
version, the title), replacing any earlier line for it. Use it instead of
editing the ledger by hand.
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

# The review skill's own version: `version` is bumped on any change that could
# find something new; `reviews_ok_from` is the oldest version whose reviews
# still count (a minor change leaves it alone, a major one raises it).
skill_md = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "SKILL.md")
front = open(skill_md, encoding="utf-8").read().split("---")[1]
field = lambda k: int(re.search(rf"^{k}:\s*(\d+)", front, re.M).group(1))
version, ok_from = field("version"), field("reviews_ok_from")

reviewed = {}  # uuid -> (through, version); a line with no "by vN" is v1
if ledger_path and os.path.exists(ledger_path):
    for line in open(ledger_path, encoding="utf-8"):
        m = re.match(r"^- `([0-9a-f-]{36})` reviewed through (\S+?)(?: by v(\d+))?(?: |$)", line)
        if m:
            reviewed[m.group(1)] = (m.group(2), int(m.group(3) or 1))

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
        if "--mark" in args and path != os.path.expanduser(args[args.index("--mark") + 1]):
            continue
        if invocations:
            sid = name[:-6]
            rows.append((first, path, os.path.getsize(path), last, invocations, reviewed.get(sid)))

if "--mark" in args:
    i = args.index("--mark")
    if not ledger_path or len(rows) != 1 or len(args) < i + 3:
        sys.exit("error: --mark needs --ledger, the path of a transcript that invoked the skill, and a title")
    first, path, size, last, inv, rev = rows[0]
    sid = os.path.basename(path)[:-6]
    new = f"- `{sid}` reviewed through {last} by v{version} — {args[i + 2]}\n"
    lines = open(ledger_path, encoding="utf-8").read().splitlines(keepends=True)
    at = [n for n, l in enumerate(lines) if l.startswith(f"- `{sid}`")]
    if at:
        lines[at[0]] = new
    else:
        end = max(n for n, l in enumerate(lines) if re.match(r"^- `[0-9a-f-]{36}` reviewed through", l))
        lines.insert(end + 1, new)
    open(ledger_path, "w", encoding="utf-8").write("".join(lines))
    print(new, end="")
    sys.exit(0)

for first, path, size, last, inv, rev in sorted(rows):
    if not rev:
        status = "NOT REVIEWED"
    elif rev[1] < ok_from:
        status = f"STALE: reviewed by v{rev[1]}, v{ok_from} or later counts (now v{version}); review it again in full"
    else:
        status = f"reviewed through {rev[0]} by v{rev[1]}"
        if last and last > rev[0]:
            status += " (has newer activity)"
    print(f"{path}\n  {size/1e6:.1f} MB  {first} .. {last}  {status}")
    for ts, a in inv:
        print(f"  invoked {ts}: {a!r}")
