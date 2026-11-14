#!/usr/bin/env python3
"""Send one command to tod's agent control socket and print its whole reply.

Usage:
  python3 -I sock.py <port> '<command>' [--timeout SECONDS]

Most replies are one line (`ok …` / `err …`). `tree` replies `ok N nodes` and
then N lines; `coverage` replies `ok … M missing` and then M lines: those are
read in full. Exits 1 on an `err` reply, so a shell can tell.
"""
import re
import socket
import sys

args = sys.argv[1:]
timeout = 90.0
if "--timeout" in args:
    i = args.index("--timeout")
    timeout = float(args[i + 1])
    del args[i : i + 2]
if len(args) != 2:
    sys.exit(__doc__)
port, command = int(args[0]), args[1]

# Input commands may block up to 60 s while the user has the app.
with socket.create_connection(("127.0.0.1", port), timeout=timeout) as s:
    s.sendall((command + "\n").encode())
    f = s.makefile(encoding="utf-8")
    first = f.readline().rstrip("\n")
    print(first)
    more = 0
    if m := re.match(r"ok (\d+) nodes\b", first):
        more = int(m.group(1))
    elif m := re.search(r"\b(\d+) missing\b", first):
        more = int(m.group(1))
    for _ in range(more):
        print(f.readline().rstrip("\n"))
sys.exit(1 if first.startswith("err") else 0)
