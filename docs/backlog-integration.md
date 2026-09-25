## Backlog integration

Maintained by `setup-shady2k-skills`. The protocol ships with the skills;
project facts and verified commands live here. Changing choices live only in
the config. No installation state is recorded: the installed checks' versions
are the repository's installation, and each person's plugin and hooks are theirs.

- **Config:** `.backlog/config.json`; read current values there.
- **Scope:** personal. The files are committed, but the hooks act only in a
  clone that ran `make backlog-connect`, and CI enforces nothing of this.
- **Vision, roadmap and charters:** none yet. `README.md` describes what the
  system does today; the current milestone has no charter until `/to-milestone`
  writes one. Status comes from the tracker, never from a document.
- **Current specifications:** `docs/api/conventions.md` and the OpenAPI document
  the server publishes (`GET /v1/openapi.json`) for the API contract;
  `docs/import-boundary.md`, `docs/irreversible-core.md`, `docs/deployment.md`.
  There is no capability catalogue; coverage of behaviour outside the API
  contract is unknown and is read from the code and tests.
- **Changes:** design specs in `.internal/specs/`, implementation plans in
  `.internal/plans/`, linked from their epic's description. Short deltas live on
  the task itself.
- **Document resources:** none installed; the document gate is not installed.
- **Tracker layout:** the store is beads' Dolt database, outside every branch
  (synced through `refs/dolt/data`). Its export, `.beads/issues.jsonl` with
  `.beads/interactions.jsonl`, is committed on `main` only, in `chore(beads): ...`
  commits: the pre-commit block `IAAM TRACKER LAYOUT` refuses either file staged
  on any other branch, so a feature branch never carries the tracker. The
  tracker at a code revision is that export as committed at it
  (`adapter.mjs --at <rev>`); transitions over a range are the difference
  between the exports at its two ends. Known limit: an export committed on
  `main` still carries the states of runs not landed yet (a claim, a submitted
  or implemented leaf on another feature's branch): its `implemented` states
  point at their recorded revision, not at `main`. At the end of a session the
  export is committed on `main`, never on the working branch.
- **Workflow ownership:** beads is the one authority for task status. The
  beads-superpowers plugin and its hooks stay as they are; this installation
  adds blocks outside the beads markers and changes none of beads' own.
- **Architecture and explorations:** `scripts/check-architecture.sh` guards the
  dependency direction; the crate docs describe the rest.
- **Glossary and decisions:** `docs/glossary-ru-en.md`; `docs/decisions/`
  (index in `docs/decisions/INDEX.md`, numbers never reused).
- **Acceptance records:** a `Stage accepted` comment on the stage's issue:
  base and final revisions, included tasks, criteria, test, mutation and review
  evidence, and pending limitations.
- **Features and stages:** beads `epic`. A feature is a root epic wearing the
  milestone label; a stage is an epic under it. A stage's coordinator holds it
  (assignee) while it is being integrated.
- **Implemented:** beads status stays `open`; metadata `shady2k_state=implemented`,
  `shady2k_revision=<merge revision>`, `shady2k_evidence=<checks run>`, set by the
  coordinator with `bd update <id> --set-metadata ...`. Neither ready nor closed.
- **Submitted:** the same, with `shady2k_state=submitted` and the result's
  branch and revision; the worker's hold is cleared. Resumes at integration.
- **Commit task links:** the task id in parentheses in the message, as this
  repository always did: `Carry the blocked quantity (iaam-1u7b)`, or
  `(iaam-a, iaam-b.2)`. Only ids inside parentheses count, so crate names in
  prose are not read as tasks. The id must be an existing leaf (closed counts);
  an epic is refused. A tracker export (`chore(beads): ...`) names the task
  whose change it records; closing an epic names its last leaf. A merge with no
  id carries the links of the commits it brings in; a revert keeps the
  reverted subject and its link.
- **Cleanup recovery:** the 2026-09-25 grooming deferred 194 issues to
  2026-10-26 and labelled the 20 kept ones. Snapshots before and after, and a
  field-level journal (`rollback-2026-09-25.json`), are in `.git/shady2k/` of
  the owner's clone and are not committed: they hold the whole tracker. To
  restore an issue, compare its current fields with the journal's `after` and
  only then set `before` (`bd undefer`, `bd update --remove-label`,
  `bd update --parent`), so later work is not overwritten.

### Checks and execution

- **Backlog adapter:** `node .backlog/adapter.mjs` (live, through `bd export`);
  `--at <rev>` reads `.beads/issues.jsonl` as committed at a revision;
  `--jsonl <file>` reads any beads export.
- **Rules:** `.backlog/rules/check.mjs`, `check-commits.mjs`, `check-docs.mjs`,
  byte-for-byte copies of shady2k-skills 0.59.1 (setup 0.28.0),
  `skills/backlog/setup-shady2k-skills/`. Their `--version` is the installation.
- **Document adapter and gate:** not installed yet: "Install the document gate:
  specs and acceptance evidence checked at each transition" (iaam-46x4).
- **Document policy / baseline / evidence:** not applicable until iaam-46x4.
  Evidence level when installed: records (trusted, not verified); CI enforces
  nothing in personal scope.
- **Backlog gate:** `make backlog` (`node .backlog/gate.mjs [--base <rev>]`):
  the live tracker against `.beads/issues.jsonl` and `.backlog/config.json` as
  committed at `<rev>`, default `HEAD`, under the config's strength.
- **JSON report:** `node .backlog/gate.mjs --json`.
- **Commit-link input and check:** `node .backlog/commits.mjs --message-file <f>`
  or `--range <base>..<head>`, which builds the normalized input and runs
  `check-commits.mjs`. An empty range fails.
- **Local entry points:** `.beads/hooks/pre-commit` (backlog gate after the
  privacy guard, then the tracker layout) and `.beads/hooks/commit-msg` (commit
  links), all in blocks outside the beads markers, acting only when `git config iaam.backlog` is `on`.
- **Connecting a clone:** `make backlog-connect`. It checks node (18+), bd and
  every file the hooks read, runs `make hooks`, adds the three blocks, sets
  `iaam.backlog=on` and runs the gate once; it refuses with what is missing and
  disconnects again if the gate cannot run.
- **CI:** none for these checks (personal scope).
- **Bulk-edit age correction:** `gate.mjs` passes `--ages-from` and
  `--ages-through` from the two `.git/shady2k/` snapshots when the clone has them.
- **Static checks:** `make fmt lint arch privacy skill-doc`.
- **Related tests:** `nix develop -c cargo nextest run -p <crate>` for each crate
  the change touches and each crate that depends on it.
- **Full stage checks:** `make check` and `make diff-lint BASE=origin/main`
  (inside `nix develop`). `make check` does NOT include the diff lint, which CI
  runs: it refuses new `allow(...)`, `expect(...)`, `#[ignore]` and `todo!` in
  lines the branch adds, and any change to a quality-policy file (scripts/,
  .github/workflows, deny.toml, clippy.toml, flake.*, rustfmt.toml, root
  Cargo.toml, tests/fixtures) unless the PR is labelled `policy-change`, the
  change is justified in its task, and the owner has set the repository
  variable `POLICY_CHANGE_APPROVED=1`. It reads committed history, so run it
  after committing. CI takes about ten to twelve minutes.
- **Mutation checks:** `make mutants-diff BASE=<stage base>`, within the config's
  budget; over budget or with a meaningful survivor, escalate to the owner.
- **Reviewer:** Codex (another model), through its MCP server or a herdr worker;
  if neither starts, an independent Claude reviewer, disclosed as same-model.
- **Parallel execution:** one git worktree per worker (`make sweep` retires merged
  ones), up to the config's `maxWorkers`, dispatched as herdr worker sessions.
  Claims are atomic through `bd update <id> --claim` with `BEADS_ACTOR` set to the
  agent's full name; the stage's coordinator merges and records `implemented`.

### Tracker operations

The tracker's own reference is `bd prime` and `bd <command> --help`; only what
this protocol adds is listed.

| operation | project implementation |
| --- | --- |
| create | `bd create -t task\|bug\|epic --parent <id> -l <milestone>,<area> --acceptance ...`; a feature's epic carries `## Success Criteria` or `--acceptance`. A child inherits its parent's labels: pass `--no-inherit-labels` when its area differs, or it carries two areas and the gate refuses it |
| link / unlink | `bd dep add <needs> <produces>` (`blocks`) with the reason in a comment; provenance is `discovered-from`, which the adapter never reads as a dependency |
| claim | `BEADS_ACTOR='<harness>-<role>:<person>@<machine>:<branch>#<session>' bd update <id> --claim` (atomic; sets assignee and in_progress), then a comment with start time and checkout path. A same-stage dependant of an implemented prerequisite is claimed the same way: beads does not refuse claims on blocked issues, and the edge stays |
| release | `bd update <id> --status open --assignee ''`; implemented and submitted metadata stays |
| implemented | coordinator: `bd update <id> --status open --assignee '' --set-metadata shady2k_state=implemented --set-metadata shady2k_revision=<rev> --set-metadata shady2k_evidence='<checks>'` |
| submitted | worker: the same with `shady2k_state=submitted` and the result's branch and revision |
| reopen | `bd update <id> --unset-metadata shady2k_state --unset-metadata shady2k_revision --unset-metadata shady2k_evidence`, then recheck dependants |
| close | `bd close <id> --reason ...` after stage acceptance; cancellation or duplicate says so in the reason |
| comment / edit | `bd comments add`, `bd update` |
| defer / undefer | `bd defer <id> --until <date> --reason ...`; `bd undefer <id>` |
| milestone / label | `bd update <id> --add-label` with values from the config |
| ready | `node .backlog/ready.mjs [--stage <id>] [--checkout <rev>]`: open unheld leaves whose prerequisites are closed, or implemented in the same stage with the recorded revision contained in the checkout. Not `bd ready`, which offers submitted and implemented leaves again |
| holds | `bd list --status in_progress` |
| pending integration / acceptance | `node .backlog/adapter.mjs` and filter status `submitted` / `implemented` |
| children | `bd children <id>` (every status) |
| search / show | `bd search`, `bd show <id>`, `bd list -l <area>` |
| publish | `bd dolt push` after the gate is clean |

When a skill reports the installation is out of date, run `setup-shady2k-skills`. An explicit
setup invocation rechecks everything even if its recorded version matches.
