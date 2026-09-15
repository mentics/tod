# Reading and changing data

You do **not** have direct database access, and you should not try to open the
database file yourself. Use the `tod-cli` command instead. It goes through the
same validated code path the application UI uses, so it cannot leave the data in
an inconsistent state.

`tod-cli` is installed next to the tod executable. Every invocation needs the
data root, which is given to you in the context section below:

```
tod-cli --data-root <DATA_ROOT> <noun> <command> [options]
```

Add `--json` to any read command when you want to parse the result rather than
read it.

## node

Every node has a stable, unique **slug**, shown alongside its title. Nodes may
be addressed by slug or by full UUID everywhere a `<SLUG_OR_UUID>` argument is
expected below — prefer the slug once you know it, since it stays valid even
if the node is renamed.

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
e.g. the user says "base this on that reusable login component over there"
and you need its id or slug to reference or inspect it. The match is fuzzy
(typo- and skipped-letter-tolerant), not exact; it returns up to `--limit`
(default 10) results across every list, best match first, one per line as
`<NODE_UUID> <slug> <title>`.

## obligations

```
tod-cli --data-root <DATA_ROOT> obligations list       --node <NODE_UUID> [--kind requirement|constraint] [--inherited]
tod-cli --data-root <DATA_ROOT> obligations show       <OBLIGATION_UUID>
tod-cli --data-root <DATA_ROOT> obligations add        --node <NODE_UUID> --kind requirement|constraint --body <TEXT> [--after <OBLIGATION_UUID>] [--before] [--attention low|medium|high --why <TEXT>]
tod-cli --data-root <DATA_ROOT> obligations update     <OBLIGATION_UUID> [--body <TEXT>] [--attention low|medium|high --why <TEXT>]
tod-cli --data-root <DATA_ROOT> obligations move       <OBLIGATION_UUID> --node <NODE_UUID>
tod-cli --data-root <DATA_ROOT> obligations delete     <OBLIGATION_UUID>
tod-cli --data-root <DATA_ROOT> obligations deleted    --node <NODE_UUID> [--by user|agent|<SESSION_ID>]
tod-cli --data-root <DATA_ROOT> obligations history    <OBLIGATION_UUID>
tod-cli --data-root <DATA_ROOT> obligations restore    <r-N>... | --node <NODE_UUID> --by user|agent|<SESSION_ID>
tod-cli --data-root <DATA_ROOT> obligations check-refs [--node <NODE_UUID>]
```

`add` appends to the end of its kind group by default. Pass `--after` to place
it after a specific obligation, and add `--before` to place it before that one
instead.

Any text flag (`--body`, `--why`, `--detail`) takes `-` to read its text from
stdin, for long or multi-line text passed with a heredoc. Only one flag per
command can read stdin. Obligation text with no words is refused.

Every obligation has a **provenance**. What you write through `tod-cli` is
`agent`: in effect, but not confirmed by the user, and listed with a
`<agent, attention: reason>` mark. Only the user, in the app, makes one `user`.
When you write or change one, give `--attention` (how likely the user is to
change it) with a one-line `--why`. `move` keeps provenance.

Deleting or rewording an obligation keeps the version it replaced for 30 days.
`deleted` lists a node's obligations that are gone but restorable, and
`history` lists one obligation's earlier versions (it also accepts the id of
a deleted one). Both list changes as `r-<n>`. `restore r-<n>` puts back the
obligation as it was before that change: a deleted one returns with its id,
position, section, phase and marks, and an edited one gets its earlier wording
back. Pass several `r-<n>`, or `--node` with `--by`, to restore a batch;
deletions come back newest first, so they return to their original order. A
restore can itself be restored: it is recorded like any other edit.

Obligation text can reference any node inline by slug: `[[dynamic-form]]`. A
write naming a slug no node has is refused; `check-refs` lists existing
obligations whose references are broken.

## drafting

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

## plan

Plan steps are the structured, dependency-graph breakdown of how a node's
obligations get implemented — created during the node's `planning` lifecycle
phase, one node at a time. A step's ordinal is display order only; execution
order comes from `depends-on` links.

```
tod-cli --data-root <DATA_ROOT> plan list      --node <NODE_UUID>
tod-cli --data-root <DATA_ROOT> plan show      <STEP_ID>
tod-cli --data-root <DATA_ROOT> plan add       --node <NODE_UUID> --body <TEXT> [--after <STEP_ID>] [--before] [--depends-on <STEP_ID>] [--satisfies <OBLIGATION_ID>]
tod-cli --data-root <DATA_ROOT> plan update    <STEP_ID> [--body <TEXT>] [--status pending|ready|in_progress|implemented|verified|blocked]
tod-cli --data-root <DATA_ROOT> plan delete    <STEP_ID>
tod-cli --data-root <DATA_ROOT> plan depend    <STEP_ID> --on <STEP_ID>
tod-cli --data-root <DATA_ROOT> plan undepend  <STEP_ID> --on <STEP_ID>
tod-cli --data-root <DATA_ROOT> plan satisfy   <STEP_ID> --obligation <OBLIGATION_UUID>
tod-cli --data-root <DATA_ROOT> plan unsatisfy <STEP_ID> --obligation <OBLIGATION_UUID>
tod-cli --data-root <DATA_ROOT> plan ready     --node <NODE_UUID>
```

Step and obligation ids may be given in full or as the 8-character prefix
shown in listings. `satisfy`/`unsatisfy` link a step to the requirement or
constraint it fulfills — use `obligations list --node <NODE_UUID>` to find the
obligation id if you weren't given it. `ready` lists the steps eligible to
start now (status `ready`, or `pending` with every dependency
`implemented`/`verified`).

If you change an obligation and existing plan steps depend on it or claim to
satisfy it, check `plan list --node <NODE_UUID>` for steps whose `satisfies`
links point at it — they may need their body updated, a new step added, or
their status reset to `pending` so the change gets picked up downstream.

More nouns will be added over time. Run `tod-cli --help` or
`tod-cli <noun> --help` to see what the installed version actually supports —
prefer that over assuming a command exists.
