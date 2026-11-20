## `tod-cli guidance`

Which of the project's own instructions a task's change needs: its skills,
the files every session reads (`REVIEW.md`, a touched submodule's
`CLAUDE.md`), the app's review lenses, and the phases' checks. The selection is
put, text and all, into every implement, verify, review and fix session of
the task.

```
tod-cli --data-root <DATA_ROOT> guidance list    [<NODE>]
tod-cli --data-root <DATA_ROOT> guidance select  [<NODE>] --kind skill|file|lens|check --item <ITEM> --reason <TEXT> [--not-applying]
tod-cli --data-root <DATA_ROOT> guidance add     <NODE> --kind skill|file|lens|check --item <ITEM> [--reason <TEXT>]
tod-cli --data-root <DATA_ROOT> guidance remove  <NODE> --kind skill|file|lens|check --item <ITEM>
```

`<NODE>` is the task's slug or full UUID. Inside a guidance selection it
defaults to the task being selected for.

`list` shows the selection: each item with its kind, the reason it was
selected, who selected it (its own triggers, the selector, or the user), and
the stage (`predicted` before the work, `actual` at review, `grown` after a
fix reached new areas). `--all` adds what was dropped or removed.

`select` records one choice, with a one-line `--reason` naming what in the
change (or the design) it matches. Only the guidance selector session may
run it, and only for the candidates it was offered: a skill by its name, a
file by its path, a check as `<phase>/<label>`, and every lens by its slug.
`--not-applying` records a lens as not applying, with the reason; every lens
must be recorded one way or the other. Anyone else is refused.

`add` and `remove` are the user's: an item the user removed is not selected
again until the user adds it back. Refused in an agent session.
