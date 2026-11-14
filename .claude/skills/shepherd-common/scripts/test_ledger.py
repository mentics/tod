"""Tests for ledger.py against a tiny fake tod.db. Run: python3 -m unittest test_ledger -v"""
import contextlib
import io
import json
import os
import sqlite3
import subprocess
import sys
import tempfile
import threading
import time
import unittest
import uuid

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import ledger  # noqa: E402

TOD_SCHEMA = """
CREATE TABLE nodes (id BLOB PRIMARY KEY, slug TEXT UNIQUE, title TEXT, created_at INT, updated_at INT, managed INT DEFAULT 0);
CREATE TABLE node_fields (node_id BLOB PRIMARY KEY, ticket TEXT, linked_prs TEXT NOT NULL DEFAULT '[]', updated_at INT DEFAULT 0);
CREATE TABLE node_lifecycle (node_id BLOB PRIMARY KEY, state TEXT NOT NULL, updated_at INT NOT NULL);
CREATE TABLE run_state (node_id BLOB PRIMARY KEY, phase TEXT, reason_kind TEXT, reason_text TEXT, since INT, updated_at INT);
CREATE TABLE decisions (id BLOB PRIMARY KEY, node_id BLOB, question TEXT, status TEXT DEFAULT 'pending', created_at INT);
CREATE TABLE waits (id BLOB PRIMARY KEY, node_id BLOB, match_spec TEXT, due_at INT, state TEXT DEFAULT 'pending');
CREATE TABLE node_files_locations (node_id BLOB PRIMARY KEY, sandbox TEXT);
CREATE TABLE outline_entries (node_id BLOB PRIMARY KEY, parent_id BLOB);
"""


class Tod:
    def __init__(self, root):
        self.root = root
        self.db = sqlite3.connect(os.path.join(root, "tod.db"))
        self.db.executescript(TOD_SCHEMA)
        self.ids = {}

    def node(self, slug, ticket=None, state="active", parent=None, lifecycle=True, phase="working"):
        nid = uuid.uuid4().bytes
        self.ids[slug] = nid
        self.db.execute("INSERT INTO nodes(id, slug, title, created_at, updated_at) VALUES (?,?,?,0,0)", (nid, slug, slug))
        self.db.execute("INSERT INTO node_fields(node_id, ticket) VALUES (?,?)", (nid, ticket))
        self.db.execute("INSERT INTO outline_entries VALUES (?,?)", (nid, self.ids[parent] if parent else None))
        if lifecycle:
            self.db.execute("INSERT INTO node_lifecycle VALUES (?,?,1)", (nid, state))
            self.db.execute("INSERT INTO run_state VALUES (?,?,?,?,1,1)", (nid, phase, "working", "x"))
        self.db.commit()

    def set(self, sql, *p):
        self.db.execute(sql, p)
        self.db.commit()

    def state(self, slug, state):
        self.set("UPDATE node_lifecycle SET state=? WHERE node_id=?", state, self.ids[slug])

    def run(self, slug, phase, kind="k", text="t"):
        self.set("UPDATE run_state SET phase=?, reason_kind=?, reason_text=? WHERE node_id=?", phase, kind, text, self.ids[slug])

    def decide(self, slug):
        self.set("INSERT INTO decisions VALUES (?,?,?,?,?)", uuid.uuid4().bytes, self.ids[slug], "q?", "pending", 1)


