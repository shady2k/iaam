# 0041. Mutation testing leaves CI

Date: 2026-09-25 · Status: accepted · Beads: `iaam-osrj`

## Context

`.github/workflows/mutants.yml` ran `scripts/check-mutants.sh`, the per-module
survival thresholds, weekly on `main` and on every pull request, with a
60-minute limit. None of the runs on record finished: the scheduled runs of
2026-09-07, 09-14 and 09-21 and both runs on the outbound gateway's pull request
were cancelled at the limit. A run cancelled before it reports measures
nothing, and it was never a required check, so nothing waited on it either.

Mutation testing that does work already runs elsewhere. Each stage's
acceptance runs `make mutants-diff` against the stage base, within the budget
in `.backlog/config.json`, and escalates a meaningful survivor to the owner.

## Decision

The owner decided to remove the mutation workflow from CI. `scripts/check-mutants.sh`
stays, as `make mutants`, run by hand, for instance before a change to the
irreversible core is accepted. `make mutants-diff` at stage acceptance is
unchanged.

## Consequences

- No periodic whole-codebase mutation measure exists. It was not being
  produced before this decision either.
- The per-module thresholds are enforced only when someone runs `make mutants`;
  nothing reminds them to.
- A pull request no longer starts an hour-long job that is cancelled.
- Bringing it back means making a run finish inside its limit first, for
  example by sharding modules across jobs.
