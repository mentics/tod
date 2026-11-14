#!/usr/bin/env python3
"""The shepherds' ledger: scope, leases, verdicts, gaps and signals.

Usage:
  python3 -I ledger.py <data-root> [--ledger PATH] [--text] <command> ...

The ledger is `<data-root>/shepherd/ledger.db` (SQLite, WAL) and holds only
what is current. Whatever stops being current (a closed gap, a finished or
unscoped node, a shepherd that is gone, a superseded signal, every verdict as
it is recorded) is appended to `history.jsonl` next to it and removed from the
ledger. tod.db is only ever read, read-only. See ../ledger.md for every
command; `ledger.py <root> --help` lists them.

Exit codes: 0 ok; 1 error (message on stderr); 2 usage; 3 `check` refused;
10 `gate` says pause; 11 `gate` says retire.
"""
import argparse
import contextlib
import hashlib
import json
import os
import sqlite3
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))  # python -I drops it
import shepherd_lib as lib  # noqa: E402
from shepherd_lib import die  # noqa: E402

SCRIPT_DIR = os.path.dirname(os.path.abspath(__file__))
VERDICTS = ("ceiling", "parked", "in_motion", "blocked_on_gap")
GAP_STATUSES = ("open", "fixing", "verified", "deployed", "wontfix")
GAP_UNRESOLVED = ("open", "fixing", "verified")
GAP_CLOSED = ("deployed", "wontfix")
LIFECYCLE = ["proposed", "design", "planning", "ready", "active", "verifying", "review", "pr",
             "pr_draft", "pr_review", "approved", "merged", "released", "learn", "done"]
DEFAULT_LEASE_MINUTES = 20.0
STALE_OWNER_SECONDS = 30 * 60
RETIRED_OWNER_SECONDS = 10 * 60  # a retired shepherd may still record verdicts for a while
FP_PARTS_KEEP_SECONDS = 24 * 3600


def now():
    return time.time()


# --------------------------------------------------------------- shepherd version

def shepherd_version():
    """Hash of the shepherd skills and agents; a verdict under another version
    no longer counts. Found relative to this script."""
    claude = os.path.abspath(os.path.join(SCRIPT_DIR, "..", "..", ".."))
    files = []
    for sub in ("skills/shepherd-cli", "skills/shepherd-ui", "skills/shepherd-common"):
        base = os.path.join(claude, sub)
        for dirpath, dirnames, filenames in os.walk(base):
            dirnames[:] = [d for d in dirnames if d != "__pycache__"]
            files += [os.path.join(dirpath, f) for f in filenames]
    agents = os.path.join(claude, "agents", "shepherd")
    if os.path.isdir(agents):
        files += [os.path.join(agents, f) for f in os.listdir(agents) if f.endswith(".md")]
    h = hashlib.sha256()
    for path in sorted(files):
        h.update(os.path.relpath(path, claude).encode() + b"\0")
        with open(path, "rb") as f:
            h.update(f.read() + b"\0")
    return h.hexdigest()[:12]


# --------------------------------------------------------------- ledger db

SCHEMA = """
CREATE TABLE IF NOT EXISTS meta (k TEXT PRIMARY KEY, v TEXT);
CREATE TABLE IF NOT EXISTS scope (item TEXT NOT NULL, node_id TEXT NOT NULL, added_at REAL NOT NULL, PRIMARY KEY (item, node_id));
CREATE TABLE IF NOT EXISTS nodes (
  node_id TEXT PRIMARY KEY, slug TEXT, ticket TEXT, state TEXT,
  fingerprint TEXT, parts TEXT, info TEXT,
  in_scope INTEGER NOT NULL DEFAULT 1, finished INTEGER NOT NULL DEFAULT 0, first_seen REAL,
  lease_owner TEXT, lease_expires REAL, lease_at REAL, lease_fp TEXT,
  verdict TEXT, reason TEXT, verdict_fp TEXT, verdict_version TEXT, verdict_at REAL, verdict_owner TEXT,
  recheck_at REAL, gap_id INTEGER,
  force_due TEXT, due INTEGER NOT NULL DEFAULT 0, due_reason TEXT);
CREATE TABLE IF NOT EXISTS fp_parts (fp TEXT PRIMARY KEY, parts TEXT NOT NULL, at REAL);
CREATE TABLE IF NOT EXISTS holds (node_id TEXT PRIMARY KEY, slug TEXT, reason TEXT, at REAL);
CREATE TABLE IF NOT EXISTS owners (
  owner TEXT PRIMARY KEY, kind TEXT, last_heartbeat REAL, visits INTEGER NOT NULL DEFAULT 0,
  empty_visits INTEGER NOT NULL DEFAULT 0, ack TEXT NOT NULL DEFAULT 'running', ack_at REAL,
  first_seen REAL, first_signal_id INTEGER NOT NULL DEFAULT 0);
CREATE TABLE IF NOT EXISTS signals (id INTEGER PRIMARY KEY AUTOINCREMENT, owner TEXT NOT NULL, signal TEXT NOT NULL, at REAL NOT NULL);
CREATE TABLE IF NOT EXISTS gaps (
  id INTEGER PRIMARY KEY AUTOINCREMENT, signature TEXT NOT NULL, summary TEXT, status TEXT NOT NULL,
  commit_sha TEXT, note TEXT, created_at REAL, updated_at REAL, recurrence INTEGER NOT NULL DEFAULT 0);
CREATE TABLE IF NOT EXISTS gap_nodes (gap_id INTEGER NOT NULL, node_id TEXT NOT NULL, owner TEXT, at REAL, PRIMARY KEY (gap_id, node_id));
"""


ADDED_COLUMNS = [
    ("fp_parts", "at", "REAL"),
    ("owners", "first_seen", "REAL"),
    ("owners", "first_signal_id", "INTEGER NOT NULL DEFAULT 0"),
    ("gaps", "recurrence", "INTEGER NOT NULL DEFAULT 0"),
]


def open_ledger(path):
    try:
        os.makedirs(os.path.dirname(path), exist_ok=True)
        conn = sqlite3.connect(path, timeout=60, isolation_level=None)
        conn.execute("PRAGMA busy_timeout=60000")
        conn.execute("PRAGMA journal_mode=WAL")
        conn.row_factory = sqlite3.Row
        conn.execute("BEGIN IMMEDIATE")
        for stmt in SCHEMA.strip().split(";\n"):
            if stmt.strip():
                conn.execute(stmt)
        for table, column, decl in ADDED_COLUMNS:  # ledgers made before the column existed
            if column not in {r["name"] for r in conn.execute(f"PRAGMA table_info({table})")}:
                conn.execute(f"ALTER TABLE {table} ADD COLUMN {column} {decl}")
        conn.execute("COMMIT")
    except (sqlite3.Error, OSError) as e:
        die(f"cannot open the ledger {path}: {e}")
    return conn


