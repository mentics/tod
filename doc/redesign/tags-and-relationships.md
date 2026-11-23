# Tags and relationships

## REQ-TAG-01 · Explain effective obligations

Users can tag any addressable entity, including product components, tasks, sessions, rules, and built-in definitions. A tag can organize items, link to a collection of reusable items, or apply behavior through explicit rules. The tag inspector shows **Applied to**, **Includes**, and **Effects** so users can see what a tag labels, what it contains, and what applying it will do.

For example, **Keyboard-first** includes [CON-KEYBOARD-01](product-definitions.md#con-keyboard-01--operate-every-component-by-keyboard). An effect applies that constraint to product components that carry the tag. Both the tag assignment and the source of the effective constraint appear on **Query bar**. Users can preview effects when tagging one item, a marked selection, or an entire result set.

Each automatic tag assignment shows its source. Imports and deterministic rules may assign tags. Agents can suggest a tag with a reason, and users can accept, correct, or suppress the suggestion. A tag whose effect changes obligations is presented as a change to those obligations before it takes effect unless the user has configured a rule to apply it automatically.

**Acceptance** — Preview applying **Keyboard-first** to two components. Inspect the affected items and obligations, apply it, and navigate from the resulting constraint back to its source tag. Conflicting constraints are visible and can be resolved explicitly. Changes to a shared tag's effects show which existing items would be affected.

**Open** — Whether existing work follows new versions of shared constraints automatically, and when a removed tag should leave an obligation in place on active work.

## REQ-RELATIONSHIP-01 · Define a relationship when linking items

A user can create a relationship type from the link picker and reuse it elsewhere. A type can have a name, a reverse label, and optional restrictions on what it can connect. Follow both directions of a link in an inspector and filter results through the same relationship. The picker supports linking multiple targets in one action.

Starter types can make common work easier. A type such as `Blocked by` can have a rule that pauses a transition until its targets are resolved. A similarly named custom type can be descriptive. A type's effect is explicit, inspectable, and editable separately from its label. Agents can propose new types and links using the same definitions people use.

**Acceptance** — From **Query bar**, define `Shares interaction with` and link it to **Command palette**. Find the link from both endpoints and use it as a query criterion. Create a dependency with two targets and inspect why it blocks a transition. A later rename keeps existing links attached to the same type.

**Open** — Whether the first release should support live query targets for rule-bearing relationships. An explicit selection is a workable default for dependency targets. If a link can follow a saved query, show that membership and rule results can change when the query returns new items.
