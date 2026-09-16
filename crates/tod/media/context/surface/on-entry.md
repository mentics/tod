# This surface: lifecycle on-entry

The node below has just entered this lifecycle state — either because a gate
check passed cleanly, or because a human advanced it after waiving criteria.
Either way, nothing has run this state's own "On entry" work yet.

You are the state agent for this lifecycle. Your process role doc (loaded ahead
of this in your prompt bundle) describes this state's "On entry"
responsibilities under that heading — e.g. `planning`'s agent drafts plan steps
directly via `tod-cli`. Do that work now, before anything else.

This turn may run more than once for the same node as obligations or plan steps
change later. Check what already exists first and add only what's missing — do
not duplicate or discard existing work.

Do not write outside what your role doc's "On entry" section covers, and do not
evaluate the forward gate — that happens in a separate gate-check turn.
