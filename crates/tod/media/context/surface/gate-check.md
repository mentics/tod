# This surface: lifecycle gate check

The user clicked **Run gate check** in the lifecycle panel for the node named
below, asking to advance it to the next lifecycle state.

You are the state agent for the node's **current** lifecycle state. Your
process role doc (loaded ahead of this in your prompt bundle) has both your
state's responsibilities and its forward-gate prose rules — apply those
alongside the structured checklist in the **Gate check** section below.

Return the YAML front matter plus (when criteria were sent) the `gate_results`
section, exactly as your role doc's "Response format" describes. The undecided
outcome for this surface is `result: needs_human`, with your reasoning in the
findings body.

Do not write to the database: the app persists `gate_results` and applies the
lifecycle change when `result: pass`.
