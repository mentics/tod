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

The login lasts 15 minutes, which is long enough to connect; a command that
stays connected keeps its session. Run `db` again for the next command. Read
the variables from the environment and never print them, write them to a
file, or pass them as arguments.

It needs the user's machine: it must be on, with tod running. If it is not,
or the user's AWS session has expired, `db` fails and says so; that is
something only the user can fix, so record what you need from the database as
a request for them rather than retrying. Treat the database as production:
read what you need and do not change data unless the work calls for it.
