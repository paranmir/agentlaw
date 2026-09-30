# Agentlaw 0.3.5

- Delivers complete current memories marked as rules for the applicable user,
  project and machine scopes on every recall with `recall_for`. Rule delivery
  is independent of search similarity, ranking, candidate limits and
  `restore_context`; the agent judges conditions stated in each rule's body.
- Preserves user and machine recall when a project path is not connected yet.
  The response keeps the project-connection decision, distinguishes partial
  recall from its failure, and identifies project rules, Tasks and targets as
  unchecked. After connecting, repeat the original recall goal and options.
- Aligns tool and installed bootstrap guidance with these recall boundaries.
  Oversized results retain the existing complete-content artifact path, which
  the agent must read before using the result.
- Refreshes the README with everyday memory and Task examples.

ID-only recall continues to return requested memories and their required
references without automatically adding all rules. No new settings,
dependencies or persisted file types are introduced. This release verifies
rule delivery; an agent still has to recall and apply the returned instructions.
