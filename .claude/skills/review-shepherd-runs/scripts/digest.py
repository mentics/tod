#!/usr/bin/env python3
"""Condense one Claude Code transcript into what a skill review needs.

Usage: python3 -I digest.py <transcript.jsonl> [--since <ISO>] [--tools] [--stats]

Default output, in transcript order:
  ## USER        what the user typed
  ## AGENT       a subagent's hand-back (model output, not the user)
  ## SUMMARY     a compaction summary (the context ran out)
  ## ASK         a question the agent put to the user
  >> SELF        agent prose that admits a mistake, corrects itself, or says
                 it skipped, guessed or didn't verify something
  -- A           the agent's other prose (status reports, decisions)
  [tool] ...     one line per tool call, only with --tools
  !! ERROR       a tool result that was an error or a refusal

--stats prints only a behavior profile instead: tool calls by tool, errors by
kind, commands run more than twice, the longest gaps between records, wall
time, compactions, subagents started, and how many SELF lines there were.
Subagent (sidechain) records and skill bodies are left out.
"""
import collections
import datetime
import json
import re
import sys

args = sys.argv[1:]
path = args[0]
since = args[args.index("--since") + 1] if "--since" in args else ""
show_tools = "--tools" in args
stats_only = "--stats" in args

SELF = re.compile(
    r"\b(I should have|I shouldn't have|I should not have|my mistake|I was wrong|"
    r"correction to|I missed|I skipped|I overlooked|I misread|I assumed|I guessed|"
    r"was a guess|I didn'?t (?:check|verify|re-?run|confirm|look)|I haven'?t (?:verified|checked|re-?run|confirmed)|"
    r"not (?:yet )?(?:independently )?verified|untested|didn'?t take|that was wrong|"
    r"turned out|actually,|on second look|I retried|retrying|again\b.*fail|"
    r"didn'?t work|went wrong|by mistake|wrong (?:row|node|click|place))",
    re.I,
)


def texts(content):
    if isinstance(content, str):
        return [content]
    return [x.get("text", "") for x in content or [] if isinstance(x, dict) and x.get("type") == "text"]


def ptime(ts):
    try:
        return datetime.datetime.fromisoformat(ts.replace("Z", "+00:00"))
    except ValueError:
        return None


tools = collections.Counter()
errors = collections.Counter()
commands = collections.Counter()
gaps = []
compactions = subagents = self_lines = 0
first = last = None
prev = None

out = []
emit = out.append

for line in open(path, encoding="utf-8", errors="replace"):
    try:
        o = json.loads(line)
    except json.JSONDecodeError:
        continue
    if o.get("isSidechain") or o.get("timestamp", "") < since:
        continue
    full_ts = o.get("timestamp", "")
    t = ptime(full_ts) if full_ts else None
    if t:
        first = first or t
        if prev and (t - prev).total_seconds() > 300:
            gaps.append(((t - prev).total_seconds(), prev.isoformat()[5:16]))
        prev = last = t
    ts = full_ts[5:16]
    msg = o.get("message") or {}
    content = msg.get("content")
    if o.get("type") == "user":
        for s in (x.strip() for x in texts(content)):
            if not s or s.startswith("Base directory for this skill"):
                continue
            if s.startswith("Another Claude session sent a message"):
                m = re.search(r'from="([^"]+)"', s)
                body = s.split("authority", 1)[-1] if "[Subagent hand-back]" in s else s
                kind = "hand-back" if "[Subagent hand-back]" in s else "agent message"
                emit(f"\n## AGENT {ts} ({kind} from {m.group(1) if m else '?'})\n{body.strip()[:1200]}")
            elif "<task-notification" in s:
                m = re.search(r"<summary>(.*?)</summary>", s, re.S)
                emit(f"   (notification {ts}: {(m.group(1) if m else '')[:150]})")
            elif s.startswith("This session is being continued"):
                compactions += 1
                emit(f"\n## SUMMARY {ts}\n{s[:4000]}")
            elif s.startswith("<system-reminder>") and s.endswith("</system-reminder>"):
                continue
            else:
                emit(f"\n## USER {ts}\n{s[:4000]}")
        for x in content if isinstance(content, list) else []:
            if isinstance(x, dict) and x.get("type") == "tool_result" and x.get("is_error"):
                body = x.get("content")
                body = body if isinstance(body, str) else " ".join(texts(body))
                body = re.sub(r"^.*vite-plus/env\n?", "", body, flags=re.M)
                key = "classifier refusal" if "auto mode classifier" in body else \
                      "user denied" if "doesn't want to proceed" in body or "denied" in body.lower() else \
                      re.sub(r"[0-9a-f]{7,}|\d+", "#", body.strip().splitlines()[-1] if body.strip() else "?")[:90]
                errors[key] += 1
                emit(f"!! ERROR {ts}: {body[:300]!r}")
    elif o.get("type") == "assistant":
        for x in content or []:
            if not isinstance(x, dict):
                continue
            if x.get("type") == "text" and x["text"].strip():
                tag = ">> SELF" if SELF.search(x["text"]) else "-- A"
                self_lines += tag == ">> SELF"
                emit(f"\n{tag} {ts}: {x['text'][:1500]}")
            elif x.get("type") == "tool_use":
                name, inp = x.get("name", ""), x.get("input", {})
                tools[name] += 1
                if name == "Agent":
                    subagents += 1
                if inp.get("command"):
                    commands[re.sub(r"\s+", " ", inp["command"])[:160]] += 1
                if name == "AskUserQuestion" or (name.endswith("wrap_up") and inp.get("ask")):
                    emit(f"\n## ASK {ts}: {json.dumps(inp)[:1200]}")
                elif show_tools:
                    s = inp.get("command") or inp.get("description") or inp.get("file_path") or json.dumps(inp)
                    emit(f"  [{name}] {str(s)[:200].replace(chr(10), ' ')}")

if not stats_only:
    print("\n".join(out))
    sys.exit(0)

wall = (last - first) if first and last else None
print(f"wall time: {wall}   compactions: {compactions}   subagents started: {subagents}   SELF lines: {self_lines}")
print("\ntool calls:", ", ".join(f"{k} {v}" for k, v in tools.most_common()))
print("\nerrors by kind:")
for k, v in errors.most_common(15):
    print(f"  {v:3}  {k}")
print("\ncommands run more than twice:")
for k, v in commands.most_common(25):
    if v > 2:
        print(f"  {v:3}  {k}")
print("\nlongest gaps between records (waiting or idle):")
for secs, at in sorted(gaps, reverse=True)[:10]:
    print(f"  {secs/60:6.1f} min  after {at}")