@contextlib.contextmanager
def txn(conn):
    conn.execute("BEGIN IMMEDIATE")
    try:
        yield conn
    except BaseException:
        conn.execute("ROLLBACK")
        raise
    else:
        conn.execute("COMMIT")


# --------------------------------------------------------------- history

HISTORY_PATH = None  # set by main(): history.jsonl next to the ledger


def history_path():
    if HISTORY_PATH is None:
        die("the history file is not set (main() sets it next to the ledger)")
    return HISTORY_PATH


def to_history(event, **fields):
    """Append one record. Called inside the ledger's write transaction, so
    writers are serialised and a record goes out before its row is removed
    (a failed commit can at worst leave a duplicate record, never lose one)."""
    rec = {"at": now(), "event": event, **fields}
    try:
        with open(history_path(), "a", encoding="utf-8") as f:
            f.write(json.dumps(rec, default=str) + "\n")
    except OSError as e:
        die(f"cannot append to the history {history_path()}: {e}")


def read_history():
    path = history_path()
    if not os.path.exists(path):
        return []
    out = []
    with open(path, encoding="utf-8") as f:
        for n, line in enumerate(f, 1):
            if line.strip():
                try:
                    out.append(json.loads(line))
                except ValueError as e:
                    die(f"{path} line {n} is not JSON: {e}")
    return out


def gap_record(conn, g):
    nodes = conn.execute(
        "SELECT g.node_id, g.owner, g.at, n.slug, n.ticket FROM gap_nodes g LEFT JOIN nodes n ON n.node_id = g.node_id WHERE g.gap_id = ?",
        (g["id"],)).fetchall()
    return dict(id=g["id"], signature=g["signature"], summary=g["summary"], status=g["status"], commit=g["commit_sha"],
                note=g["note"], created_at=g["created_at"], updated_at=g["updated_at"], recurrence=g["recurrence"],
                nodes=[dict(node_id=n["node_id"], slug=n["slug"], ticket=n["ticket"], owner=n["owner"], at=n["at"]) for n in nodes])


def node_record(r):
    return dict(node_id=r["node_id"], slug=r["slug"], ticket=r["ticket"], state=r["state"],
                last_verdict=r["verdict"], reason=r["reason"], verdict_at=r["verdict_at"], verdict_owner=r["verdict_owner"],
                gap=r["gap_id"], first_seen=r["first_seen"])


def housekeep(conn):
    """Inside a transaction: move what is no longer current to the history."""
    t = now()
    for g in conn.execute(f"SELECT * FROM gaps WHERE status IN {GAP_CLOSED}").fetchall():
        to_history("gap_closed", **gap_record(conn, g))
        conn.execute("DELETE FROM gap_nodes WHERE gap_id = ?", (g["id"],))
        conn.execute("DELETE FROM gaps WHERE id = ?", (g["id"],))
    for r in conn.execute("SELECT * FROM nodes WHERE finished = 1 OR in_scope = 0").fetchall():
        to_history("node_done" if r["finished"] else "node_left_scope", **node_record(r))
        conn.execute("DELETE FROM nodes WHERE node_id = ?", (r["node_id"],))
    for r in conn.execute("SELECT * FROM owners").fetchall():
        if conn.execute("SELECT 1 FROM nodes WHERE lease_owner = ? AND lease_expires > ?", (r["owner"], t)).fetchone():
            continue
        age = t - (r["last_heartbeat"] or 0)
        how = "retired" if r["ack"] == "retired" and age > RETIRED_OWNER_SECONDS else (
            "went_silent" if age > STALE_OWNER_SECONDS else None)
        if how:
            to_history("owner_gone", owner=r["owner"], kind=r["kind"], how=how, visits=r["visits"],
                       empty_visits=r["empty_visits"], first_seen=r["first_seen"], last_heartbeat=r["last_heartbeat"])
            conn.execute("DELETE FROM signals WHERE owner = ?", (r["owner"],))
            conn.execute("DELETE FROM owners WHERE owner = ?", (r["owner"],))
    conn.execute(
        """DELETE FROM fp_parts WHERE COALESCE(at, 0) < ? AND fp NOT IN (
             SELECT fingerprint FROM nodes WHERE fingerprint IS NOT NULL UNION SELECT verdict_fp FROM nodes WHERE verdict_fp IS NOT NULL
             UNION SELECT lease_fp FROM nodes WHERE lease_fp IS NOT NULL)""", (t - FP_PARTS_KEEP_SECONDS,))


def remember_fp(conn, fp, parts):
    conn.execute("INSERT OR IGNORE INTO fp_parts(fp, parts, at) VALUES (?, ?, ?)", (fp, json.dumps(parts), now()))
    conn.execute("UPDATE fp_parts SET at = ? WHERE fp = ?", (now(), fp))


# --------------------------------------------------------------- sync

def scope_nodes(conn, tod, warnings):
    ids = []
    for r in conn.execute("SELECT item, node_id FROM scope ORDER BY added_at"):
        root = bytes.fromhex(r["node_id"])
        if not tod.execute("SELECT 1 FROM nodes WHERE id = ?", (root,)).fetchone():
            warnings.append(f"scope item `{r['item']}` no longer exists in tod.db")
            continue
        ids += lib.subtree(tod, [root])
    return list(dict.fromkeys(ids))


def base_rank(info):
    if info["phase"] == "stopped":
        return 0
    if info["requests"]:
        return 1
    if info["overdue_waits"]:
        return 2
    return None  # 3 or 4 by whether it has a verdict


def sync_read(conn, tod):
    """Read tod.db for every node in scope (no ledger lock held)."""
    warnings = []
    ids = scope_nodes(conn, tod, warnings)
    ms = now() * 1000
    snaps = []
    for nid in ids:
        s = lib.snapshot(tod, nid)
        fp, parts = lib.fingerprint(s)
        s["fp"], s["parts"] = fp, parts
        s["overdue_waits"] = [spec for spec, _, due in s["waits"] if due < ms]
        snaps.append(s)
    return snaps, warnings


