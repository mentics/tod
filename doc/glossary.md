**lifecycle config**
A capability that lists, per lifecycle phase, the skills the agent for that phase should use. It is set on a high-level node and inherited by everything below it; a lower node overrides a phase by listing it itself, and an empty list turns that phase's skills off.

**task node**
A node that is defined by having the *lifecycle* capability on itself and an *agent* capability that is inheritable. It is expected that one lifecycle processor is assigned to it to drive it to completion throught he lifecycle phases.