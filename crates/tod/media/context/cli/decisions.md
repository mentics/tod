## `tod-cli decisions`

What the user answers. Reduce a decision to options wherever you can — the
more it is reduced, the faster the user gets through the queue. Ask for free
text (no `--option`) only when the answers cannot reasonably be listed, such
as what a node with nothing on it is for. You only ask; answering is the
user's.

```
tod-cli --data-root <DATA_ROOT> decisions ask  [--node <NODE_UUID>] <QUESTION> [--option <TEXT> (repeatable)] [--evidence <KIND>:<ID> (repeatable)] [--reason <KIND>] [--moves-to <N>:<STATE> (repeatable)] [--edit-obligation <N>:<OBLIGATION_ID>=<NEW_TEXT> (repeatable)] [--affirm-obligation <N>:<OBLIGATION_ID> (repeatable)] [--blocks <STEP_ID> (repeatable)] [--new-evidence <TEXT>] [--need <TEXT>] [--command <TEXT>] [--error <TEXT>] [--tried <TEXT> (repeatable)] [--after <TEXT>]
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
- `--edit-obligation <N>:<OBLIGATION_ID>=<NEW_TEXT>`: repeatable. Option N
  (1-based) changes that obligation of the node to `NEW_TEXT` when the user
  picks it; the id may be the 8-character prefix. You may not change an
  obligation the user wrote or approved at all, or an agent-written one once
  the phase that wrote it is done (requirements through `proposed`, design
  through `design`): only the user may, so whenever an option changes the
  approach and an obligation names the approach it replaces (a check, a
  mechanism, a name, a number), put the obligation's new text on that option.
  The user sees the edit beside the option, and answering applies it as their
  own edit (recorded in your conversation's change set, where it can be
  reversed), so you never ask
  a second time to "update the obligations to match". Give the full new text
  of the obligation, and the edit to every obligation the option makes stale;
  an option that leaves them true carries none. The edits of the option the
  user did not pick are never made. If an obligation changes before the user
  answers, the answer is refused and says which; ask again then.
- `--affirm-obligation <N>:<OBLIGATION_ID>`: repeatable. Option N (1-based)
  approves that obligation of the node as it stands when the user picks it:
  it becomes the user's (`approved`) and no agent changes or removes it
  afterwards. Use it to have the user confirm a requirement you are unsure of
  without rewording it; use `--edit-obligation` when the text must change.
- `--blocks`: repeatable plan step id (the 8-character prefix is fine) of this
  node that the question holds up. Use it whenever you hand a step back
  (`plan update --status blocked|partial`) for the same cause: ask once, with
  every step it holds up in `--blocks`, instead of a separate hand-back
  request and a question. The user sees one request with the steps listed
  under it, and answering it opens the steps again (ready) and tells you the
  answer. If the step is handed back after the question, pass
  `plan update <STEP> --status blocked ... --decision <DECISION_ID>` instead.
- `--new-evidence`: a request whose cause is already pending or answered on
  this node (in any session, however long ago) is refused: the same question,
  the same `--blocks` steps, or the same `--error`. The message gives the
  existing request's id, or the answer: act on it, or wait for it. Pass
  `--new-evidence` only when something really changed since (you did what
  they said and it failed: say the command and the error). Never to repeat
  the question, and never to re-word it.
- `--need`, `--command`, `--error`, `--tried`, `--after`: only with `--reason
  access`, `broken` or `capability`. The app writes the request from these;
  you do not word it. `--need` is what you need, in one line ("Read access to
  the prod-kb Redis"); `--command` is the exact command for the user to paste,
  every value filled in, when there is one; `--error` is the exact error text
  you got (required for `broken`; it is how the same failure on other tasks is
  recognized); `--tried` is one thing you tried per flag, repeated; `--after`
  is what you will do or check once they are done. The question may be left
  out when `--need` is given. Without `--option`, the choices are "Done,
  continue" and "Do not do it". Give the other flags when you have them and
  leave out the ones you do not.

```
tod-cli decisions ask --reason access --need "Read access to the prod-kb Redis" \
  --command "tod-cli access list" --tried "access call redis-prod scan: not set up" \
  --after "I rerun the scan and continue verification"
```

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

- **One call**: put the exact command in `--command`, ready to paste, with
  every value filled in, what it changes in `--need`, and what you will check
  afterwards in `--after`.
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
  `<SCRIPT>` by the file name, so the user pastes one line (that line is the
  `--command`). `tod-sandbox cp
  <name>:<absolute path> <local>` copies one file byte for byte. Tell the user
  what the script does, in a few lines, before they run it: they are
  trusting your code with production. Do not leave a secret in the script.