def sync_apply(conn, snaps, warnings, version):
    """Inside a transaction: upsert nodes, finish done ones, recompute due."""
    t = now()
    seen = set()
    finished = 0
    for s in snaps:
        if s["state"] == "done":
            finished += 1
            # a done node is history: a row left from before is finished (housekeep moves it), none is made
            conn.execute("UPDATE nodes SET finished = 1, state = 'done' WHERE node_id = ?", (s["id"],))
            continue
        seen.add(s["id"])
        info = dict(phase=s["phase"], kind=s["kind"], text=s["text"], requests=len(s["decisions"]),
                    first_question=(s["decisions"][0][1] if s["decisions"] else None),
                    overdue_waits=s["overdue_waits"], waits=[w[0] for w in s["waits"]],
                    sandbox=s["sandbox"], state_at=s["state_at"], since=s["since"], title=s["title"])
        remember_fp(conn, s["fp"], s["parts"])
        conn.execute("INSERT OR IGNORE INTO nodes(node_id, slug, first_seen) VALUES (?, ?, ?)", (s["id"], s["slug"], t))
        conn.execute(
            "UPDATE nodes SET slug=?, ticket=?, state=?, fingerprint=?, parts=?, info=?, in_scope=1, finished=0 WHERE node_id=?",
            (s["slug"], s["ticket"], s["state"], s["fp"], json.dumps(s["parts"]), json.dumps(info), s["id"]))
    for r in conn.execute("SELECT node_id FROM nodes WHERE in_scope = 1 AND finished = 0").fetchall():
        if r["node_id"] not in seen:
            conn.execute("UPDATE nodes SET in_scope = 0, due = 0, due_reason = NULL WHERE node_id = ?", (r["node_id"],))
    conn.execute("INSERT OR REPLACE INTO meta(k, v) VALUES ('finished_in_scope', ?)", (str(finished),))
    housekeep(conn)
    for r in conn.execute("SELECT * FROM nodes WHERE in_scope = 1").fetchall():
        reason = None
        if not r["finished"]:
            if r["verdict"] is None:
                reason = "no verdict yet"
            elif r["verdict_fp"] != r["fingerprint"]:
                reason = "fingerprint changed since the verdict"
            elif r["verdict_version"] != version:
                reason = "shepherd version changed since the verdict"
            elif r["recheck_at"] is not None and r["recheck_at"] <= t:
                reason = "recheck time reached"
            elif r["force_due"]:
                reason = r["force_due"]
        conn.execute("UPDATE nodes SET due = ?, due_reason = ? WHERE node_id = ?",
                     (1 if reason else 0, reason, r["node_id"]))
    return warnings


def do_sync(conn, tod):
    snaps, warnings = sync_read(conn, tod)
    with txn(conn):
        sync_apply(conn, snaps, warnings, shepherd_version())
        conn.execute("INSERT OR REPLACE INTO meta(k, v) VALUES ('synced_at', ?)", (str(now()),))
        counts = summary(conn)
    counts["warnings"] = warnings
    return counts


def is_leased(r, t):
    return r["lease_owner"] is not None and (r["lease_expires"] or 0) > t


def summary(conn):
    t = now()
    rows = conn.execute("SELECT * FROM nodes WHERE in_scope = 1 AND finished = 0").fetchall()
    holds = {r["node_id"] for r in conn.execute("SELECT node_id FROM holds")}
    active = rows
    fin = conn.execute("SELECT v FROM meta WHERE k = 'finished_in_scope'").fetchone()
    by = {}
    for r in active:
        by[r["verdict"] or "none"] = by.get(r["verdict"] or "none", 0) + 1
    return {
        "in_scope": len(rows) + (int(fin["v"]) if fin else 0),
        "due": sum(1 for r in active if r["due"] and r["node_id"] not in holds),
        "leased": sum(1 for r in active if is_leased(r, t)),
        "by_verdict": by,
        "held": sum(1 for r in rows if r["node_id"] in holds),
        "finished": int(fin["v"]) if fin else 0,
    }


# --------------------------------------------------------------- owners & signals

def touch_owner(conn, owner, kind=None):
    last = conn.execute("SELECT COALESCE(MAX(id), 0) FROM signals").fetchone()[0]
    conn.execute("INSERT OR IGNORE INTO owners(owner, kind, last_heartbeat, first_seen, first_signal_id) VALUES (?, ?, ?, ?, ?)",
                 (owner, kind, now(), now(), last))
    conn.execute("UPDATE owners SET last_heartbeat = ?, kind = COALESCE(?, kind) WHERE owner = ?", (now(), kind, owner))


def effective_signal(conn, owner):
    """(id, signal) of the latest signal addressed to the owner or to all; (0, None) if none.
    A `retire` sent to all before the owner existed is not for it: the manager
    retires every running shepherd and then starts fresh ones. A `pause` to all
    does reach late owners (a deploy is under way)."""
    r = conn.execute("SELECT id, owner, signal FROM signals WHERE owner IN (?, '*') ORDER BY id DESC LIMIT 1", (owner,)).fetchone()
    if not r:
        return (0, None)
    if r["owner"] == "*" and r["signal"] == "retire":
        o = conn.execute("SELECT first_signal_id FROM owners WHERE owner = ?", (owner,)).fetchone()
        if o is None or r["id"] <= o["first_signal_id"]:
            return (r["id"], None)
    return (r["id"], r["signal"])


def set_ack(conn, owner, ack):
    conn.execute("UPDATE owners SET ack = ?, ack_at = ? WHERE owner = ? AND ack != ?", (ack, now(), owner, ack))


def ack_for(signal):
    return {"pause": "paused", "retire": "retired"}.get(signal, "running")


def renew_leases(conn, owner, minutes):
    cur = conn.execute("UPDATE nodes SET lease_expires = ? WHERE lease_owner = ?", (now() + minutes * 60, owner))
    return cur.rowcount


# --------------------------------------------------------------- node views

def node_view(r, extra=None):
    info = json.loads(r["info"] or "{}")
    last = None
    if r["verdict"]:
        last = dict(verdict=r["verdict"], reason=r["reason"], at=r["verdict_at"], owner=r["verdict_owner"],
                    gap=r["gap_id"], recheck_at=r["recheck_at"])
    out = dict(
        ticket=r["ticket"], slug=r["slug"], node_id=r["node_id"], state=r["state"],
        run=dict(phase=info.get("phase"), reason_kind=info.get("kind"), reason_text=info.get("text")),
        requests=info.get("requests", 0), first_question=info.get("first_question"),
        waits=info.get("waits", []), sandbox=info.get("sandbox"),
        fingerprint=r["fingerprint"], due_reason=r["due_reason"], last_verdict=last)
    if extra:
        out.update(extra)
    return out


