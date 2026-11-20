# Reading and changing data: `tod-cli`

You do **not** have direct database access, and you should not try to open the
database file yourself. Use the `tod-cli` command instead. It goes through the
same validated code path the application UI uses, so it cannot leave the data
in an inconsistent state.

`tod-cli` is installed next to the app's executable. Every invocation needs the
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
`review` `guidance` `suggest` `pr` `verdicts` `incoming` `learn` `lifecycle` `secrets` `db` `access` `environment` `decisions` `triage` `phase`
`wait` `batch` `questions` `memory` `interview` `visual-design` `journey` `skills` `lessons`

For a noun's commands and rules run `tod-cli <noun> --help`; to find a command
by topic run `tod-cli help <WORDS>` (e.g. `tod-cli help lifecycle`). Search
before concluding something cannot be done, and before asking the user to do
it in the app. An option a command does not know is ignored with a warning on
stderr, not applied, so heed that warning.

If you are asked to do something you do not know how to do, `tod-cli skills`
lists skills for working with the app itself (for example, reviewing a journey)
and `tod-cli skills show <name>` reads one.

If the work needs to look at a production system (a database, Hatchet runs,
Redis keys, a service's API) and you are in a cloud node's sandbox, the way to
get that access is the user's machine, through the `db` and `access` nouns
(`tod-cli access list` shows what the user has set up). They hold the
credentials so you never do. Try them before you record a request asking the
user for a credential, a token, a VPN or a command to run for you.
