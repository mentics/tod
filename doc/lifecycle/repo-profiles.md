# Repository lifecycle profiles

Status: design, not built.

The lifecycle assumes one way of shipping: PR, human review, merge, then a
GitHub release that names the PR. Some repositories ship differently. The
first alternative is an infrastructure repository applied with Atlantis,
where the change is applied *before* it merges. A **profile** says how a
repository ships; the default profile is today's behavior, unchanged.

## Setting

`tod.yml` gains a list keyed by GitHub `owner/repo`; edited in Settings →
Lifecycle. A repository not listed uses `default`.

```yaml
lifecycle:
  repo_profiles:
    - repo: acme/infra
      profile: atlantis
```

## Which profile applies

A profile belongs to a **pull request**, resolved from the PR's own
`owner/repo` (`NodePr`). A node touching a submodule has a PR in the
submodule's repository, so it picks up that repository's profile with no
extra detection: `fleet::repositories::node_repositories` already yields the
`owner/repo` of the superproject and of each submodule.

A node with PRs under different profiles advances only when **every** linked
PR has met its own profile's criteria for the transition.

## What a profile owns

The 13 lifecycle states and `next_lifecycle` stay fixed (no schema change).
A profile owns, per linked PR:

1. the **babysit** step in `pr` once the PR is ready for review
   (`autopilot::babysit`),
2. the **waits** after approval (`autopilot::github_wait::hold_for_github`),
3. the derived **criteria** for `pr → approved`, `approved → merged`, and
   `merged → released` (`gate::derived`),
4. the **labels** the UI shows for those states.

`default` wraps the existing code. Everything up to "ready for review"
(draft, review bots, threads, checks, up-to-date) is shared by all profiles.

## `atlantis`

State meanings (the states are reused, the labels differ):

| State | Default | Atlantis |
|---|---|---|
| `pr` | PR open, babysat until ready | same, then post `atlantis plan` and wait for the plan result |
| `approved` | human review done | human review done (plan succeeded) |
| `merged` | PR merged | **applied**: plan + apply commands run, apply succeeded |
| `released` | release names the PR | **PR merged** (apply came first) |

Flow once the PR is ready for review:

1. Comment `atlantis plan`. Poll the PR's comments until Atlantis reports the
   plan succeeded or failed. A failure is *work* for the `Pr` agent.
2. Wait for the human review (unchanged).
3. Read the plan and apply commands out of Atlantis's "auto run is disabled"
   comments (they carry the parameter naming the plan), post them, and poll
   until each reports success. A failed apply is work for the agent; polling
   is a matter of minutes.
4. Merge (the app or the `Pr` agent, as today), then `released`.

Atlantis's comment format and its author are `pr_readiness`-style settings
(bot login, command prefix) rather than constants.

## Implementation order

1. `RepoProfile` trait + `default` wrapping current behavior; resolve it from
   `NodePr` owner/repo; settings + Settings UI. Behavior unchanged.
2. Route `babysit`, `hold_for_github`, and the three transitions' derived
   criteria through the profile.
3. `atlantis`: comment parsing in `tod_core` (pure, tested against recorded
   comment fixtures), plan/apply steps, profile-specific criteria and labels.
4. Docs: `phase-agents.md` and `pr-readiness.md` point here.
