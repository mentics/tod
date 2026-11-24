## `tod-cli lessons`

The improvement lessons the app records about its own process: requests that
should not have been asked, send-backs and disputes, quick fixes that
escalated, tool failures and the user's remarks. The process review reads them
and drafts changes.

```
tod-cli --data-root <DATA_ROOT> lessons list [--status <STATUS>] [--source <SOURCE>]
tod-cli --data-root <DATA_ROOT> lessons show <LESSON_ID>
tod-cli --data-root <DATA_ROOT> lessons stats [--days <N>]
tod-cli --data-root <DATA_ROOT> lessons add --node <NODE> --text <TEXT> [--category <CATEGORY>] [--target <TEXT>]
tod-cli --data-root <DATA_ROOT> lessons decline <LESSON_ID>
tod-cli --data-root <DATA_ROOT> lessons apply [<LESSON_ID>] [--lesson <LESSON_ID> (repeatable)] --ref <TEXT>
```

- `list` shows the lessons, oldest first. `--status` is `new`, `grouped`,
  `applied` or `declined`; `--source` is `triage`, `send_back`, `incident`,
  `ladder` or `user`.
- `show` prints one lesson in full, with where it came from. The 8-character
  prefix of its id is enough.
- `stats` prints the measures of the improvement loop (requests by reason and
  asking protocol, who settled them, triage's rates, time to answer, cost at
  list prices, incidents with their `auth` failures by environment (this machine,
  dev container or sandbox) and how many came within 30 minutes of that
  environment starting, send-backs by target phase). `--days` limits it to
  the recent past. It also says which figures the data cannot give.
- `add` records a lesson in the user's own words about the task `--node`.
  `--category` is one of `asker_could_decide`, `app_should_handle`,
  `upstream_unclear`, `brief_unclear`, `necessary` or `upstream_wrong` (default
  `asker_could_decide`); `--target` names what it points at.
- `decline` is the user's: the lesson is not acted on. It is refused in an
  agent session and for a lesson already applied.
- `apply` marks lessons acted on, by the commit (or task) in `--ref`. Only the
  process review's session may use it, and only for the lessons its own run
  took in.