def lookup_node(conn, tod, ref):
    """(hex id, ledger row or None) for a ticket, slug or UUID."""
    nid = lib.hexid(lib.resolve_one_or_die(tod, ref))
    return nid, conn.execute("SELECT * FROM nodes WHERE node_id = ?", (nid,)).fetchone()


def live_fp(tod, hexid):
    s = lib.snapshot(tod, bytes.fromhex(hexid))
    fp, parts = lib.fingerprint(s)
    return fp, parts


# --------------------------------------------------------------- commands

def cmd_scope(a, conn, tod):
    if a.action == "list":
        rows = [dict(item=r["item"], node_id=r["node_id"]) for r in conn.execute("SELECT * FROM scope ORDER BY added_at, item")]
        return {"scope": rows}
    if not a.items:
        die(f"scope {a.action} needs at least one item")
    if a.action == "add":
        resolved = []
        for item in a.items:
            nids = lib.resolve_or_die(tod, item)  # a ticket may name several nodes
            if not lib.subtree(tod, nids):
                die(f"`{item}` and everything under it have no Lifecycle: nothing to shepherd")
            resolved += [(item, lib.hexid(n)) for n in nids]
        with txn(conn):
            for item, nid in resolved:
                conn.execute("INSERT OR REPLACE INTO scope(item, node_id, added_at) VALUES (?, ?, ?)", (item, nid, now()))
        return {"added": list(dict.fromkeys(i for i, _ in resolved))}
    ids = []
    for item in a.items:
        found = [r["node_id"] for r in conn.execute("SELECT node_id FROM scope WHERE item = ?", (item,))]
        if not found:
            found = [lib.hexid(n) for n in lib.resolve(tod, item)
                     if conn.execute("SELECT 1 FROM scope WHERE node_id = ?", (lib.hexid(n),)).fetchone()]
        if not found:
            die(f"`{item}` is not in the scope")
        ids += found
    with txn(conn):
        for nid in ids:
            conn.execute("DELETE FROM scope WHERE node_id = ?", (nid,))
    return {"removed": a.items}


def cmd_sync(a, conn, tod):
    return do_sync(conn, tod)


def priority_key(r):
    info = json.loads(r["info"] or "{}")
    rank = base_rank(info)
    if rank is None:
        rank = 3 if r["verdict"] else 4
    state = LIFECYCLE.index(r["state"]) if r["state"] in LIFECYCLE else len(LIFECYCLE)
    return (rank, state, info.get("since") or 0, r["slug"])


def cmd_next(a, conn, tod):
    snaps, warnings = sync_read(conn, tod)
    version = shepherd_version()
    t = now()
    with txn(conn):
        sync_apply(conn, snaps, warnings, version)
        touch_owner(conn, a.owner, a.kind)
        held = {r["node_id"] for r in conn.execute("SELECT node_id FROM holds")}
        mine = conn.execute("SELECT * FROM nodes WHERE lease_owner = ? AND in_scope = 1 AND finished = 0", (a.owner,)).fetchall()
        room = max(0, a.n - len(mine))
        cands = [r for r in conn.execute("SELECT * FROM nodes WHERE in_scope = 1 AND finished = 0 AND due = 1").fetchall()
                 if r["node_id"] not in held and not is_leased(r, t)]
        cands.sort(key=priority_key)
        taken = cands[:room]
        for r in taken:
            conn.execute("UPDATE nodes SET lease_owner=?, lease_expires=?, lease_at=?, lease_fp=? WHERE node_id=?",
                         (a.owner, t + a.lease_minutes * 60, t, r["fingerprint"], r["node_id"]))
        new_ids = {r["node_id"] for r in taken}
        out = []
        for r in sorted(mine, key=priority_key) + taken:  # already held first, then new in priority order
            r = conn.execute("SELECT * FROM nodes WHERE node_id = ?", (r["node_id"],)).fetchone()
            out.append(node_view(r, {"new": r["node_id"] in new_ids}))
        counts = summary(conn)
    return {"owner": a.owner, "leased": out, "due_remaining": counts["due"] - len(taken), "warnings": warnings}


def cmd_verdict(a, conn, tod):
    if a.verdict == "in_motion" and a.recheck_minutes is None:
        die("--verdict in_motion requires --recheck-minutes")
    if a.verdict == "blocked_on_gap" and a.gap is None:
        die("--verdict blocked_on_gap requires --gap")
    if a.acted and a.no_action:
        die("--acted and --no-action are exclusive")
    nid, row = lookup_node(conn, tod, a.node)
    if row is None:
        die(f"node `{a.node}` is not in the ledger (scope it and run `sync`)")
    fp, parts = live_fp(tod, nid)
    version = shepherd_version()
    t = now()
    with txn(conn):
        row = conn.execute("SELECT * FROM nodes WHERE node_id = ?", (nid,)).fetchone()
        if is_leased(row, t) and row["lease_owner"] != a.owner:
            die(f"node `{a.node}` is leased by {row['lease_owner']}, not {a.owner}")
        if a.gap is not None and not conn.execute("SELECT 1 FROM gaps WHERE id = ?", (a.gap,)).fetchone():
            die(f"gap {a.gap} does not exist")
        remember_fp(conn, fp, parts)
        conn.execute(
            """UPDATE nodes SET verdict=?, reason=?, verdict_fp=?, verdict_version=?, verdict_at=?, verdict_owner=?,
                 recheck_at=?, gap_id=?, force_due=NULL, due=0, due_reason=NULL, fingerprint=?, parts=?,
                 lease_owner=NULL, lease_expires=NULL, lease_at=NULL, lease_fp=NULL WHERE node_id=?""",
            (a.verdict, a.reason, fp, version, t, a.owner,
             t + a.recheck_minutes * 60 if a.recheck_minutes is not None else None, a.gap, fp, json.dumps(parts), nid))
        touch_owner(conn, a.owner)
        conn.execute("UPDATE owners SET visits = visits + 1, empty_visits = empty_visits + ? WHERE owner = ?",
                     (1 if a.no_action else 0, a.owner))
        to_history("verdict", node_id=nid, slug=row["slug"], ticket=row["ticket"], state=row["state"], verdict=a.verdict,
                   reason=a.reason, owner=a.owner, gap=a.gap, acted=bool(a.acted), no_action=bool(a.no_action),
                   fingerprint=fp, version=version, recheck_minutes=a.recheck_minutes)
    return {"node": row["slug"], "verdict": a.verdict, "fingerprint": fp, "version": version,
            "recheck_at": t + a.recheck_minutes * 60 if a.recheck_minutes is not None else None}


