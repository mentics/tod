## `tod-cli secrets`

How you use a credential the user has stored in tod, such as an API key,
without ever seeing it. You never read a secret's value: you name it, and
`run` puts it in the environment of the command you start.

```
tod-cli --data-root <DATA_ROOT> secrets list
tod-cli --data-root <DATA_ROOT> secrets run --env <VAR>=<SECRET> [--env <VAR>=<SECRET> ...] -- <COMMAND> [ARGS...]
```

`list` shows each secret tod knows by name (e.g. `linear_api_key`) and whether
it is set. `run` starts `<COMMAND>` with each named secret in environment
variable `<VAR>`; the command's output comes back with every secret value
replaced by `***`, and `run` exits with the command's exit code. For example,
a script that calls Linear's API reads its key from `LINEAR_API_KEY`:

```
tod-cli --data-root <DATA_ROOT> secrets run --env LINEAR_API_KEY=linear_api_key -- python scripts/fetch_schema.py
```

Write the command to read the variable itself and never print it, write it to
a file, or pass it on as an argument. If the secret you need is not set, or is
not a kind tod stores, `run` fails with what the user has to do; that is
something only the user can unblock.
