#!/usr/bin/env python3
"""Minimal fake Claude CLI that speaks ACP on stdin/stdout when invoked as `... acp`.

Set CLAUDE_BIN to this script (or its path) for local testing without a real Claude install.
Optional: set CLAUDE_MOCK_LOG to a file path to append one JSON line per handled prompt.
"""
from __future__ import annotations

import json
import os
import sys
from datetime import datetime, timezone


def main() -> None:
    if len(sys.argv) < 2 or sys.argv[1] != "acp":
        print("usage: mock_claude_acp.py acp", file=sys.stderr)
        sys.exit(2)
    run_acp()


def run_acp() -> None:
    session_id = "mock-claude-session"
    for raw in sys.stdin:
        line = raw.strip()
        if not line:
            continue
        try:
            msg = json.loads(line)
        except json.JSONDecodeError:
            continue
        if "method" not in msg or "id" not in msg:
            continue
        req_id = msg["id"]
        method = msg["method"]
        params = msg.get("params") or {}
        if method == "initialize":
            respond(req_id, {"protocolVersion": 1})
        elif method == "authenticate":
            respond(req_id, {})
        elif method == "session/new":
            respond(req_id, {"sessionId": session_id, "configOptions": []})
        elif method == "session/prompt":
            prompt_text = extract_prompt(params)
            reply = f"[claude-mock] {prompt_text[:120]}"
            send_notification(
                "session/update",
                {
                    "update": {
                        "sessionUpdate": "agent_message_chunk",
                        "content": {"text": reply},
                    }
                },
            )
            append_log({"prompt": prompt_text, "reply": reply})
            respond(req_id, {})
        elif method == "session/set_config_option":
            respond(req_id, {})


def extract_prompt(params: dict) -> str:
    chunks = []
    for item in params.get("prompt") or []:
        if isinstance(item, dict) and item.get("type") == "text":
            chunks.append(str(item.get("text") or ""))
    return "\n".join(chunks).strip()


def respond(req_id, result: dict) -> None:
    print(json.dumps({"jsonrpc": "2.0", "id": req_id, "result": result}), flush=True)


def send_notification(method: str, params: dict) -> None:
    print(json.dumps({"jsonrpc": "2.0", "method": method, "params": params}), flush=True)


def append_log(entry: dict) -> None:
    path = os.environ.get("CLAUDE_MOCK_LOG")
    if not path:
        return
    payload = {
        "ts": datetime.now(timezone.utc).isoformat(),
        **entry,
    }
    with open(path, "a", encoding="utf-8") as fh:
        fh.write(json.dumps(payload) + "\n")


if __name__ == "__main__":
    main()
