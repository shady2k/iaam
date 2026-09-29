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
- **Tracker layout:** the store is `br` (beads_rust, 0.7+): one SQLite database
  per machine under `.beads/`, which `br` resolves to the main checkout's even
  from a worktree, and its JSONL export `.beads/issues.jsonl`. Until 2026-09-27
  it was bd with Dolt (iaam-r1v7); the export format is the same, so revisions
  from before read alike. The export is committed on `main` only, in
  `chore(beads): ...` commits: the pre-commit block `IAAM TRACKER LAYOUT` refuses
  it staged on any other branch, so a feature branch never carries the tracker.
  `br` never runs git: publishing is `br sync --flush-only`, a commit on `main`
  and a push. The tracker at a code revision is that export as committed at it
  (`adapter.mjs --at <rev>`); transitions over a range are the difference
  between the exports at its two ends. Known limit: an export committed on
  `main` still carries the states of runs not landed yet (a claim, a submitted
  or implemented leaf on another feature's branch). `br sync --merge` is never
  used to catch up: it tombstones what the export lacks; `--reconcile-additive`
  is (AGENTS.md, The tracker: br).
- **Workflow ownership:** `br` is the one authority for task status. `br` manages
  no hooks and no agent context: the hooks in `.beads/hooks` are this project's,
  and the agent docs carry the tracker's rules (AGENTS.md).
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
- **Implemented:** the `br` status `implemented`, declared in `.beads/policy.yaml`,
  set by the coordinator with the transition comment
  `implemented: <merge revision> -- <checks run>`. Neither ready nor closed.
- **Submitted:** the status `submitted`, with the comment
  `submitted: <branch>@<revision> -- <local evidence>`; the worker's hold is
  cleared. Resumes at integration. The adapter reads the latest such comment;
  for revisions from the bd era it reads bd's `shady2k_*` metadata instead.
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
  only then set `before` (`br undefer`, `br update --remove-label`,
  `br update --parent`), so later work is not overwritten.
- **Rollback of the move to br:** bd's database is intact on the remote
  (`refs/dolt/data`, pushed 2026-09-27 before the move) and its local files,
  final export and memories are in `.git/shady2k/bd-archive-2026-09-27/` of the
  owner's clone. Fields br does not keep (bd metadata, `started_at`, `spec_id`)
  are on each item that had them, as a comment by `migration-from-bd`.

### Checks and execution

- **Backlog adapter:** `node .backlog/adapter.mjs` (live: `br sync --flush-only`,
  then the export `br where` names); `--at <rev>` reads `.beads/issues.jsonl`
  as committed at a revision; `--jsonl <file>` reads any beads-format export.
  A status the project does not use is refused (exit 2); tombstones are dropped.
- **Rules:** `.backlog/rules/check.mjs` with `time-format.mjs` beside it,
  `check-commits.mjs`, `check-docs.mjs`, and `check-present.mjs` with
  `document-format.mjs` beside it, byte-for-byte copies of shady2k-skills
  0.73.0 (setup 0.37.0), `skills/backlog/setup-shady2k-skills/`. Their
  `--version` is the installation.
- **Present documents:** `make present` (`node .backlog/rules/check-present.mjs
  --config .backlog/config.json --base <rev>`; `BASE` defaults to the merge base
  with `origin/main`), run when a pull request is opened. It
  refuses a change that leaves a present document naming a path the change
  removed, and reports older drift without refusing. The documents, areas and
  ignores are in the config. Personal scope: CI does not run it.
- **Work records:** a run's claims, receipts and stops are `br` comments whose
  text starts with `[shady2k-time`. The export carries every comment; the
  adapter passes those raw and whole as `comments` (`id` is br's comment id as
  a string, `at` its `created_at`, `author`, `body` its text), damaged or not. The run
  script reads `--backlog` from `node .backlog/adapter.mjs` written to a file.
  A record is posted unchanged with `br comments add <id> -f <file>` holding
  exactly what the script printed. `timeRecordsExempt` in the config was
  adopted empty on 2026-09-27: no work was in flight unclaimed.
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
- **Connecting a clone:** `make backlog-connect`. It checks node (18+), br and
  every file the hooks read, builds br's database from the export when the
  clone has none, points `core.hooksPath` at `.beads/hooks`, runs `make hooks`,
  adds the three blocks, sets `iaam.backlog=on` and runs the gate once; it
  refuses with what is missing and disconnects again if the gate cannot run.
- **CI:** none for these checks (personal scope).
- **Push ranges:** no pre-push hook and no CI run the range checks here, so
  the rule for what a push introduces has no entry point yet. The commit-link
  check runs per commit in `commit-msg`; `commits.mjs --range` is run by hand.
- **Jev:** the owner consented on 2026-09-29, route `openrouter`. The key's
  place is this machine's (`~/.config/shady2k-skills/jev.json`), never the
  repository's. The config's `idPattern` masks item ids but not crate names
  (`iaam-http` stays), and `maskPatterns` adds card- and account-number
  shapes. Masking goes by shape: a counterparty's name or an amount in a
  session transcript is not masked, so text known to carry the owner's
  statement data is not sent. Check with `jev.mjs status --config
  .backlog/config.json` from the set's skill directory.
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
  ones), up to the config's `maxWorkers`. Workers are **omp agents in herdr
  worktrees** (`herdr worktree create`, `herdr agent start --kind omp`), the
  owner's choice of 2026-09-27; they never touch the tracker. Claims are atomic
  and exclusive (`claim_exclusive` in `.beads/config.yaml`) through
  `br update <id> --claim --actor '<full agent name>'`; the stage's coordinator
  merges and records `implemented`.

