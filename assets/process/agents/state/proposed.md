# State: `proposed`

**Gate:** `proposed` → `design`. The app checks it: the node has at least one requirement of its own, and the phase is certified.

## Your work

Turn whatever the node says it is for into requirements-phase obligations: the concrete task this node exists to do.

1. Read the node's title, details, and any ticket text, its own obligations, and what it inherits.
2. If they say what the node is for, write the requirements that capture it with the `obligations` noun: specific enough to act on, each one checkable. Fill in the node's details when it has none.
3. If they do not say what the node is for (a title alone, a placeholder, a ticket with no body), **ask the user** through the `decisions` noun: a free-text question, `intent` reason, such as "What is this node for? Nothing on it says what to build." When their answer comes back, write the requirements from it. Ask a follow-up only for what the answer left genuinely open, with options where you can list them.

- **Inherit, do not duplicate.** Record only node-specific requirements and exceptions. Never copy or paraphrase inherited obligations here.
- Obligations are in effect as soon as they are written; never ask the user to confirm them.
- Anything about *how* it gets built stays a requirement as the user said it; design refines it.
- Never invent product intent. A node with no stated purpose gets a question, not guessed requirements.

## Done when

- [ ] The node's own requirements define a real, concrete task: someone could start designing it without asking what it is for.
- [ ] Each requirement is specific enough to check whether it was met.
- [ ] No two requirements conflict, and none conflicts with an inherited obligation.
- [ ] No requirement restates an ancestor's obligation, and none duplicates a sibling's cross-cutting one. Where the same concern appears on several siblings, it is on their common ancestor instead.

Nothing else. "The user said to start design" is not a condition: the runner takes the node on as soon as the task is defined.
