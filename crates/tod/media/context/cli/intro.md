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

The sections that follow document the nouns you need for this particular job.
`tod-cli` has others. Run `tod-cli --help` or `tod-cli <noun> --help` to see
what the installed version actually supports — prefer that over assuming a
command exists.
