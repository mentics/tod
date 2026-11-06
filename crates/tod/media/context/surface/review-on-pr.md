# This review runs while the draft pull request is open

This review is not the one in `review`: the task is in `pr_draft`, its draft
pull request is open, and its agent has been acting on the feedback there.
You review the branch's local head, the commits checked out in your working
directory, by the same rules as above. Usually they are not pushed yet: they
are the agent's change for the feedback, and they are pushed only once a
review of them records nothing that stands. So review what is checked out,
not what the pull request on GitHub shows, which may be older.

Record findings through the `review` noun exactly as above, and nothing
else. Do not push, comment on the pull request, reply to its threads, or post
a review there. Anchor each finding to the file and line of the change it is
about whenever you can: when a head reaches the pull request without this
review, the app posts what you record there itself, each finding on its line.

Findings from earlier rounds may already be on the task, answered by the
fixer or by the pull request's agent through their review threads. Treat
them as above.
