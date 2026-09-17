## `tod-cli changeset`

The change set is everything this conversation has changed — nodes,
obligations, and plan steps — shown to the user net of every turn. It only
exists inside a conversation; outside one these commands fail.

```
tod-cli --data-root <DATA_ROOT> changeset list
tod-cli --data-root <DATA_ROOT> changeset flag   (--node <ID> | --obligation <ID> | --plan-step <ID>) --why <TEXT>
tod-cli --data-root <DATA_ROOT> changeset unflag (--node <ID> | --obligation <ID> | --plan-step <ID>)
```

`<ID>` is whatever that item's `show` command accepts: a node slug or UUID, or
an obligation or plan-step id in full or as its 8-character prefix.

`list` prints one line per changed item:
`<op> <entity> <id> on <node-slug>: <text>`, followed by any context in
parentheses (such as `from <node>`) and `<unsure: reason>` when flagged. `op`
is `added`, `edited`, `moved`, `deleted`, or `reversed`. `reversed` means the
user reversed that change; don't make it again unless asked. An item added and
then deleted in the same conversation does not appear.

`flag` marks an item in the change set as one you are not confident about;
`--why` is the one-line reason the user sees. Only items this conversation
changed can be flagged, and flagging an already-flagged item replaces the
reason. The flag clears on its own when the user edits or reverses the item;
`unflag` clears it yourself once the doubt is resolved.
