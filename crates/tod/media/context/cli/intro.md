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

## Finding a command

The sections that follow document only the commands you are most likely to
need here. `tod-cli` does much more: nodes' capabilities and their settings,
notes, review findings, verdicts, and so on. To find one:

```
tod-cli help <WORDS>        # every command that mentions the words, e.g. `tod-cli help lifecycle`
tod-cli <noun> --help       # a noun's full syntax and rules
tod-cli --help              # every noun
```

Search before concluding something cannot be done, and before asking the user
to do it in the app. An option a command does not know is ignored with a
warning on stderr, not applied, so heed that warning.
