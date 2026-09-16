## `tod-cli node`

CRUD on outline nodes themselves. Nodes may be addressed by slug or by full
UUID everywhere a `<SLUG_OR_UUID>` argument is expected.

```
tod-cli --data-root <DATA_ROOT> node list   [--parent <SLUG_OR_UUID>]
tod-cli --data-root <DATA_ROOT> node show   <SLUG_OR_UUID>
tod-cli --data-root <DATA_ROOT> node search --query <TEXT> [--limit N]
tod-cli --data-root <DATA_ROOT> node create --title <TEXT> (--parent <SLUG_OR_UUID> | --list <SLUG_OR_UUID>) [--after <SLUG_OR_UUID>] [--before]
tod-cli --data-root <DATA_ROOT> node rename <SLUG_OR_UUID> --title <TEXT>
tod-cli --data-root <DATA_ROOT> node move   <SLUG_OR_UUID> --parent <SLUG_OR_UUID|root> [--after <SLUG_OR_UUID>] [--before]
tod-cli --data-root <DATA_ROOT> node delete <SLUG_OR_UUID>
```

`delete` removes the node and its entire subtree (archived for undo, same as
the app). A node's slug may change on rename if it was auto-derived from the
title — address it by id in scripts that rename and then reuse the reference.

Use `search` to find another node when you only have an approximate title —
e.g. the user says "base this on that reusable login component over there" and
you need its id or slug to reference or inspect it. The match is fuzzy (typo-
and skipped-letter-tolerant), not exact; it returns up to `--limit` (default
10) results across every list, best match first, one per line as
`<NODE_UUID> <slug> <title>`.
