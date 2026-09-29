## `tod-cli capabilities`

Which capabilities a node has (spec, lifecycle, agent, generator, tags, files,
ticket), and each one's settings.

```
tod-cli --data-root <DATA_ROOT> capabilities list    <NODE>
tod-cli --data-root <DATA_ROOT> capabilities enable  <NODE> <CAP>...
tod-cli --data-root <DATA_ROOT> capabilities disable <NODE> <CAP>
tod-cli --data-root <DATA_ROOT> capabilities set     <NODE> agent [--platform claude|cursor] [--model <TEXT>] [--effort <TEXT>]
tod-cli --data-root <DATA_ROOT> capabilities set     <NODE> files [--dir <PATH>] [--branch <TEXT>] [--worktree on|off] [--container <NAME|ID>] [--mounted on|off] [--sandbox image[:<IMAGE>]|fork:<NAME>]
tod-cli --data-root <DATA_ROOT> capabilities set     <NODE> ticket [--ticket <ID>] [--pr <URL>]...
tod-cli --data-root <DATA_ROOT> capabilities set     <NODE> tags (--tags <A,B,..> | --add <TAG> | --remove <TAG>)
tod-cli --data-root <DATA_ROOT> capabilities set     <NODE> generator --source <TYPE> --config <JSON>
```

`<NODE>` is a slug or full UUID. `enable` adds capabilities with their
defaults; generator and lifecycle cannot both be on. `set` changes only the
settings you pass (an empty value clears one; `--pr` replaces the whole
list) and needs the capability enabled first. A node is at most one ticket:
`--ticket` replaces it, and related tickets go in the node's notes.
Refreshing a generator updates every node that is one of its tickets,
wherever that node came from.

`files --container` runs the node's agents, terminals, and git in that
running dev container (`--container ''` moves them back to this machine).
The repository lives in the container, so `--dir` is its path there. With
`--mounted on` the repository is on the host and mounted into the container
instead: `--dir` is the host path, and the directory inside the container
follows from its mounts. `files --sandbox` gives each node working from
these settings a cloud sandbox of its own, made when it first needs one:
`image` (the default image), `image:<IMAGE>`, or `fork:<NAME>` (a copy of
that sandbox); `--sandbox ''` moves them back. The image or forked sandbox
must hold the repository, and `--dir` is its path there. Changing where the
files are is refused while nodes have worktrees or sandboxes made from the
current settings; the user removes those in the app.

`disable` removes the capability **and everything that belongs to it** — a
Spec's obligations and details, a generator's generated nodes. It is archived
and reversible, but only disable a capability when that is what was asked for.
It is refused while a live agent, open shell, or worktree still depends on it.

A node's lifecycle state cannot be set here; only the lifecycle process moves it.
