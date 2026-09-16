## `tod-cli drafting`

The record of drafting a node's spec: the user's dumps, the rare choices put to
the user, and the node's **buildable** evaluation (the `design` → `planning`
gate). Choices are `c-<n>` and dumps `d-<n>`.

```
tod-cli --data-root <DATA_ROOT> drafting dump            [--node <NODE_UUID>] --body <TEXT>
tod-cli --data-root <DATA_ROOT> drafting dumps           --node <NODE_UUID> [--limit N]
tod-cli --data-root <DATA_ROOT> drafting choices         --node <NODE_UUID> [--status open|answered|delegated|withdrawn]
tod-cli --data-root <DATA_ROOT> drafting add-choice      --node <NODE_UUID>      # YAML on stdin: question, context, options: [{label, obligations: [{kind, body, section}]}]
tod-cli --data-root <DATA_ROOT> drafting withdraw-choice --node <NODE_UUID> <c-N>
tod-cli --data-root <DATA_ROOT> drafting buildable       --node <NODE_UUID> --outcome pass|fail|pending [--detail <TEXT>]
```

`dump` hands the text to the node's drafter, as if the user had typed it in the
drafting view.
