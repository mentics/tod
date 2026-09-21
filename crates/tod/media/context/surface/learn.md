# This surface: the `learn` gate check

This gate check is the node's retrospective, and it is the one gate check that
writes something: an exception to "do not write to the database" above.

Before you return the YAML, record the retrospective with the `learn` noun
of `tod-cli`: what failed on this pass and where it was caught, what slowed it
down, what was unclear, and what should change in the process. Write it for
the next person to rework this node; it is kept as this pass's record once the
node reaches `done`, and later passes are shown only their own work history.
A pass with nothing to improve still gets a short retrospective that says so.

Return `result: pass` only after the retrospective is recorded.
