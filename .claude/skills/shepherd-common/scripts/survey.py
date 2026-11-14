#!/usr/bin/env python3
"""Survey the nodes in scope from the live database, read-only.

Usage:
  python3 -I survey.py <data-root> <item>... [--save FILE] [--compare FILE]

Each item is a ticket (ENG-2925), a node slug, or a node UUID. A node with
children means every node under it that has Lifecycle. An item that matches
nothing is an error.

One line per node: ticket, slug, lifecycle state and how long it has been
there, run state (phase, reason_kind, reason_text) and its age, pending
requests (with the first question), pending waits, the sandbox name, and the node's fingerprint in [brackets]
(shepherd_lib.fingerprint_parts says what goes into it).

--save FILE    also write the survey as JSON, with each node's fingerprint
               (take one right before a deploy).
--compare FILE list every node whose run state got worse since that survey
               (now stopped or failed, or a new pending request): after a
               deploy, each is a regression to fix before anything else.
"""
import json
import os
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))  # python -I drops it
import shepherd_lib as lib  # noqa: E402

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
db = lib.open_tod(root)


def age(ms):
    if not ms:
        return "?"
    m = int((time.time() * 1000 - ms) / 60000)
    return f"{m}m" if m < 120 else f"{m // 60}h" if m < 2880 else f"{m // 1440}d"


nodes = lib.subtree(db, [n for i in items for n in lib.resolve_or_die(db, i)])
if not nodes:
    sys.exit("error: no node in scope has Lifecycle")

survey = {}
for nid in nodes:
    snap = lib.snapshot(db, nid)
    fp, parts = lib.fingerprint(snap)
    slug, ticket, state, phase, kind = snap["slug"], snap["ticket"], snap["state"], snap["phase"], snap["kind"]
    survey[slug] = dict(ticket=ticket, state=state, phase=phase, kind=kind,
                        requests=len(snap["decisions"]), fingerprint=fp)
    line = f"{ticket:10} {slug[:34]:34} {state:9} {age(snap['state_at']):>4} | {phase} {kind} {age(snap['since'])} [{fp}]"
    if snap["text"]:
        line += f" — {snap['text'][:90]}"
    if snap["decisions"]:
        line += f"\n{'':16}{len(snap['decisions'])} request(s): {' '.join(snap['decisions'][0][1].split())[:110]}"
    for spec, _, due in snap["waits"]:
        line += f"\n{'':16}wait {spec} " + (f"overdue by {age(due)}" if due < time.time() * 1000 else "pending")
    if snap["sandbox"]:
        line += f"\n{'':16}sandbox {snap['sandbox']}"
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
