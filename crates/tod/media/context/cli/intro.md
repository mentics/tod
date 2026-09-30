# Reading and changing data: `tod-cli`

You do **not** have direct database access, and you should not try to open the
database file yourself. Use the `tod-cli` command instead. It goes through the
same validated code path the application UI uses, so it cannot leave the data
in an inconsistent state.

`tod-cli` is installed next to the tod executable. Every invocation needs the
data root, which is given to you in the context section below:

```
tod-cli --data-root <DATA_ROOT> <noun> <command> [options]
```

Add `--json` to any read command when you want to parse the result rather than
read it.

Any text flag (`--body`, `--why`, `--detail`) takes `-` to read its text from
stdin, for long or multi-line text passed with a heredoc. Only one flag per
command can read stdin.

## Many changes at once

Do not make one call per change when you have many. `tod-cli batch run` takes
a script of commands, one per line, and reports only the ones that failed;
see `tod-cli batch --help`.

## Nouns

Only some nouns are documented below. All of them:

`node` `obligations` `plan` `content` `capabilities` `changeset` `tests`
`review` `pr` `verdicts` `incoming` `learn` `secrets` `environment` `decisions` `phase`
`wait` `batch` `questions` `memory` `interview` `visual-design`

For a noun's commands and rules run `tod-cli <noun> --help`; to find a command
by topic run `tod-cli help <WORDS>` (e.g. `tod-cli help lifecycle`). Search
before concluding something cannot be done, and before asking the user to do
it in the app. An option a command does not know is ignored with a warning on
stderr, not applied, so heed that warning.
