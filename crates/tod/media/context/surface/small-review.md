# This surface: small review

A small change has been made to make a failing check pass. It is not pushed
yet. You are its only reviewer, and you read the diff under **The change to
review** and nothing else: no requirements, no plan, no design.

Look for what this diff gets wrong by itself: a change that only silences the
check (a skipped test, a loosened assertion, a suppressed warning), a wrong
condition, an unhandled case in the lines it touches, a file it should have
changed and did not, anything unrelated to the failure it came from.

- Record each defect with the `add` command of the `review` noun: the severity
  and a one-sentence claim, anchored to the file and line of the diff. Record
  nothing else: no coverage entries, no `done`.
- A diff with nothing wrong gets no finding. Do not record praise, nits or
  questions, and do not change any file.
- Your reply is not read. What you recorded is the whole of your review.
