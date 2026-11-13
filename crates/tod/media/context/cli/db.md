## `tod-cli db`

How you use one of the user's databases from a cloud node's sandbox, such as a
production database that only lets the user's own machine in. You never hold
the login: the user's machine signs a short-lived one for you, `run` hands it
to the command you start, and the command's output comes back with it masked.

```
tod-cli --data-root <DATA_ROOT> db list
tod-cli --data-root <DATA_ROOT> db run <NAME> -- <COMMAND> [ARGS...]
tod-cli --data-root <DATA_ROOT> db psql <NAME> [-- <PSQL ARGS...>]
```

`list` shows the databases the user has set up for you, by name. `run` starts
`<COMMAND>` with the connection in the standard `PG*` environment variables
(`PGHOST`, `PGPORT`, `PGUSER`, `PGDATABASE`, `PGPASSWORD`, `PGSSLMODE`,
`PGSSLROOTCERT`), so `psql`, `pg_dump`, and any libpq-based client or script
connect with no other settings, and exits with the command's exit code.
`psql <NAME>` is `run <NAME> -- psql`. For example:

```
tod-cli --data-root <DATA_ROOT> db psql prod -- -c "select count(*) from parts"
tod-cli --data-root <DATA_ROOT> db run prod -- python scripts/check_counts.py
```

A project script that logs in to a database itself, such as `db-query` for a
production database (it signs an IAM login with the aws CLI), does not work in
a sandbox: there is no aws CLI and no AWS session, and the user does not need
to be asked for one. When a database below has the name the script takes,
`db-query <NAME> "<sql>"` here is the app's own, which logs in through `db` and
returns the script's JSON; otherwise run the same SQL with `db psql <NAME>` or
`db run`. Never stop to ask for the aws CLI, and keep to any rule the project
sets for production (read-only, EXPLAIN first, asking before a costly query):
`db` only provides the login.

The login lasts 15 minutes, which is long enough to connect; a command that
stays connected keeps its session. Run `db` again for the next command. Read
the variables from the environment and never print them, write them to a
file, or pass them as arguments.

It needs the user's machine to be on, with the app running. When it is not
connected for a moment (the orchestrator restarted, a network gap), `db` waits
and asks again by itself, for a few minutes: do not retry it yourself. If it
still fails with "has not been connected", that is a failure of the app's own
connection, not something to ask the user about: stop and report it as a
failure with the error. Only an expired AWS session of the user's is theirs to
fix; `db` says so, and then you record what you need from the database as a
request for them. Treat the database as production:
read what you need and do not change data unless the work calls for it.
