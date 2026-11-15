"""Shared by survey.py and ledger.py: read-only access to tod.db, node
resolution, and the fingerprint.

Neither script ever writes tod.db: the connection is `mode=ro`.
"""
import hashlib
import json
import sqlite3
import sys
import uuid


def die(msg, code=1):
    sys.stderr.write(f"error: {msg}\n")
    sys.exit(code)


def open_tod(root):
    path = f"{root}/tod.db"
    try:
        db = sqlite3.connect(f"file:{path}?mode=ro", uri=True, timeout=30)
        db.execute("SELECT 1 FROM nodes LIMIT 1")
    except sqlite3.Error as e:
        die(f"cannot read {path} read-only: {e}")
    return db


def has_table(db, name):
    return bool(db.execute("SELECT 1 FROM sqlite_master WHERE type='table' AND name=?", (name,)).fetchone())


def resolve(db, item):
    """Node ids (bytes) an item names: a UUID, a slug, or a ticket
    (`ENG-2925`, or a URL ending in it). Empty if it matches nothing."""
    try:
        return [uuid.UUID(item).bytes]
    except ValueError:
        pass
    rows = db.execute("SELECT id FROM nodes WHERE lower(slug) = lower(?)", (item,)).fetchall()
    rows = rows or db.execute(
        "SELECT node_id FROM node_fields WHERE upper(ticket) = upper(?) OR upper(ticket) LIKE '%/' || upper(?)",
        (item, item),
    ).fetchall()
    return [r[0] for r in rows]


def resolve_or_die(db, item):
    ids = resolve(db, item)
    if not ids:
        die(f"`{item}` matches no node (not a slug, UUID or ticket)")
    return ids


def resolve_one_or_die(db, item):
    ids = resolve_or_die(db, item)
    if len(ids) > 1:
        die(f"`{item}` matches {len(ids)} nodes; use a slug or UUID")
    return ids[0]


def subtree(db, root_ids):
    """The nodes at or under the roots that have Lifecycle (ids, bytes)."""
    out = []
    for nid in root_ids:
        out += [r[0] for r in db.execute(
            """WITH RECURSIVE t(id) AS (SELECT ?
                 UNION SELECT e.node_id FROM outline_entries e JOIN t ON e.parent_id = t.id)
               SELECT t.id FROM t JOIN node_lifecycle l ON l.node_id = t.id""", (nid,))]
    return list(dict.fromkeys(out))


def hexid(b):
    return b.hex()


def short_ticket(ticket):
    ticket = ticket or "-"
    return ticket.rstrip("/").rsplit("/", 1)[-1] if "/" in ticket else ticket


def snapshot(db, nid):
    """Everything the shepherds read about one node. `nid` is bytes."""
    q = lambda sql, *p: db.execute(sql, p).fetchall()
    slug, title = q("SELECT slug, title FROM nodes WHERE id = ?", nid)[0]
    fields = q("SELECT ticket, linked_prs FROM node_fields WHERE node_id = ?", nid)
    ticket, linked = fields[0] if fields else (None, "[]")
    row = q("SELECT state, updated_at FROM node_lifecycle WHERE node_id = ?", nid)
    state, state_at = row[0] if row else (None, None)
    run = q("SELECT phase, reason_kind, reason_text, since FROM run_state WHERE node_id = ?", nid)
    phase, kind, text, since = run[0] if run else ("none", "", "", None)
    decisions = q("SELECT id, question FROM decisions WHERE node_id = ? AND status = 'pending' ORDER BY created_at, id", nid)
    waits = q("SELECT match_spec, state, due_at FROM waits WHERE node_id = ? AND state = 'pending' ORDER BY match_spec", nid)
    sandbox = (q("SELECT sandbox FROM node_files_locations WHERE node_id = ?", nid) or [[None]])[0][0]
    heads = None
    if has_table(db, "conversation_reports") and has_table(db, "conversations"):
        # The PR heads tod's own pr_draft review last reviewed. tod.db does not
        # record the live PR head anywhere else.
        for (body,) in q(
            """SELECT r.body FROM conversation_reports r JOIN conversations c ON c.id = r.conversation_id
               WHERE c.protocol = 'review' AND c.focus_kind = 'node' AND c.focus_id = ?
                 AND r.body LIKE '%pr_heads%'
               ORDER BY c.created_at DESC, r.turn_seq DESC LIMIT 1""", nid):
            try:
                heads = json.loads(body).get("pr_heads")
            except (ValueError, AttributeError) as e:
                die(f"unreadable pr_heads report for node {slug}: {e}")
    return dict(
        id=hexid(nid), slug=slug, title=title, ticket=short_ticket(ticket), state=state, state_at=state_at,
        phase=phase, kind=kind, text=text, since=since,
        decisions=[(hexid(d), question) for d, question in decisions],
        waits=[(spec, st, due) for spec, st, due in waits],
        sandbox=sandbox, linked_prs=linked or "[]", reviewed_heads=heads,
    )


# A waiting_outside run's text is free wording for what it waits on, and the
# babysitter alternates it between an event id and a sentence for the SAME wait
# (gap 23). reason_kind already says why and `waits` says what, so the text is
# left out there. A stopped or failed run's text carries the error: kept.
WORDING_ONLY_KINDS = ("waiting_outside",)


def stable_reason_text(kind, text):
    return "" if kind in WORDING_ONLY_KINDS else text


def fingerprint_parts(snap):
    """The fields that change when something meaningful happens. Left out on
    purpose: updated_at, `since`, wait due times and attempt counters, leases
    and heartbeats, anything that moves on every tick."""
    return {
        "state": snap["state"],
        "run.phase": snap["phase"],
        "run.reason_kind": snap["kind"],
        "run.reason_text": stable_reason_text(snap["kind"], snap["text"]),
        "decisions": sorted(d for d, _ in snap["decisions"]),
        "waits": sorted(f"{spec}|{st}" for spec, st, _ in snap["waits"]),
        "linked_prs": snap["linked_prs"],
        "reviewed_pr_heads": snap["reviewed_heads"] or {},
        "sandbox": snap["sandbox"],
    }


def fingerprint(snap):
    parts = fingerprint_parts(snap)
    blob = json.dumps(parts, sort_keys=True, separators=(",", ":"))
    return hashlib.sha256(blob.encode()).hexdigest()[:16], parts


def diff_parts(old, new):
    """Human lines for what differs between two parts dicts."""
    out = []
    for k in sorted(set(old) | set(new)):
        if old.get(k) != new.get(k):
            out.append(f"{k}: {json.dumps(old.get(k))} -> {json.dumps(new.get(k))}")
    return out