def cmd_check(a, conn, tod):
    nid, row = lookup_node(conn, tod, a.node)
    fp, parts = live_fp(tod, nid)
    held = row is not None and row["lease_owner"] == a.owner
    if fp == a.fingerprint and held:
        return {"ok": True, "fingerprint": fp}
    changed = []
    if fp != a.fingerprint:
        old = conn.execute("SELECT parts FROM fp_parts WHERE fp = ?", (a.fingerprint,)).fetchone()
        changed = lib.diff_parts(json.loads(old["parts"]), parts) if old else ["(the given fingerprint is not one the ledger saw)"]
    out = {"ok": False, "reason": "fingerprint_changed" if fp != a.fingerprint else "lease_not_held",
           "fingerprint": fp, "changed": changed, "lease_owner": row["lease_owner"] if row else None}
    if not held:
        out["reason"] = "lease_not_held" if fp == a.fingerprint else "fingerprint_changed_and_lease_not_held"
    sys.stdout.write(emit(out, a.text) + "\n")
    sys.stderr.write(f"check refused: {out['reason']}\n")
    sys.exit(3)


def cmd_ack(a, conn, tod):
    """Rebaseline held nodes: the shepherd has looked at the current state."""
    out = []
    with txn(conn):
        for ref in a.nodes:
            nid, row = lookup_node(conn, tod, ref)
            if row is None or row["lease_owner"] != a.owner:
                die(f"node `{ref}` is not leased by {a.owner}")
            fp, parts = live_fp(tod, nid)
            remember_fp(conn, fp, parts)
            conn.execute("UPDATE nodes SET lease_fp = ?, lease_at = ? WHERE node_id = ?", (fp, now(), nid))
            out.append({"node": row["slug"], "fingerprint": fp})
    return {"acked": out}


def cmd_renew(a, conn, tod):
    with txn(conn):
        touch_owner(conn, a.owner)
        n = renew_leases(conn, a.owner, a.lease_minutes)
    return {"owner": a.owner, "renewed": n}


def cmd_release(a, conn, tod):
    released, skipped = [], []
    with txn(conn):
        for ref in a.nodes:
            nid, row = lookup_node(conn, tod, ref)
            if row is None or row["lease_owner"] is None:
                skipped.append(ref)
                continue
            if row["lease_owner"] != a.owner:
                die(f"node `{ref}` is leased by {row['lease_owner']}, not {a.owner}")
            conn.execute("UPDATE nodes SET lease_owner=NULL, lease_expires=NULL, lease_at=NULL, lease_fp=NULL WHERE node_id=?", (nid,))
            released.append(ref)
    return {"released": released, "not_leased": skipped}


def cmd_gate(a, conn, tod):
    with txn(conn):
        touch_owner(conn, a.owner)
        renew_leases(conn, a.owner, a.lease_minutes)
        _, sig = effective_signal(conn, a.owner)
        set_ack(conn, a.owner, ack_for(sig))
    if sig == "pause":
        print("pause")
        sys.exit(10)
    if sig == "retire":
        print("retire")
        sys.exit(11)
    print("ok")
    return None


def cmd_wait(a, conn, tod):
    deadline = time.monotonic() + a.max_seconds  # real time, whatever now() says
    with txn(conn):
        touch_owner(conn, a.owner)
        sig0 = effective_signal(conn, a.owner)
        set_ack(conn, a.owner, ack_for(sig0[1]))
    if sig0[1] == "retire":
        return {"reason": "signal", "signal": "retire"}
    while True:
        with txn(conn):
            touch_owner(conn, a.owner)
            renew_leases(conn, a.owner, a.lease_minutes)
            sig = effective_signal(conn, a.owner)
            set_ack(conn, a.owner, ack_for(sig[1]))
        if sig[0] != sig0[0]:
            return {"reason": "signal", "signal": sig[1]}
        if sig[1] != "pause":
            counts_before = do_sync(conn, tod)
            t = now()
            mine = conn.execute("SELECT * FROM nodes WHERE lease_owner = ? AND in_scope = 1", (a.owner,)).fetchall()
            changed = [r for r in mine if r["fingerprint"] != r["lease_fp"]]
            if changed:
                return {"reason": "held_changed", "nodes": [node_view(r) for r in changed]}
            rech = [r for r in mine if r["recheck_at"] and r["lease_at"] and r["lease_at"] < r["recheck_at"] <= t]
            if rech:
                return {"reason": "held_recheck", "nodes": [node_view(r) for r in rech]}
            if len(mine) < a.batch:
                held = {r["node_id"] for r in conn.execute("SELECT node_id FROM holds")}
                avail = [r for r in conn.execute("SELECT * FROM nodes WHERE in_scope=1 AND finished=0 AND due=1").fetchall()
                         if r["node_id"] not in held and not is_leased(r, t)]
                if avail:
                    avail.sort(key=priority_key)
                    return {"reason": "due_available", "count": len(avail), "nodes": [node_view(r) for r in avail[:a.batch]]}
        if time.monotonic() >= deadline:
            return {"reason": "timeout", "paused": sig[1] == "pause"}
        time.sleep(max(0.0, min(a.poll_seconds, deadline - time.monotonic())))


def running_owners(conn, t, stale_seconds):
    """Owners that have not retired and whose heartbeat is fresh."""
    return [r for r in conn.execute("SELECT * FROM owners").fetchall()
            if r["ack"] != "retired" and t - (r["last_heartbeat"] or 0) <= stale_seconds]


