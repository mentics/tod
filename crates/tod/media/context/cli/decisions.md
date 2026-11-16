## `tod-cli decisions`

What the user answers. Reduce a decision to options wherever you can — the
more it is reduced, the faster the user gets through the queue. Ask for free
text (no `--option`) only when the answers cannot reasonably be listed, such
as what a node with nothing on it is for. You only ask; answering is the
user's.

```
tod-cli --data-root <DATA_ROOT> decisions ask  [--node <NODE_UUID>] <QUESTION> [--option <TEXT> (repeatable)] [--evidence <KIND>:<ID> (repeatable)] [--reason <KIND>] [--moves-to <N>:<STATE> (repeatable)] [--blocks <STEP_ID> (repeatable)] [--new-evidence <TEXT>]
tod-cli --data-root <DATA_ROOT> decisions list [--node <NODE_UUID>] [--all]
tod-cli --data-root <DATA_ROOT> decisions show <DECISION_ID>
```

Inside a conversation `--node` defaults to the node it is about, and an ask
is filed under that conversation and its protocol. Decision ids may be given
in full or as the 8-character prefix listings show.

`ask` records a pending decision:

- The question is one positional argument — quote it. It is the whole request
  as the user reads it: the ask on the first line, a blank line, then the
  background (see "Wording a request" in how you work here). Options go in
  `--option`, never in the question text.
- `--option`: repeatable, in the order they should be offered to the user
  (numbered 1, 2, 3 … in the task panel). The user can always answer in
  their own words instead. With none, the question asks for free text.
- `--evidence`: repeatable `kind:id` links the user can open while
  answering, kind one of `obligation`, `plan_step`, `test_run`,
  `conversation`, `finding`, `node`. An obligation or plan step may be given
  as the 8-character prefix shown in listings; the rest take the full UUID.
- `--reason`: one of `intent`, `missing_rule`, `conflict`, `access`, `risk`,
  `capability`, `broken`, `other` — defaults to `other` when omitted. See "Asking the
  user" for what each means.
- `--moves-to <N>:<STATE>`: repeatable. Option N (1-based, as numbered in
  `--option` order) sends the node back to `STATE` when the user picks it: the
  app moves the node itself, as the user's own move, and when `STATE` is
  `active` or earlier it reopens the node's implementation steps and
  verdicts. An option that offers to send the task back, rework it, or start
  an earlier state over must carry this: nothing else moves the node, and
  your session cannot, so a plain option worded that way does nothing. `STATE`
  must be earlier than the node's current state and before `pr_review`; the
  ask fails otherwise.
- `--blocks`: repeatable plan step id (the 8-character prefix is fine) of this
  node that the question holds up. Use it whenever you hand a step back
  (`plan update --status blocked|partial`) for the same cause: ask once, with
  every step it holds up in `--blocks`, instead of a separate hand-back
  request and a question. The user sees one request with the steps listed
  under it, and answering it opens the steps again (ready) and tells you the
  answer. If the step is handed back after the question, pass
  `plan update <STEP> --status blocked ... --decision <DECISION_ID>` instead.
- `--new-evidence`: a question the user answered on this node in the last 30
  minutes (the same text, or one whose `--blocks` an answer already released)
  is refused, and the message gives the answer: act on it. Pass
  `--new-evidence` only when something really changed since (you did what
  they said and it failed: say the command and the error). Never to repeat
  the question, and never to re-word it.

`list` shows a node's pending decisions, oldest first; `--all` also lists
answered and withdrawn ones. `show` prints one decision and its full answer
log — every answer the user has given it, oldest first, since a change of
mind adds a new answer rather than replacing the old one.

### A change the sandbox cannot make: hand it to the user to run

In a cloud node's sandbox you have no write access to a production system
(a web API such as Flagship's, a database, Redis); this is deliberate and
there is no way around it. When the work needs such a write (a per-org
resync, a retry, a deploy), file a request (`--reason access`) that asks the
user to run it on their machine. Never ask for a token, never try another
route.

- **One call**: put the exact command in the request, ready to paste, with
  every value filled in, and say what it changes and what you will check
  afterwards. Options such as "Done, continue" and "Do not run it".
- **More than one step**: write a script in the sandbox (a file under
  `/tmp`, set to stop on the first error, printing what it does) and test
  what you can without the write. The user copies it from the sandbox and
  runs it, on their own machine from the root of their checkout of the
  repository (`.local/` is not tracked; `mkdir -p` makes it):

```
mkdir -p .local && tod-sandbox cp "$TOD_SANDBOX":/tmp/<SCRIPT> .local/<SCRIPT> && bash .local/<SCRIPT>
```

  Write the request's command with `$TOD_SANDBOX` replaced by your sandbox's
  name (read it from your own `TOD_SANDBOX` environment variable) and
  `<SCRIPT>` by the file name, so the user pastes one line. `tod-sandbox cp
  <name>:<absolute path> <local>` copies one file byte for byte. Tell the user
  what the script does, in a few lines, before they run it: they are
  trusting your code with production. Do not leave a secret in the script.
