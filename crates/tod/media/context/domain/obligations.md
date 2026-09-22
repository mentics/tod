# Obligations

Obligations are what a node commits to. They come in two kinds, both attached
directly to one node:

- **Requirements** — what must be true for the work to be considered done.
- **Constraints** — what bounds how the work may be carried out: technology
  choices, compatibility rules, boundaries against other work.

They are ordered within their kind, and that order is meaningful: it is the
order shown to the user in the app.

An obligation's text must stand on its own. It is stored with the outline, not
beside any repository, so it cannot link to or name a file (a repo doc, a
relative markdown link, a local path) — state what the file says instead. Such
text is refused. URLs and `[[slug]]` node references are fine.

## Inheritance

The two kinds inherit differently down the outline, and the difference matters:

- An ancestor's **requirements** define that ancestor's own scope. They are
  settled and out of bounds for a descendant — a child does not re-satisfy or
  renegotiate them.
- An ancestor's **constraints** bound everything beneath it. They apply in full
  at every level of the subtree.

So when you are given an ancestor's context, expect its constraints in full and
only a summary of its requirements. That is deliberate, not truncation.

## References

A `[[slug]]` in an obligation names another node, usually a reusable
component this node uses. A referenced node is not inherited: none of its
obligations are in your context. When you need them, most often when
planning, since the component's requirements and constraints can call for plan
steps here, look the node up by its slug and list its obligations with
`tod-cli`.