class Base(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.root = self.tmp.name
        self.tod = Tod(self.root)
        self.clock = [1_000_000.0]
        self._orig = (ledger.now, ledger.shepherd_version)
        ledger.now = lambda: self.clock[0]
        self.version = ["v1"]
        ledger.shepherd_version = lambda: self.version[0]

    def tearDown(self):
        ledger.now, ledger.shepherd_version = self._orig
        self.tmp.cleanup()

    def cli(self, *args, expect=0):
        """Run ledger.main in-process; returns parsed JSON (or the text)."""
        out, err = io.StringIO(), io.StringIO()
        code = 0
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            try:
                ledger.main([self.root, *args])
            except SystemExit as e:
                code = e.code if isinstance(e.code, int) else 1
        self.assertEqual(code, expect, f"{args}: exit {code}, stderr {err.getvalue()}")
        text = out.getvalue().strip()
        try:
            return json.loads(text)
        except ValueError:
            return text

    def setup_nodes(self, *slugs):
        for s in slugs:
            self.tod.node(s)
        self.cli("scope", "add", *slugs)

    def lease(self, owner, n=10):
        return self.cli("next", "--owner", owner, "--n", str(n))["leased"]

    def verdict(self, slug, owner, v="ceiling", *extra):
        return self.cli("verdict", slug, "--owner", owner, "--verdict", v, "--reason", "r", *extra)


class ScopeAndSync(Base):
    def test_unknown_item_is_an_error(self):
        self.cli("scope", "add", "nope", expect=1)

    def test_subtree_rescoped_on_every_sync(self):
        self.tod.node("parent", lifecycle=True)
        self.tod.node("kid", parent="parent")
        self.cli("scope", "add", "parent")
        self.assertEqual(self.cli("sync")["in_scope"], 2)
        self.tod.node("newkid", parent="kid")
        self.assertEqual(self.cli("sync")["in_scope"], 3)

    def test_subtree_through_non_lifecycle_parent(self):
        self.tod.node("folder", lifecycle=False)
        self.tod.node("a", parent="folder")
        self.cli("scope", "add", "folder")
        self.assertEqual(self.cli("sync")["in_scope"], 1)

    def test_ticket_and_remove(self):
        self.tod.node("a", ticket="https://linear.app/x/issue/ENG-1")
        self.cli("scope", "add", "ENG-1")
        self.assertEqual(self.cli("sync")["in_scope"], 1)
        self.cli("scope", "remove", "ENG-1")
        self.assertEqual(self.cli("sync")["in_scope"], 0)
        self.cli("scope", "remove", "ENG-1", expect=1)

    def test_done_is_finished(self):
        self.setup_nodes("a", "b")
        self.tod.state("a", "done")
        c = self.cli("sync")
        self.assertEqual((c["finished"], c["due"]), (1, 1))
        self.assertEqual([n["slug"] for n in self.lease("o")], ["b"])


class Leasing(Base):
    def test_priority_order(self):
        for s in ("routine", "never", "stopped", "ask", "overdue"):
            self.tod.node(s)
        self.cli("scope", "add", "routine", "never", "stopped", "ask", "overdue")
        self.cli("sync")
        self.lease("w", 10)
        self.verdict("routine", "w")
        self.cli("release", "never", "stopped", "ask", "overdue", "--owner", "w")
        self.tod.run("stopped", "stopped")
        self.tod.decide("ask")
        self.tod.set("INSERT INTO waits VALUES (?,?,?,?,'pending')", uuid.uuid4().bytes, self.tod.ids["overdue"], "gh:x", 1)
        self.tod.state("routine", "design")  # fingerprint change -> due again
        order = [n["slug"] for n in self.lease("p", 10)]
        self.assertEqual(order, ["stopped", "ask", "overdue", "routine", "never"])

    def test_concurrent_next_never_double_leases(self):
        slugs = [f"n{i}" for i in range(12)]
        self.setup_nodes(*slugs)
        self.tod.db.close()
        script = os.path.join(HERE, "ledger.py")
        procs = [subprocess.Popen([sys.executable, "-I", script, self.root, "next", "--owner", f"o{i}", "--n", "3"],
                                  stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True) for i in range(8)]
        seen = []
        for p in procs:
            out, err = p.communicate(timeout=60)
            self.assertEqual(p.returncode, 0, err)
            seen += [n["slug"] for n in json.loads(out)["leased"]]
        self.assertEqual(len(seen), len(set(seen)))
        self.assertEqual(len(seen), 12)

    def test_concurrent_next_threads(self):
        slugs = [f"t{i}" for i in range(10)]
        self.setup_nodes(*slugs)
        got, errs = [], []

        def worker(i):
            try:
                conn = ledger.open_ledger(os.path.join(self.root, "shepherd", "ledger.db"))
                tod = ledger.lib.open_tod(self.root)
                a = type("A", (), dict(owner=f"o{i}", n=2, kind=None, lease_minutes=20.0))
                got.extend(n["slug"] for n in ledger.cmd_next(a, conn, tod)["leased"])
            except BaseException as e:  # noqa: BLE001
                errs.append(e)

        ts = [threading.Thread(target=worker, args=(i,)) for i in range(6)]
        [t.start() for t in ts]
        [t.join() for t in ts]
        self.assertEqual(errs, [])
        self.assertEqual(len(got), len(set(got)))
        self.assertEqual(len(got), 10)

    def test_lease_expiry(self):
        self.setup_nodes("a")
        self.assertEqual(len(self.lease("o1")), 1)
        self.assertEqual(self.lease("o2"), [])
        self.clock[0] += 21 * 60
        self.assertEqual([n["slug"] for n in self.lease("o2")], ["a"])
        # o1 lost it: check refuses
        fp = self.cli("sync")  # noqa: F841
        self.cli("check", "a", "--owner", "o1", "--fingerprint", "x", expect=3)

    def test_renew_extends(self):
        self.setup_nodes("a")
        self.lease("o1")
        self.clock[0] += 15 * 60
        self.assertEqual(self.cli("renew", "--owner", "o1")["renewed"], 1)
        self.clock[0] += 15 * 60
        self.assertEqual(self.lease("o2"), [])

    def test_hold_is_never_leased(self):
        self.setup_nodes("a", "b")
        self.cli("hold", "a", "--reason", "mine")
        self.assertEqual([n["slug"] for n in self.lease("o")], ["b"])
        self.assertEqual(self.cli("sync")["held"], 1)
        self.cli("unhold", "a")
        self.cli("release", "b", "--owner", "o")
        self.assertEqual({n["slug"] for n in self.lease("o")}, {"a", "b"})

    def test_n_is_a_cap_including_held(self):
        self.setup_nodes("a", "b", "c")
        self.assertEqual(len(self.lease("o", 2)), 2)
        again = self.lease("o", 2)
        self.assertEqual(len(again), 2)
        self.assertFalse(any(n["new"] for n in again))


class Verdicts(Base):
    def test_fingerprint_change_makes_due(self):
        self.setup_nodes("a")
        self.lease("o")
        self.verdict("a", "o")
        self.assertEqual(self.cli("sync")["due"], 0)
        self.tod.run("a", "stopped", "failed", "boom")
        self.assertEqual(self.cli("sync")["due"], 1)

    def test_noise_does_not_change_fingerprint(self):
        self.setup_nodes("a")
        self.lease("o")
        self.verdict("a", "o")
        self.tod.set("UPDATE run_state SET updated_at = 99, since = 98 WHERE node_id = ?", self.tod.ids["a"])
        self.tod.set("UPDATE node_lifecycle SET updated_at = 99 WHERE node_id = ?", self.tod.ids["a"])
        self.assertEqual(self.cli("sync")["due"], 0)

    def test_recheck(self):
        self.setup_nodes("a")
        self.lease("o")
        self.verdict("a", "o", "in_motion", "--recheck-minutes", "10")
        self.assertEqual(self.cli("sync")["due"], 0)
        self.clock[0] += 11 * 60
        self.assertEqual(self.cli("sync")["due"], 1)

    def test_required_flags(self):
        self.setup_nodes("a")
        self.lease("o")
        self.cli("verdict", "a", "--owner", "o", "--verdict", "in_motion", "--reason", "r", expect=1)
        self.cli("verdict", "a", "--owner", "o", "--verdict", "blocked_on_gap", "--reason", "r", expect=1)
        self.cli("verdict", "a", "--owner", "o", "--verdict", "blocked_on_gap", "--reason", "r", "--gap", "9", expect=1)

    def test_version_change_voids(self):
        self.setup_nodes("a")
        self.lease("o")
        self.verdict("a", "o")
        self.assertEqual(self.cli("sync")["due"], 0)
        self.version[0] = "v2"
        self.assertEqual(self.cli("sync")["due"], 1)

    def test_visits_and_release(self):
        self.setup_nodes("a", "b")
        self.lease("o")
        self.verdict("a", "o", "ceiling", "--acted")
        self.verdict("b", "o", "parked", "--no-action")
        o = self.cli("owners")["owners"][0]
        self.assertEqual((o["visits"], o["empty_visits"], o["held"]), (2, 1, []))

    def test_clear(self):
        self.setup_nodes("a", "b")
        self.lease("o")
        self.verdict("a", "o", "ceiling")
        self.verdict("b", "o", "parked")
        self.assertEqual(self.cli("clear", "--verdict", "parked")["cleared"], 1)
        self.assertEqual(self.cli("sync")["due"], 1)
        self.assertEqual(self.cli("clear", "--all")["cleared"], 1)
        self.assertEqual(self.cli("sync")["due"], 2)

    def test_check_guard(self):
        self.setup_nodes("a")
        n = self.lease("o")[0]
        self.cli("check", "a", "--owner", "o", "--fingerprint", n["fingerprint"])
        self.cli("check", "a", "--owner", "other", "--fingerprint", n["fingerprint"], expect=3)
        self.tod.run("a", "stopped", "failed", "boom")
        out = self.cli("check", "a", "--owner", "o", "--fingerprint", n["fingerprint"], expect=3)
        self.assertEqual(out["reason"], "fingerprint_changed")
        self.assertTrue(any("run.phase" in c for c in out["changed"]))
        self.assertNotEqual(out["fingerprint"], n["fingerprint"])


class Gaps(Base):
    def test_dedupe_and_deploy(self):
        self.setup_nodes("a", "b")
        self.lease("o")
        g1 = self.cli("gap", "report", "--owner", "o", "--node", "a", "--signature", "sig", "--summary", "s")
        g2 = self.cli("gap", "report", "--owner", "p", "--node", "b", "--signature", "sig", "--summary", "s")
        self.assertEqual((g1["id"], g1["created"], g2["id"], g2["created"]), (1, True, 1, False))
        self.assertEqual(sorted(g2["nodes"]), ["a", "b"])
        self.verdict("a", "o", "blocked_on_gap", "--gap", "1")
        self.verdict("b", "o", "ceiling")
        self.assertEqual(self.cli("sync")["due"], 0)
        self.cli("gap", "set", "1", "--status", "fixing", "--commit", "abc")
        self.assertEqual(self.cli("sync")["due"], 0)
        out = self.cli("gap", "set", "1", "--status", "deployed", "--note", "n")
        self.assertEqual(out["nodes_made_due"], 1)
        n = self.lease("o")
        self.assertEqual([x["slug"] for x in n], ["a"])
        self.assertEqual(n[0]["due_reason"], "fix for gap 1 deployed")
        self.assertEqual(self.cli("gap", "list", "--open")["gaps"], [])
        # a new report with the same signature after deploy is a new gap
        self.assertEqual(self.cli("gap", "report", "--owner", "o", "--node", "a", "--signature", "sig", "--summary", "s")["id"], 2)
        self.assertEqual(self.cli("gap", "show", "1")["status"], "deployed")


class History(Base):
    """The ledger holds only what is current; the past goes to history.jsonl."""

    def rows(self, sql):
        db = sqlite3.connect(os.path.join(self.root, "shepherd", "ledger.db"))
        try:
            return db.execute(sql).fetchall()
        finally:
            db.close()

    def history(self, *args):
        return self.cli("history", "--limit", "0", *args)["history"]

    def test_closed_gap_leaves_the_ledger_and_a_recurrence_is_flagged(self):
        self.setup_nodes("a")
        self.lease("o")
        self.cli("gap", "report", "--owner", "o", "--node", "a", "--signature", "bug:x:y", "--summary", "s")
        self.cli("gap", "set", "1", "--status", "fixing", "--commit", "abc")
        out = self.cli("gap", "set", "1", "--status", "deployed")
        self.assertTrue(out["moved_to_history"])
        self.assertEqual(self.rows("SELECT COUNT(*) FROM gaps")[0][0], 0)
        self.assertEqual(self.rows("SELECT COUNT(*) FROM gap_nodes")[0][0], 0)
        self.assertEqual(self.cli("gap", "list")["gaps"], [])
        [rec] = self.history("--event", "gap_closed")
        self.assertEqual((rec["id"], rec["status"], rec["commit"], [n["slug"] for n in rec["nodes"]]), (1, "deployed", "abc", ["a"]))
        again = self.cli("gap", "report", "--owner", "o", "--signature", "bug:x:y", "--summary", "s2")
        self.assertEqual((again["id"], again["recurrence"]), (2, 1))
        self.assertEqual(again["closed_before"][0]["commit"], "abc")
        self.cli("gap", "set", "2", "--status", "wontfix")
        # even with the ledger deleted, an id in the history is never reused
        os.remove(os.path.join(self.root, "shepherd", "ledger.db"))
        self.assertEqual(self.cli("gap", "report", "--owner", "o", "--signature", "z", "--summary", "s")["id"], 3)

    def test_a_gap_without_a_node(self):
        g = self.cli("gap", "report", "--owner", "manager", "--signature", "measure:review:rounds", "--summary", "s")
        self.assertEqual((g["id"], g["nodes"]), (1, []))
        self.assertEqual(self.cli("owners")["owners"], [])  # the reporter is not made a shepherd

    def test_done_and_unscoped_nodes_leave_the_ledger(self):
        self.setup_nodes("a", "b", "c")
        self.lease("o")
        self.verdict("a", "o")
        self.tod.state("a", "done")
        c = self.cli("sync")
        self.assertEqual((c["in_scope"], c["finished"]), (3, 1))
        self.assertEqual(sorted(r[0] for r in self.rows("SELECT slug FROM nodes")), ["b", "c"])
        [rec] = self.history("--event", "node_done")
        self.assertEqual((rec["slug"], rec["last_verdict"]), ("a", "ceiling"))
        self.cli("sync")  # a done node is not put back
        self.assertEqual(len(self.rows("SELECT slug FROM nodes")), 2)
        self.cli("scope", "remove", "c")
        self.cli("sync")
        self.assertEqual([r[0] for r in self.rows("SELECT slug FROM nodes")], ["b"])
        self.assertEqual([r["slug"] for r in self.history("--event", "node_left_scope")], ["c"])

    def test_every_verdict_is_recorded(self):
        self.setup_nodes("a")
        self.lease("o")
        self.verdict("a", "o", "parked", "--no-action")
        [rec] = self.history("--event", "verdict")
        self.assertEqual((rec["slug"], rec["verdict"], rec["owner"], rec["no_action"]), ("a", "parked", "o", True))

    def test_a_gone_shepherd_leaves_the_ledger_with_its_counts(self):
        self.setup_nodes("a")
        self.lease("o")
        self.verdict("a", "o", "ceiling", "--acted")
        self.cli("signal", "retire", "--owner", "o")
        self.cli("gate", "--owner", "o", expect=11)
        self.clock[0] += ledger.RETIRED_OWNER_SECONDS + 1
        self.cli("sync")
        self.assertEqual(self.cli("owners")["owners"], [])
        [rec] = self.history("--event", "owner_gone")
        self.assertEqual((rec["owner"], rec["how"], rec["visits"]), ("o", "retired", 1))
        self.assertEqual(self.rows("SELECT COUNT(*) FROM signals")[0][0], 0)

    def test_only_the_latest_signal_is_kept(self):
        self.cli("signal", "pause", "--owner", "o")
        self.cli("signal", "resume", "--owner", "o")
        self.cli("signal", "pause", "--all")
        self.cli("signal", "resume", "--all")
        self.assertEqual(self.rows("SELECT owner, signal FROM signals"), [("*", "resume")])
        self.assertEqual(len(self.history("--event", "signal")), 4)

    def test_old_fingerprints_are_pruned(self):
        self.setup_nodes("a")
        self.lease("o")
        self.tod.run("a", "stopped")
        self.cli("sync")
        self.tod.run("a", "working", "working", "x")  # back to the leased state
        self.clock[0] += ledger.FP_PARTS_KEEP_SECONDS + 1
        self.cli("sync")
        # left: the current fingerprint (also the lease's); gone: the stopped one
        self.assertEqual(self.rows("SELECT COUNT(*) FROM fp_parts")[0][0], 1)


class Signals(Base):
    def test_gate_and_status(self):
        self.setup_nodes("a")
        self.assertEqual(self.cli("gate", "--owner", "o"), "ok")
        self.cli("signal", "pause", "--owner", "o")
        st = self.cli("signal", "status")
        self.assertFalse(st["all_paused"])
        self.assertEqual(st["awaiting_pause"], ["o"])
        self.assertEqual(self.cli("gate", "--owner", "o", expect=10), "pause")
        self.assertTrue(self.cli("signal", "status")["all_paused"])
        self.cli("signal", "resume", "--owner", "o")
        self.assertEqual(self.cli("gate", "--owner", "o"), "ok")
        self.cli("signal", "retire", "--all")
        self.assertEqual(self.cli("gate", "--owner", "o", expect=11), "retire")
        self.cli("signal", "pause", expect=1)

    def test_all_applies_to_late_owners(self):
        self.cli("signal", "pause", "--all")
        self.assertEqual(self.cli("gate", "--owner", "late", expect=10), "pause")

    def test_retire_to_all_spares_shepherds_started_after_it(self):
        self.assertEqual(self.cli("gate", "--owner", "old"), "ok")
        self.cli("signal", "retire", "--all")
        self.assertEqual(self.cli("gate", "--owner", "old", expect=11), "retire")
        self.assertEqual(self.cli("gate", "--owner", "fresh"), "ok")


class Wait(Base):
    def wait(self, *extra, expect=0):
        return self.cli("wait", "--max-seconds", "2", "--poll-seconds", "0.05", *extra, expect=expect)

    def test_timeout(self):
        self.setup_nodes("a")
        self.lease("o")
        out = self.wait("--owner", "o", "--batch", "1")
        self.assertEqual(out["reason"], "timeout")

    def test_due_available(self):
        self.setup_nodes("a")
        out = self.wait("--owner", "o", "--batch", "1")
        self.assertEqual((out["reason"], out["nodes"][0]["slug"]), ("due_available", "a"))
        # not leased by wait
        self.assertEqual(len(self.lease("o")), 1)

    def test_batch_full_does_not_return_due(self):
        self.setup_nodes("a", "b")
        self.lease("o", 1)
        self.assertEqual(self.wait("--owner", "o", "--batch", "1")["reason"], "timeout")

    def test_held_changed(self):
        self.setup_nodes("a")
        self.lease("o")
        threading.Timer(0.3, lambda: self._mutate()).start()
        out = self.wait("--owner", "o", "--batch", "1")
        self.assertEqual((out["reason"], out["nodes"][0]["slug"]), ("held_changed", "a"))

    def _mutate(self):
        db = sqlite3.connect(os.path.join(self.root, "tod.db"))
        db.execute("UPDATE run_state SET phase = 'stopped'")
        db.commit()
        db.close()

    def test_held_recheck(self):
        self.setup_nodes("a")
        self.lease("o")
        self.verdict("a", "o", "in_motion", "--recheck-minutes", "5")
        self.tod.state("a", "design")
        self.lease("o")  # leased again because the fingerprint changed; recheck_at still ahead
        self.clock[0] += 6 * 60
        out = self.wait("--owner", "o", "--batch", "1")
        self.assertEqual(out["reason"], "held_recheck")

    def test_signal_pause_blocks_until_resume(self):
        self.setup_nodes("a")
        self.cli("signal", "pause", "--owner", "o")
        out = self.wait("--owner", "o", "--batch", "1")  # paused: a is due but wait keeps blocking
        self.assertEqual((out["reason"], out["paused"]), ("timeout", True))
        self.assertEqual(self.cli("owners")["owners"][0]["ack"], "paused")
        threading.Timer(0.3, lambda: self._signal("resume")).start()
        out = self.wait("--owner", "o", "--batch", "1")
        self.assertEqual((out["reason"], out["signal"]), ("signal", "resume"))

    def _signal(self, sig):
        conn = ledger.open_ledger(os.path.join(self.root, "shepherd", "ledger.db"))
        conn.execute("INSERT INTO signals(owner, signal, at) VALUES ('o', ?, 0)", (sig,))
        conn.close()

    def test_signal_pause_and_retire_during_wait(self):
        self.setup_nodes("a")
        self.lease("o")
        threading.Timer(0.3, lambda: self._signal("pause")).start()
        out = self.wait("--owner", "o", "--batch", "1")
        self.assertEqual((out["reason"], out["signal"]), ("signal", "pause"))
        self.cli("signal", "retire", "--owner", "o")
        self.assertEqual(self.wait("--owner", "o")["signal"], "retire")


class Version(Base):
    def test_real_version_hashes_files(self):
        ledger.shepherd_version = self._orig[1]
        v = ledger.shepherd_version()
        self.assertEqual(len(v), 12)
        self.assertEqual(v, ledger.shepherd_version())


class Fingerprint(Base):
    def test_reviewed_heads_and_sandbox_count(self):
        self.tod.db.executescript("""
            CREATE TABLE conversations (id BLOB PRIMARY KEY, focus_kind TEXT, focus_id BLOB, protocol TEXT, created_at INT);
            CREATE TABLE conversation_reports (conversation_id BLOB, turn_seq INT, body TEXT);""")
        self.setup_nodes("a")
        fp0 = self.lease("o")[0]["fingerprint"]
        cid = uuid.uuid4().bytes
        self.tod.set("INSERT INTO conversations VALUES (?,?,?,?,1)", cid, "node", self.tod.ids["a"], "review")
        self.tod.set("INSERT INTO conversation_reports VALUES (?,?,?)", cid, 1, '{"pr_heads":{"o/r#1":"abc"},"review":"done"}')
        fp1 = self.cli("check", "a", "--owner", "o", "--fingerprint", fp0, expect=3)["fingerprint"]
        self.tod.set("INSERT INTO node_files_locations VALUES (?, 'sb')", self.tod.ids["a"])
        self.assertNotEqual(self.cli("check", "a", "--owner", "o", "--fingerprint", fp1, expect=3)["fingerprint"], fp1)


if __name__ == "__main__":
    unittest.main()
