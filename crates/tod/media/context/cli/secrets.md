## `tod-cli secrets`

How you use a credential the user has stored in tod, such as an API key,
without ever seeing it. You never read a secret's value: you name it, and
`run` puts it in the environment of the command you start.

```
tod-cli --data-root <DATA_ROOT> secrets list
tod-cli --data-root <DATA_ROOT> secrets set <NAME> [VALUE]
tod-cli --data-root <DATA_ROOT> secrets run --env <VAR>=<SECRET> [--env <VAR>=<SECRET> ...] -- <COMMAND> [ARGS...]
```

`list` shows each secret tod knows by name (e.g. `linear_api_key`) and whether
it is set. `set` is how the user stores one (e.g. `github_token`) — not
something you run on their behalf; VALUE from stdin when omitted, so it never
sits in shell history. `run` starts `<COMMAND>` with each named secret in
environment variable `<VAR>`; the command's output comes back with every
secret value replaced by `***`, and `run` exits with the command's exit code.
For example, a script that calls Linear's API reads its key from
`LINEAR_API_KEY`:

```
tod-cli --data-root <DATA_ROOT> secrets run --env LINEAR_API_KEY=linear_api_key -- python scripts/fetch_schema.py
```

`list` and `run` also know the credentials and variables defined for this work
(the Environment block of your context lists them; `tod-cli environment`
manages them). `--env GROWTHBOOK_API_KEY=growthbook` works the same for one of
those. If you need one that is not there, ask for it with `tod-cli environment
request`.

In a cloud sandbox `secrets run` is refused: a credential that has hosts (the
Environment block says which) is applied by the sandbox's proxy to every
request to those hosts, so call the API directly (curl, your client library)
without a key. If one is missing, ask with `tod-cli environment request`.

In an autonomous node's cloud sandbox, `github_token` shows as set by the
sandbox's proxy: `run` gives the command a placeholder, which the proxy
replaces with the real token on every request to GitHub.

Write the command to read the variable itself and never print it, write it to
a file, or pass it on as an argument. If the secret you need is not set, or is
not a kind tod stores, `run` fails with what the user has to do; that is
something only the user can unblock.