def cmd_manager_wait(a, conn, tod):
    """The manager's wait: block until something needs it, then say what.
    Run in the background; one at a time."""
    deadline = time.monotonic() + a.max_seconds
    t0 = now()
    gap0 = conn.execute("SELECT COALESCE(MAX(id), 0) FROM gaps").fetchone()[0]
    stale0 = {r["owner"] for r in conn.execute("SELECT owner, ack, last_heartbeat FROM owners").fetchall()
              if r["ack"] != "retired" and t0 - (r["last_heartbeat"] or 0) > a.stale_seconds}
    unclaimed_since = {}  # node_id -> monotonic time first seen due, unheld and unleased
    while True:
        counts = do_sync(conn, tod)
        t = now()
        if counts["in_scope"] - counts["finished"] - counts["held"] <= 0:
            return {"reason": "all_finished", **counts}
        new = conn.execute("SELECT * FROM gaps WHERE id > ? ORDER BY id", (gap0,)).fetchall()
        if new:
            return {"reason": "new_gap", "gaps": [gap_view(conn, g) for g in new]}
        stale = [r["owner"] for r in conn.execute("SELECT owner, ack, last_heartbeat FROM owners").fetchall()
                 if r["ack"] != "retired" and t - (r["last_heartbeat"] or 0) > a.stale_seconds and r["owner"] not in stale0]
        if stale:
            return {"reason": "owner_stale", "owners": stale}
        with txn(conn):
            sig = conn.execute("SELECT signal FROM signals WHERE owner = '*' ORDER BY id DESC LIMIT 1").fetchone()
        if sig and sig["signal"] == "pause":
            st = cmd_signal(argparse.Namespace(action="status"), conn, tod)
            if st["all_paused"] or not [o for o in st["owners"] if not o["stale"] and o["ack"] != "retired"]:
                return {"reason": "all_paused", "owners": st["owners"]}
        else:
            held = {r["node_id"] for r in conn.execute("SELECT node_id FROM holds")}
            avail = [r for r in conn.execute("SELECT * FROM nodes WHERE in_scope=1 AND finished=0 AND due=1").fetchall()
                     if r["node_id"] not in held and not is_leased(r, t)]
            m = time.monotonic()
            unclaimed_since = {r["node_id"]: unclaimed_since.get(r["node_id"], m) for r in avail}
            # running shepherds with room claim due nodes within seconds; what is still unclaimed after
            # the grace period is the manager's to dispatch, if it may start another shepherd
            settled = [r for r in avail if m - unclaimed_since[r["node_id"]] >= a.grace_seconds]
            oldest = max((m - unclaimed_since[r["node_id"]] for r in avail), default=0)
            if settled and len(running_owners(conn, t, a.stale_seconds)) < a.max_parallel and (
                    len(settled) >= a.min_batch or oldest >= a.queue_delay_minutes * 60):
                settled.sort(key=priority_key)
                return {"reason": "unclaimed_due", "count": len(settled), "nodes": [node_view(r) for r in settled]}
        if time.monotonic() >= deadline:
            return {"reason": "timeout", **counts}
        time.sleep(max(0.0, min(a.poll_seconds, deadline - time.monotonic())))


def cmd_signal(a, conn, tod):
    if a.action == "status":
        with txn(conn):
            rows = conn.execute("SELECT * FROM owners ORDER BY owner").fetchall()
            t = now()
            out, pending = [], []
            for r in rows:
                _, sig = effective_signal(conn, r["owner"])
                stale = t - (r["last_heartbeat"] or 0) > STALE_OWNER_SECONDS and not conn.execute(
                    "SELECT 1 FROM nodes WHERE lease_owner = ? AND lease_expires > ?", (r["owner"], t)).fetchone()
                out.append(dict(owner=r["owner"], kind=r["kind"], signal=sig, ack=r["ack"], stale=stale,
                                heartbeat_age_s=round(t - (r["last_heartbeat"] or 0))))
                if sig == "pause" and r["ack"] != "paused" and not stale and r["ack"] != "retired":
                    pending.append(r["owner"])
            active = [o for o in out if not o["stale"] and o["ack"] != "retired"]
        return {"owners": out, "all_paused": bool(active) and all(o["ack"] == "paused" for o in active),
                "awaiting_pause": pending}
    if bool(a.owner) == bool(a.all):
        die("signal needs exactly one of --owner <id> or --all")
    to = "*" if a.all else a.owner
    with txn(conn):
        cur = conn.execute("INSERT INTO signals(owner, signal, at) VALUES (?, ?, ?)", (to, a.action, now()))
        # only the latest signal per addressee counts; one to all supersedes every older one
        if a.all:
            conn.execute("DELETE FROM signals WHERE id < ?", (cur.lastrowid,))
        else:
            conn.execute("DELETE FROM signals WHERE owner = ? AND id < ?", (to, cur.lastrowid))
        to_history("signal", to=to, signal=a.action)
    return {"signal": a.action, "to": "all" if a.all else a.owner}


def gap_view(conn, g, full=False):
    nodes = conn.execute(
        "SELECT g.node_id, g.owner, n.slug, n.ticket FROM gap_nodes g LEFT JOIN nodes n ON n.node_id = g.node_id WHERE g.gap_id = ?",
        (g["id"],)).fetchall()
    out = dict(id=g["id"], signature=g["signature"], summary=g["summary"], status=g["status"], commit=g["commit_sha"],
               note=g["note"], nodes=[n["slug"] or n["node_id"] for n in nodes], created_at=g["created_at"], updated_at=g["updated_at"],
               recurrence=g["recurrence"])
    if full:
        out["node_details"] = [dict(node_id=n["node_id"], slug=n["slug"], ticket=n["ticket"], owner=n["owner"]) for n in nodes]
    return out


