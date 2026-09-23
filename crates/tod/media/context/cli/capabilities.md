## `tod-cli capabilities`

Which capabilities a node has (spec, lifecycle, agent, generator, tags, files,
ticket), and each one's settings.

```
tod-cli --data-root <DATA_ROOT> capabilities list    <NODE>
tod-cli --data-root <DATA_ROOT> capabilities enable  <NODE> <CAP>...
tod-cli --data-root <DATA_ROOT> capabilities disable <NODE> <CAP>
tod-cli --data-root <DATA_ROOT> capabilities set     <NODE> agent [--platform claude|cursor] [--model <TEXT>] [--effort <TEXT>]
tod-cli --data-root <DATA_ROOT> capabilities set     <NODE> files [--dir <PATH>] [--branch <TEXT>] [--worktree on|off] [--container <NAME|ID>] [--mounted on|off] [--container-dir <PATH>]
tod-cli --data-root <DATA_ROOT> capabilities set     <NODE> ticket [--ticket <ID>]... [--pr <URL>]...
tod-cli --data-root <DATA_ROOT> capabilities set     <NODE> tags (--tags <A,B,..> | --add <TAG> | --remove <TAG>)
tod-cli --data-root <DATA_ROOT> capabilities set     <NODE> generator --source <TYPE> --config <JSON>
```

`<NODE>` is a slug or full UUID. `enable` adds capabilities with their
defaults; generator and lifecycle cannot both be on. `set` changes only the
settings you pass (an empty value clears one; `--ticket`/`--pr` replace the
whole list) and needs the capability enabled first.

`files --container` runs the node's agents, terminals, and git in that
running dev container (`--container ''` moves them back to this machine).
The repository lives in the container, so `--dir` is its path there. With
`--mounted on` the repository is on the host and mounted into the container
instead: `--dir` is the host path, and `--container-dir` names the directory
inside the container when its mounts do not say.

`disable` removes the capability **and everything that belongs to it** — a
Spec's obligations and details, a generator's generated nodes. It is archived
and reversible, but only disable a capability when that is what was asked for.
It is refused while a live agent, open shell, or worktree still depends on it.

A node's lifecycle state cannot be set here; only the lifecycle process moves it.
