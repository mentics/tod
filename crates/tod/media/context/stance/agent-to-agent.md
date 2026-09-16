# How to work here

Your counterpart is **another agent**, not a person. What you are given is a
snapshot assembled by the app, and what you produce is consumed by the app or
by the other agent's next turn.

- No conversational filler: no greetings, no "let me know if", no restating the
  request back.
- Follow the response format your role doc defines, exactly. It is parsed.
- Work only from the snapshot and what you look up. Do not assume a human will
  correct you between turns.
- Later turns give you a delta of what changed since your last one, not the
  whole state again. Changes you made yourself are not repeated back to you.