def cmd_gap(a, conn, tod):
    if a.action == "list":
        return {"gaps": [gap_view(conn, g) for g in conn.execute("SELECT * FROM gaps ORDER BY id")]}
    if a.action == "show":
        g = conn.execute("SELECT * FROM gaps WHERE id = ?", (a.id,)).fetchone()
        if g:
            return gap_view(conn, g, full=True)
        closed = [h for h in read_history() if h.get("event") == "gap_closed" and h.get("id") == a.id]
        if not closed:
            die(f"gap {a.id} does not exist")
        return dict(closed[-1], closed=True)
    t = now()
    if a.action == "report":
        nid = lookup_node(conn, tod, a.node)[0] if a.node else None
        earlier = [dict(id=h["id"], status=h["status"], commit=h.get("commit"), closed_at=h["at"])
                   for h in read_history() if h.get("event") == "gap_closed" and h.get("signature") == a.signature]
        with txn(conn):
            g = conn.execute("SELECT * FROM gaps WHERE signature = ? AND status IN ('open','fixing','verified') ORDER BY id LIMIT 1",
                             (a.signature,)).fetchone()
            created = g is None
            if created:
                # ids keep counting past closed gaps, so a history record's id is never reused
                # (also when the ledger was deleted and the history kept)
                last = max([h["id"] for h in read_history() if h.get("event") == "gap_closed"] + [0])
                seq = conn.execute("SELECT seq FROM sqlite_sequence WHERE name = 'gaps'").fetchone() if conn.execute(
                    "SELECT 1 FROM sqlite_master WHERE name = 'sqlite_sequence'").fetchone() else None
                gid = max(last, seq[0] if seq else 0, conn.execute("SELECT COALESCE(MAX(id), 0) FROM gaps").fetchone()[0]) + 1
                conn.execute(
                    "INSERT INTO gaps(id, signature, summary, status, created_at, updated_at, recurrence) VALUES (?, ?, ?, 'open', ?, ?, ?)",
                    (gid, a.signature, a.summary, t, t, len(earlier)))
            else:
                gid = g["id"]
            if nid:
                conn.execute("INSERT OR IGNORE INTO gap_nodes(gap_id, node_id, owner, at) VALUES (?, ?, ?, ?)", (gid, nid, a.owner, t))
            g = conn.execute("SELECT * FROM gaps WHERE id = ?", (gid,)).fetchone()
            out = gap_view(conn, g)
        out["created"] = created
        if created and earlier:
            out["closed_before"] = earlier  # the same signature was fixed (or dropped) before: it came back
        return out
    # set
    with txn(conn):
        g = conn.execute("SELECT * FROM gaps WHERE id = ?", (a.id,)).fetchone()
        if not g:
            die(f"gap {a.id} does not exist")
        conn.execute("UPDATE gaps SET status = ?, commit_sha = COALESCE(?, commit_sha), note = COALESCE(?, note), updated_at = ? WHERE id = ?",
                     (a.status, a.commit, a.note, t, a.id))
        woken = 0
        if a.status == "deployed":
            woken = conn.execute(
                "UPDATE nodes SET force_due = ?, due = 1, due_reason = ? WHERE verdict = 'blocked_on_gap' AND gap_id = ? AND finished = 0",
                (f"fix for gap {a.id} deployed", f"fix for gap {a.id} deployed", a.id)).rowcount
        g = conn.execute("SELECT * FROM gaps WHERE id = ?", (a.id,)).fetchone()
        out = gap_view(conn, g)
        if a.status in GAP_CLOSED:
            housekeep(conn)  # a closed gap is history now
            out["moved_to_history"] = True
    out["nodes_made_due"] = woken
    return out


def cmd_hold(a, conn, tod):
    with txn(conn):
        for ref in a.nodes:
            nid, row = lookup_node(conn, tod, ref)
            if a.action == "hold":
                slug = tod.execute("SELECT slug FROM nodes WHERE id = ?", (bytes.fromhex(nid),)).fetchone()[0]
                conn.execute("INSERT OR REPLACE INTO holds(node_id, slug, reason, at) VALUES (?, ?, ?, ?)", (nid, slug, a.reason, now()))
            else:
                if not conn.execute("DELETE FROM holds WHERE node_id = ?", (nid,)).rowcount:
                    die(f"node `{ref}` is not held")
    return {("held" if a.action == "hold" else "unheld"): a.nodes}


def cmd_clear(a, conn, tod):
    chosen = [bool(a.all), bool(a.node), bool(a.verdict), bool(a.reason_like)]
    if sum(chosen) != 1:
        die("clear needs exactly one of --all, --node, --verdict, --reason-like")
    where, params = "verdict IS NOT NULL", []
    if a.node:
        nid, _ = lookup_node(conn, tod, a.node)
        where, params = "node_id = ? AND verdict IS NOT NULL", [nid]
    elif a.verdict:
        if a.verdict not in VERDICTS:
            die(f"unknown verdict `{a.verdict}`")
        where, params = "verdict = ?", [a.verdict]
    elif a.reason_like:
        where, params = "reason LIKE ?", [f"%{a.reason_like}%"]
    with txn(conn):
        n = conn.execute(
            f"""UPDATE nodes SET due = CASE WHEN in_scope = 1 AND finished = 0 THEN 1 ELSE 0 END,
                  due_reason = CASE WHEN in_scope = 1 AND finished = 0 THEN 'verdict cleared' END,
                  verdict=NULL, reason=NULL, verdict_fp=NULL, verdict_version=NULL, verdict_at=NULL, verdict_owner=NULL,
                  recheck_at=NULL, gap_id=NULL, force_due=NULL WHERE {where}""", params).rowcount
    return {"cleared": n}


def cmd_status(a, conn, tod):
    out = summary(conn)
    out["open_gaps"] = conn.execute("SELECT COUNT(*) FROM gaps").fetchone()[0]
    out["owners"] = [r["owner"] for r in conn.execute("SELECT owner FROM owners ORDER BY owner")]
    sa = conn.execute("SELECT v FROM meta WHERE k = 'synced_at'").fetchone()
    out["synced_age_s"] = round(now() - float(sa["v"])) if sa else None
    out["version"] = shepherd_version()
    return out


def cmd_owners(a, conn, tod):
    t = now()
    out = []
    for r in conn.execute("SELECT * FROM owners ORDER BY owner").fetchall():
        held = [n["slug"] for n in conn.execute(
            "SELECT slug FROM nodes WHERE lease_owner = ? AND lease_expires > ? ORDER BY slug", (r["owner"], t))]
        _, sig = effective_signal(conn, r["owner"])
        out.append(dict(owner=r["owner"], kind=r["kind"], heartbeat_age_s=round(t - (r["last_heartbeat"] or 0)),
                        held=held, visits=r["visits"], empty_visits=r["empty_visits"], signal=sig, ack=r["ack"]))
    return {"owners": out}


def cmd_history(a, conn, tod):
    recs = read_history()
    if a.event:
        recs = [r for r in recs if r.get("event") == a.event]
    if a.signature:
        recs = [r for r in recs if r.get("signature") == a.signature]
    if a.node:
        recs = [r for r in recs if a.node in (r.get("slug"), r.get("node_id")) or a.node in (r.get("ticket") or "")]
    if a.owner:
        recs = [r for r in recs if r.get("owner") == a.owner]
    if a.since_hours is not None:
        recs = [r for r in recs if r.get("at", 0) >= now() - a.since_hours * 3600]
    return {"history": recs[-a.limit:] if a.limit else recs}


# --------------------------------------------------------------- output

def emit(obj, text):
    if not text:
        return json.dumps(obj, default=str)
    return render(obj)


def render(obj, indent=0):
    pad = "  " * indent
    if isinstance(obj, dict):
        lines = []
        for k, v in obj.items():
            if isinstance(v, (dict, list)) and v:
                lines.append(f"{pad}{k}:")
                lines.append(render(v, indent + 1))
            else:
                lines.append(f"{pad}{k}: {json.dumps(v, default=str) if isinstance(v, (dict, list)) else v}")
        return "\n".join(lines)
    if isinstance(obj, list):
        lines = []
        for v in obj:
            if isinstance(v, dict):
                body = render(v, indent + 1).split("\n")
                lines.append(f"{pad}- " + body[0].lstrip())
                lines += body[1:]
            else:
                lines.append(f"{pad}- {v}")
        return "\n".join(lines)
    return f"{pad}{obj}"