### Tracker operations

The tracker's own reference is `br robot-docs guide` and `br <command> --help`;
only what this protocol adds is listed.

| operation | project implementation |
| --- | --- |
| create | `br create -t task\|bug\|epic --parent <id> -l <milestone>,<area> --acceptance ... --silent "<title>"`; a feature's epic carries `## Success Criteria` or `--acceptance` |
| link / unlink | `br dep add <needs> <produces>` (`blocks`) with the reason in a comment; provenance is `discovered-from`, which the adapter never reads as a dependency |
| claim | `br update <id> --claim --actor '<harness>-<role>:<person>@<machine>:<branch>#<session>'` (atomic, exclusive; sets assignee and `in_progress`), then the record `runs.mjs claim` prints. A same-stage dependant of an implemented prerequisite is blocked to br: claim it with `--claim --force --actor ...`, which stays exclusive and keeps the edge |
| release | `br update <id> --status open --assignee '' --actor ...`; submitted and implemented work keeps its status |
| implemented | coordinator: `br update <id> --status implemented --assignee '' --transition-comment 'implemented: <rev> -- <checks>' --actor ...` |
| submitted | worker: `br update <id> --status submitted --assignee '' --transition-comment 'submitted: <branch>@<rev> -- <evidence>' --actor ...` |
| reopen | `br update <id> --status open --transition-comment 'reopened: <why>'`, then recheck dependants |
| close | `br close <id> --reason ...` after stage acceptance; cancellation or duplicate says so in the reason |
| comment / edit | `br comments add <id> ...`, `br update`; a work record with `br comments add <id> -f <file>`, the file exactly as the run script printed it, never reflowed or edited |
| defer / undefer | `br defer <id> --until <date>` (the reason in a comment); `br undefer <id>` |
| milestone / label | `br update <id> --add-label` with values from the config |
| ready | `node .backlog/ready.mjs [--stage <id>] [--checkout <rev>]`: open unheld leaves whose prerequisites are closed, or implemented in the same stage with the recorded revision contained in the checkout. `br ready` alone never releases a dependant of an implemented prerequisite |
| holds | `br list --status in_progress` |
| pending integration / acceptance | `br list --status submitted` / `br list --status implemented` |
| children | `br show <id>` lists them (`<- … (parent-child)`), every status; or the adapter's export filtered on `parent` |
| search / show | `br search`, `br show <id>`, `br list -l <area>` |
| publish | on `main`, after the gate is clean: `br sync --flush-only`, commit `.beads/issues.jsonl` as `chore(beads): ... (<task>)`, `git push` |

When a skill reports the installation is out of date, run `setup-shady2k-skills`. An explicit
setup invocation rechecks everything even if its recorded version matches.
