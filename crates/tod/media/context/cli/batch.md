## `tod-cli batch`

Many commands in one call. When you have more than a few changes to make
(tens of obligations, plan steps, or nodes), send them as one script instead
of one tool call each.

```
tod-cli --data-root <DATA_ROOT> batch run [--file <PATH>]
```

The script is read from stdin (a heredoc) or `--file`. Each line is one
ordinary command, written as it would be after `tod-cli --data-root <DATA_ROOT>`:

```
tod-cli --data-root <DATA_ROOT> batch run <<'BATCH'
$a = node create --title "Login" --parent root
node create --title "Logout" --parent $a
obligations add --node $a --kind req --body <<EOT
Sessions expire after 30 minutes.
EOT
plan add --node $a --title "Add session timeout"
node list --parent $a
BATCH
```

- The whole script is checked first. If it is malformed (an unclosed quote or
  heredoc, an unknown `$name`), nothing runs and you get one error.
- Then every line runs independently, in order. Each that can be applied is.
  The reply is a summary (`batch: 97 applied, 3 failed`) followed by only the
  failed lines, each with its line number and the command's own error, and the
  output of lines that read (`list`, `show`), each headed by its line number.
- **Do not resend the script.** What applied is applied. Fix the failed lines
  and send just those in a new batch.
- `$name = command` binds what the command created (a node's slug); later
  lines use `$name` as a whole argument. If that line failed, the lines using
  it are reported as skipped.
- Quote values with "double" or 'single' quotes. Multi-line text goes in a
  heredoc value: end the line with `<<TAG`, then the text, then a line holding
  only `TAG`. A `-` value (stdin) is not allowed.
- `batch` and `secrets` cannot be run inside a batch.