# --------------------------------------------------------------- argparse

def build_parser():
    p = argparse.ArgumentParser(prog="ledger.py", description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("root", help="the tod data root (tod.db is read-only)")
    p.add_argument("--ledger", help="ledger file (default <root>/shepherd/ledger.db)")
    p.add_argument("--text", action="store_true", help="human-readable output instead of JSON")
    sub = p.add_subparsers(dest="command", required=True)

    def add(name, fn, **kw):
        sp = sub.add_parser(name, **kw)
        sp.set_defaults(fn=fn)
        return sp

    def owner(sp, required=True):
        sp.add_argument("--owner", required=required)

    sp = add("scope", cmd_scope)
    sp.add_argument("action", choices=["add", "remove", "list"])
    sp.add_argument("items", nargs="*")
    add("sync", cmd_sync)
    sp = add("next", cmd_next)
    owner(sp)
    sp.add_argument("--n", type=int, default=1)
    sp.add_argument("--kind", choices=["cli", "ui"])
    sp.add_argument("--lease-minutes", type=float, default=DEFAULT_LEASE_MINUTES)
    sp = add("verdict", cmd_verdict)
    sp.add_argument("node")
    owner(sp)
    sp.add_argument("--verdict", required=True, choices=VERDICTS)
    sp.add_argument("--reason", required=True)
    sp.add_argument("--recheck-minutes", type=float)
    sp.add_argument("--gap", type=int)
    sp.add_argument("--acted", action="store_true")
    sp.add_argument("--no-action", action="store_true")
    sp = add("check", cmd_check)
    sp.add_argument("node")
    owner(sp)
    sp.add_argument("--fingerprint", required=True)
    sp = add("ack", cmd_ack)
    sp.add_argument("nodes", nargs="+")
    owner(sp)
    sp = add("renew", cmd_renew)
    owner(sp)
    sp.add_argument("--lease-minutes", type=float, default=DEFAULT_LEASE_MINUTES)
    sp = add("release", cmd_release)
    sp.add_argument("nodes", nargs="+")
    owner(sp)
    sp = add("gate", cmd_gate)
    owner(sp)
    sp.add_argument("--lease-minutes", type=float, default=DEFAULT_LEASE_MINUTES)
    sp = add("wait", cmd_wait)
    owner(sp)
    sp.add_argument("--max-seconds", type=float, default=540)
    sp.add_argument("--batch", type=int, default=1)
    sp.add_argument("--poll-seconds", type=float, default=5)
    sp.add_argument("--lease-minutes", type=float, default=DEFAULT_LEASE_MINUTES)
    sp = add("manager-wait", cmd_manager_wait)
    sp.add_argument("--max-seconds", type=float, default=3000)
    sp.add_argument("--poll-seconds", type=float, default=15)
    sp.add_argument("--grace-seconds", type=float, default=60,
                    help="how long a due node may sit unclaimed before running shepherds are taken to have no room")
    sp.add_argument("--min-batch", type=int, default=3)
    sp.add_argument("--queue-delay-minutes", type=float, default=15)
    sp.add_argument("--max-parallel", type=int, default=5)
    sp.add_argument("--stale-seconds", type=float, default=DEFAULT_LEASE_MINUTES * 60,
                    help="a shepherd whose heartbeat (next, gate, wait, verdict) is older than this, as old as a lease, is dead")
    sp = add("signal", cmd_signal)
    sp.add_argument("action", choices=["pause", "resume", "retire", "status"])
    sp.add_argument("--owner")
    sp.add_argument("--all", action="store_true")
    sp = add("gap", cmd_gap)
    gsub = sp.add_subparsers(dest="action", required=True)
    g = gsub.add_parser("report")
    owner(g)
    g.add_argument("--node", help="the node that hit it (omit for a gap no node shows, e.g. a measure)")
    g.add_argument("--signature", required=True)
    g.add_argument("--summary", required=True)
    g = gsub.add_parser("list")
    g.add_argument("--open", action="store_true", help="the same as no flag: closed gaps are in the history")
    g = gsub.add_parser("show")
    g.add_argument("id", type=int)
    g = gsub.add_parser("set")
    g.add_argument("id", type=int)
    g.add_argument("--status", required=True, choices=GAP_STATUSES)
    g.add_argument("--commit")
    g.add_argument("--note")
    sp = add("hold", cmd_hold)
    sp.set_defaults(action="hold")
    sp.add_argument("nodes", nargs="+")
    sp.add_argument("--reason", default="")
    sp = add("unhold", cmd_hold)
    sp.set_defaults(action="unhold", reason=None)
    sp.add_argument("nodes", nargs="+")
    sp = add("clear", cmd_clear)
    sp.add_argument("--all", action="store_true")
    sp.add_argument("--node")
    sp.add_argument("--verdict")
    sp.add_argument("--reason-like")
    add("status", cmd_status)
    add("owners", cmd_owners)
    sp = add("history", cmd_history)
    sp.add_argument("--event", choices=["verdict", "gap_closed", "node_done", "node_left_scope", "owner_gone", "signal"])
    sp.add_argument("--signature")
    sp.add_argument("--node")
    sp.add_argument("--owner")
    sp.add_argument("--since-hours", type=float)
    sp.add_argument("--limit", type=int, default=50, help="the last N records (0: all)")
    return p


def main(argv=None):
    argv = list(sys.argv[1:] if argv is None else argv)
    # --text and --ledger work anywhere on the line, not only before the command
    rest, extra, i = [], [], 0
    while i < len(argv):
        if argv[i] == "--text":
            extra.append("--text")
        elif argv[i] == "--ledger" and i + 1 < len(argv):
            extra += ["--ledger", argv[i + 1]]
            i += 1
        else:
            rest.append(argv[i])
        i += 1
    a = build_parser().parse_args(rest[:1] + extra + rest[1:])
    path = a.ledger or os.path.join(a.root, "shepherd", "ledger.db")
    global HISTORY_PATH
    HISTORY_PATH = os.path.join(os.path.dirname(os.path.abspath(path)), "history.jsonl")
    tod = lib.open_tod(a.root)
    conn = open_ledger(path)
    try:
        out = a.fn(a, conn, tod)
    except sqlite3.Error as e:
        die(f"database error: {e}")
    finally:
        conn.close()
        tod.close()
    if out is not None:
        sys.stdout.write(emit(out, a.text) + "\n")


if __name__ == "__main__":
    main()
