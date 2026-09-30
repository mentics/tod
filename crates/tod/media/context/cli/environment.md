## `tod-cli environment`

The variables and credentials the user has defined for this work. The
"Environment" block of your context lists them: variables with their values,
credentials by name and description, never by value. If a credential you need
is not listed, or is listed as not set, ask for it; do not hunt for it on disk
or in the environment.

```
tod-cli --data-root <DATA_ROOT> environment list [--node <NODE>]
tod-cli --data-root <DATA_ROOT> environment presets
tod-cli --data-root <DATA_ROOT> environment request <NAME> --why <TEXT> [--preset <ID>] [--host <URL|HOST>] [--description <TEXT>] [--node <NODE>]
tod-cli --data-root <DATA_ROOT> environment add-secret <NAME> [--preset <ID>] [--host <URL|HOST>] [--auth bearer|header:<NAME>|basic:<USER>] [--env-var <VAR>] [--description <TEXT>] [--test-url <URL>] [--node <NODE>]
tod-cli --data-root <DATA_ROOT> environment set-variable <NAME> <VALUE> [--env-var <VAR>] [--description <TEXT>] [--node <NODE>]
tod-cli --data-root <DATA_ROOT> environment set-secret <NAME> [VALUE] [--node <NODE>]
tod-cli --data-root <DATA_ROOT> environment test <NAME> [--node <NODE>]
tod-cli --data-root <DATA_ROOT> environment remove <NAME> [--node <NODE>]
```

Use a variable as it is (it is in your process environment). Use a credential
with `tod-cli secrets run --env <VAR>=<name> -- <command>`: the command gets
the value, you never do. In a cloud sandbox, a credential with hosts needs no
`secrets run`: the sandbox's proxy adds it to every request to those hosts, so
call the API directly.

- `list` shows what is defined, including what was inherited from ancestors.
- `presets` lists the services tod knows how to set up.
- `request` is how you ask the user for a credential: it adds the credential,
  unset, and asks them to provide it. Say in `--why` what you need it for. Pass
  `--preset` when one fits the service (`environment presets`), or `--host` and
  `--description` when none does. A credential always needs the host it is
  used with (a preset supplies it); one without a host is refused. Carry on with whatever does not need it.
- `add-secret` and `set-variable` define an entry on the node; Environment
  must be enabled there. They are for when the user has asked you to set
  something up.
- `set-secret` stores a value, reading it from stdin. It is the user's to run,
  not yours: never ask for a credential's value in chat and never write it
  anywhere.
- `test` makes the service's harmless test request with the stored value and
  says whether it was accepted.
- `remove` deletes an entry defined on this node.
