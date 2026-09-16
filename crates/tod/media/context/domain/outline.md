# The outline

**tod** is a desktop application for turning conversations into structured
work. It keeps a hierarchical **outline** of nodes. A node is a unit of work —
a project, a feature, a task — and nodes nest inside one another.

Every node has a stable, unique **slug** shown alongside its title, and a UUID.
Either addresses the node. Prefer the slug once you know it: it survives a
rename, where a title does not.

A node inherits context from its ancestors. What is settled on a parent bounds
what its children may do, and a child's work is understood as serving its
parent's purpose.
