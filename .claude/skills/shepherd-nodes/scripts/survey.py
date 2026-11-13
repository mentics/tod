#!/usr/bin/env python3
"""Survey the nodes in scope from the live database, read-only.

Usage:
  python3 -I survey.py <data-root> <item>... [--save FILE] [--compare FILE]

Each item is a ticket (ENG-2925), a node slug, or a node UUID. A node with
children means every node under it that has Lifecycle. An item that matches
nothing is an error.

One line per node: ticket, slug, lifecycle state and how long it has been
there, run state (phase, reason_kind, reason_text) and its age, pending
requests (with the first question), pending waits, and the sandbox name.

--save FILE    also write the survey as JSON (take one right before a deploy).
--compare FILE list every node whose run state got worse since that survey
               (now stopped or failed, or a new pending request): after a
               deploy, each is a regression to fix before anything else.
"""
import json
import sqlite3
import sys
import time
import uuid

args = sys.argv[1:]
if len(args) < 2:
    sys.exit(__doc__)


def opt(name):
    if name in args:
        i = args.index(name)
        val = args[i + 1]
        del args[i : i + 2]
        return val
    return None


save, compare = opt("--save"), opt("--compare")
root, items = args[0], args[1:]
db = sqlite3.connect(f"file:{root}/tod.db?mode=ro", uri=True)
q = lambda sql, *p: db.execute(sql, p).fetchall()


def resolve(item):
    try:
        return [uuid.UUID(item).bytes]
    except ValueError:
        pass
    rows = q("SELECT id FROM nodes WHERE lower(slug) = lower(?)", item)
    rows = rows or q("SELECT node_id FROM node_fields WHERE upper(ticket) LIKE '%' || upper(?)", item)
    if not rows:
        sys.exit(f"error: `{item}` matches no node (not a slug, UUID or ticket)")
    return [r[0] for r in rows]


def with_subtree(ids):
    out = []
    for nid in ids:
        out += [r[0] for r in q(
            """WITH RECURSIVE t(id) AS (SELECT ?
                 UNION SELECT e.node_id FROM outline_entries e JOIN t ON e.parent_id = t.id)
               SELECT t.id FROM t JOIN node_lifecycle l ON l.node_id = t.id""", nid)]
    return list(dict.fromkeys(out))


def age(ms):
    if not ms:
        return "?"
    m = int((time.time() * 1000 - ms) / 60000)
    return f"{m}m" if m < 120 else f"{m // 60}h" if m < 2880 else f"{m // 1440}d"


nodes = with_subtree([n for i in items for n in resolve(i)])
if not nodes:
    sys.exit("error: no node in scope has Lifecycle")

survey = {}
for nid in nodes:
    slug, = q("SELECT slug FROM nodes WHERE id = ?", nid)[0]
    ticket = (q("SELECT ticket FROM node_fields WHERE node_id = ?", nid) or [[None]])[0][0] or "-"
    ticket = ticket.rstrip("/").rsplit("/", 1)[-1] if "/" in ticket else ticket
    state, s_at = q("SELECT state, updated_at FROM node_lifecycle WHERE node_id = ?", nid)[0]
    run = q("SELECT phase, reason_kind, reason_text, since FROM run_state WHERE node_id = ?", nid)
    phase, kind, text, since = run[0] if run else ("none", "", "", None)
    reqs = q("SELECT question FROM decisions WHERE node_id = ? AND status = 'pending' ORDER BY created_at", nid)
    waits = q("SELECT match_spec, due_at FROM waits WHERE node_id = ? AND state = 'pending'", nid)
    sandbox = (q("SELECT sandbox FROM node_files_locations WHERE node_id = ?", nid) or [[None]])[0][0]
    survey[slug] = dict(ticket=ticket, state=state, phase=phase, kind=kind, requests=len(reqs))
    line = f"{ticket:10} {slug[:34]:34} {state:9} {age(s_at):>4} | {phase} {kind} {age(since)}"
    if text:
        line += f" — {text[:90]}"
    if reqs:
        line += f"\n{'':16}{len(reqs)} request(s): {' '.join(reqs[0][0].split())[:110]}"
    for spec, due in waits:
        line += f"\n{'':16}wait {spec} " + (f"overdue by {age(due)}" if due < time.time() * 1000 else "pending")
    if sandbox:
        line += f"\n{'':16}sandbox {sandbox}"
    print(line)

if save:
    json.dump(survey, open(save, "w"), indent=1)
if compare:
    before = json.load(open(compare))
    worse = [
        f"{s}: {b['phase']} {b['kind']} -> {n['phase']} {n['kind']}, requests {b['requests']} -> {n['requests']}"
        for s, n in survey.items()
        if (b := before.get(s))
        and ((n["phase"] == "stopped" and (b["phase"], b["kind"]) != ("stopped", n["kind"]))
             or n["requests"] > b["requests"])
    ]
    print("\nworse since", compare, ":" if worse else ": none")
    for w in worse:
        print("  " + w)
