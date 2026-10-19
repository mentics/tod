## `tod-cli capabilities`

Which capabilities a node has (spec, lifecycle, agent, generator, tags, files,
ticket), and each one's settings.

```
tod-cli --data-root <DATA_ROOT> capabilities list    <NODE>
tod-cli --data-root <DATA_ROOT> capabilities enable  <NODE> <CAP>...
tod-cli --data-root <DATA_ROOT> capabilities disable <NODE> <CAP>
tod-cli --data-root <DATA_ROOT> capabilities set     <NODE> agent [--platform claude|cursor] [--model <TEXT>] [--effort <TEXT>]
tod-cli --data-root <DATA_ROOT> capabilities set     <NODE> files [--location <NAME|ID>] [--branch <TEXT>]
tod-cli --data-root <DATA_ROOT> capabilities set     <NODE> ticket [--ticket <ID>] [--pr <URL>]...
tod-cli --data-root <DATA_ROOT> capabilities set     <NODE> tags (--tags <A,B,..> | --add <TAG> | --remove <TAG>)
tod-cli --data-root <DATA_ROOT> capabilities set     <NODE> generator --source <TYPE> --config <JSON>
tod-cli --data-root <DATA_ROOT> capabilities set     <NODE> lifecycle-config (--phase <PHASE> --skills <A,B,..> | --clear <PHASE>)
tod-cli --data-root <DATA_ROOT> capabilities set     <NODE> groups --lifecycle <true|false>
```

`<NODE>` is a slug or full UUID. `enable` adds capabilities with their
defaults; generator and lifecycle cannot both be on. `set` changes only the
settings you pass (an empty value clears one; `--pr` replaces the whole
list) and needs the capability enabled first. A node is at most one ticket:
`--ticket` replaces it, and related tickets go in the node's notes.
Refreshing a generator updates every node that is one of its tickets,
wherever that node came from.

`lifecycle-config` sets the skills the agent for a phase uses, for the node and
everything below it. `<PHASE>` is one of proposed, design, planning, implement,
verify, review, fix, pr, merged, released, learn. `--skills` replaces that
phase's list (`--skills ''` means none, even where an ancestor sets some);
`--clear` removes the phase from this node so the nearest ancestor's applies
again. Other phases are untouched.

`groups --lifecycle true` shows the node's children grouped by lifecycle state.
Every child then needs the Lifecycle capability: those without it move down to
the first place that accepts them (the next sibling of the node, and so on up).

`files --location` picks which of the user's Files locations the node, and
every node below it that does not enable Files itself, works from
(`--location ''` picks none). A location is a named setup the user keeps in
the app: a workspace directory, and whether each node gets a worktree, runs in
a dev container, or gets a cloud sandbox. You cannot create or edit them; an
unknown name is refused with the names that exist. `--branch` is this node's
own branch. Picking a different location is refused while nodes have
worktrees or sandboxes made from the current one that the new one would not
make; the user removes those in the app.

`disable` removes the capability **and everything that belongs to it** — a
Spec's obligations and details, a generator's generated nodes. It is archived
and reversible, but only disable a capability when that is what was asked for.
It is refused while a live agent, open shell, or worktree still depends on it.

A node's lifecycle state cannot be set here; only the lifecycle process moves it.
