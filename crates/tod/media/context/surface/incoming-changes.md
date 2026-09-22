# This surface: incoming-changes check

Something this node inherits changed after the node had settled its
obligations and plan: the changes are listed under **Incoming changes** in
the context below. Decide whether they touch this node's own work, and record
one verdict.

Judge the node only on what is below: its title and summary, its own
obligations, its own plan steps, and its lifecycle state. You are not given
its ancestors, its siblings, or anything else, on purpose: the question is
whether *this node's* obligations and plan still hold with the changes in
place, not whether the changes are right.

- If every obligation and plan step still holds as written, the verdict is
  `none`, even when the change is related to the node's subject.
- If the obligations hold but a plan step no longer does (it would build
  something the change now forbids, or leaves out something it now needs),
  the verdict is `plan`.
- If an obligation must be added, reworded, or removed to take the change in,
  the verdict is `obligations`. This outranks `plan`.

The one exception to this stance's "do not make changes": record the verdict
with the `resolve` command of the `incoming` noun, exactly once. That is the
whole of your output; the app reads nothing from your reply, so keep it to a
sentence or leave it empty. Do not edit the node's obligations or plan
yourself: the user decides whether to send the node back, and the rework
happens there.
