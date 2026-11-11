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
- **Tracker layout:** the store lives **off the branches** (iaam-ima7z, the
  owner's decision of 2026-11-11): one copy per machine, a linked worktree of
  this repository detached at the ref `refs/tracker/state`, parked in the
  machine's state directory (`$XDG_STATE_HOME/<main checkout's name>/tracker`,
  `~/.local/state/iaam/tracker` here). Every checkout and worktree reaches it
  through its `.beads` link (ignored by `.gitignore`), so `br` anywhere on the
  machine reads and writes the one store. No code branch carries a tracker
  record: the pre-commit block `IAAM TRACKER OFF BRANCHES` refuses anything
  staged under `.beads`. The ref carries the items (`issues.jsonl`) and the
  tracker's own settings (`config.yaml` with `claim_exclusive`, `policy.yaml`
  with this project's statuses); the database is each machine's, rebuilt from
  them. The tracker at a code revision is the recording the store made under
  that revision's own name when it was cut (post-commit) or landed
  (post-merge), never a code tree and never a later snapshot; a revision with
  no recording is refused. History: until 2026-09-27 the tracker was bd with
  Dolt (iaam-r1v7); until 2026-11-11 its export was committed on `main` in
  `chore(beads)` commits, so a revision from then is read from the
  `.beads/issues.jsonl` it committed — that layout's own record of it. The last
  such revision, `dd64694d`, was also recorded on the ref from its tree.
- **Workflow ownership:** `br` is the one authority for task status; the sync
  mechanism is the one authority for publishing it. `br` manages no hooks and no
  agent context: the hooks are composed in `.git/hooks` by
  `make backlog-connect`, and the agent docs carry the tracker's rules
  (AGENTS.md).
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
- **Feature agreement:** three lines in the feature epic's description, which
  the adapter carries as `body`: `Agreed direction: <time> — "<owner's words>"`,
  `Design: <the decision record>` (or `Design: tailored past — <reason>`) and
  `Agreed plan: <time> — "<owner's words>"`, written with `br update <id>
  --description`. The config's `agreementExempt` lists the features already
  under way on 2026-11-11, when the rule was adopted.
- **Implemented:** the `br` status `implemented`, declared in the store's
  `policy.yaml`, set by the coordinator with the transition comment
  `implemented: <merge revision> -- <checks run>`. Neither ready nor closed.
- **Submitted:** the status `submitted`, with the comment
  `submitted: <branch>@<revision> -- <local evidence>`; the worker's hold is
  cleared. Resumes at integration. The adapter reads the latest such comment;
  for revisions from the bd era it reads bd's `shady2k_*` metadata instead.
- **Commit task links:** the task id in parentheses in the message, as this
  repository always did: `Carry the blocked quantity (iaam-1u7b)`, or
  `(iaam-a, iaam-b.2)`. Only ids inside parentheses count, so crate names in
  prose are not read as tasks. The id must be an existing leaf (closed counts);
  an epic is refused. A merge with no id carries the links of the commits it
  brings in; a revert keeps the reverted subject and its link. One parser
  (`taskIds` in `.backlog/adapter.mjs`) feeds both the commit check and the
  gate's `landed` list.
- **Cleanup recovery:** the 2026-09-25 grooming deferred 194 issues to
  2026-10-26 and labelled the 20 kept ones. Snapshots before and after, and a
  field-level journal (`rollback-2026-09-25.json`), are in `.git/shady2k/` of
  the owner's clone and are not committed: they hold the whole tracker. To
  restore an issue, compare its current fields with the journal's `after` and
  only then set `before` (`br undefer`, `br update --remove-label`,
  `br update --parent`), so later work is not overwritten. The move off the
  branches kept the database the old layout left in
  `.git/shady2k/beads-before-move-2026-11-11/`.
- **Rollback of the move to br:** bd's database is intact on the remote
  (`refs/dolt/data`, pushed 2026-09-27 before the move) and its local files,
  final export and memories are in `.git/shady2k/bd-archive-2026-09-27/` of the
  owner's clone. Fields br does not keep (bd metadata, `started_at`, `spec_id`)
  are on each item that had them, as a comment by `migration-from-bd`.

### Checks and execution

- **Charter outcomes:** none: there is no charter yet, so the export carries no
  `outcomes` and the outcome rules report nothing.
- **Backlog adapter:** `node .backlog/adapter.mjs [--offline]` reads the live
  store through `tracker-sync.mjs export` (a sync with the remote first;
  `--offline` says it read this machine's store only); `--at <rev>` reads the
  recording of `<rev>` (`tracker-sync.mjs export --at`), or, for a revision
  from before 2026-11-11, the export that revision committed; `--jsonl <file>`
  reads any beads-format export. The `source` line ends in `@ <revision>`. A
  status the project does not use is refused (exit 2); tombstones are dropped.
- **Rules:** `.backlog/rules/check.mjs` with `time-format.mjs` beside it,
  `check-commits.mjs`, `check-docs.mjs`, `check-present.mjs` with
  `document-format.mjs` beside it, and `check-product.mjs`, with
  `regular-file.mjs` and `argv.mjs` beside every check: byte-for-byte copies of
  shady2k-skills 0.143.0 (setup 0.68.0),
  `skills/backlog/setup-shady2k-skills/`. Their `--version` is the installation.
- **Present documents:** `make present` (`node .backlog/rules/check-present.mjs
  --config .backlog/config.json --base <rev>`; `BASE` defaults to the merge base
  with `origin/main`), run when a pull request is opened. It
  refuses a change that leaves a present document naming a path the change
  removed, and reports older drift without refusing (filed: iaam-6xl64). The
  documents, areas and ignores are in the config. Personal scope: CI does not
  run it.
- **Product documents:** `node .backlog/rules/check-product.mjs --staged` in the
  pre-commit hook; `--rev <revision>` by hand. The repository has no product
  documents yet, so it passes untouched.
- **Work records:** a run's claims, receipts and stops are `br` comments whose
  text starts with `[shady2k-time`, and the command runner's journal starts with
  `<!-- shady2k-exec v1 -->`. The export carries every comment; the adapter
  passes those raw and whole as `comments` (`id` is br's comment id as a
  string, `at` its `created_at`, `author`, `body` its text), damaged or not. The
  run script reads `--backlog` from `node .backlog/adapter.mjs` written to a
  file, so a record another machine published is there once synced. A record
  is posted unchanged with `br comments add <id> -f <file>` holding exactly
  what the script printed. `timeRecordsExempt` was adopted empty on 2026-09-27.
- **Document adapter and gate:** not installed yet: "Install the document gate:
  specs and acceptance evidence checked at each transition" (iaam-46x4).
- **Document policy / baseline / evidence:** not applicable until iaam-46x4.
  Evidence level when installed: records (trusted, not verified); CI enforces
  nothing in personal scope. The tracker is never part of an evidence revision:
  nothing in the tree stands for it.
- **Main line for the gate:** the adapter's `landed` list: every commit on
  `main` (or `origin/main`) that the judged revision contains, from local
  history, parsed by the commit-link convention. A commit of the old layout
  that changed only `.beads/` (and at most `.backlog/config.json`) recorded
  items rather than delivering them, and is left out.
- **Backlog gate:** `make backlog` (`node .backlog/gate.mjs [--base <rev>]
  [--offline] [--no-recover]`): the live store, read at this checkout's `HEAD`
  and passed to `check.mjs --at <HEAD>`, against the store's recording at
  `<rev>` (default `HEAD`) and `.backlog/config.json` as committed at `<rev>`,
  under the config's strength. Before judging it runs `runs.mjs gaps` from the
  newest installed shady2k-skills plugin and posts every recoverable receipt of
  an ended span with `br comments add`; a recovery that fails is said and the
  gate still judges.
- **JSON report:** `node .backlog/gate.mjs --json`.
- **Commit-link input and check:** `node .backlog/commits.mjs --message-file <f>`,
  `--range <base>..<head>`, or `--push <remote>` (ref lines on stdin). Task ids
  resolve against the live store, never a code tree. Each commit carries the
  paths it changed and the config's `codePaths`, which is empty: the code rule
  stays off until the document gate (iaam-46x4) can admit a change record, the
  owner's decision of 2026-11-11. An empty range fails.
- **Local entry points** (all in `.git/hooks`, acting only where `git config
  iaam.backlog` is `on`): `pre-commit` — privacy guard, backlog gate, tracker
  off branches, product documents; `commit-msg` — commit links; `pre-push` —
  the links of every commit the push introduces; `post-commit`, `post-merge`,
  `post-checkout` — the store records the revision's state, and a checkout of a
  pre-move branch is relinked to the store. The worktree sweep stays on
  `post-merge`.
- **Connecting a clone:** `make backlog-connect` (`.backlog/connect.sh`). It
  checks node (18+), git, br, systemctl and every file a hook reads; points git
  at `.git/hooks`; runs `tracker-sync.mjs connect --all` (fetches the store's
  ref whole, rebuilds this machine's database, links every checkout and
  worktree, composes the record hooks); installs and starts the user service
  `iaam-tracker-sync` and proves it is running; composes the project's hooks;
  and runs the gate once. Safe to rerun; it refuses with what is missing.
- **CI:** none for these checks (personal scope). Pushing `refs/tracker/state`
  starts no code CI: GitHub's push event fires for branches and tags only, and
  this repository's run list after the first push showed none.
- **Push ranges:** `pre-push` runs `commits.mjs --push`: per pushed ref, the
  commits of its tip (a tag peeled) that no ref the remote has reaches; a remote
  ref whose object this clone lacks is skipped; a ref that introduces nothing
  says "introduces no commits: nothing to check"; no ref lines is refused.
- **Tracker synchronization:** the mechanism `.backlog/rules/tracker-sync.mjs`
  with the recipe `.backlog/rules/trackers/br.mjs` and `argv.mjs` one level up,
  copied from shady2k-skills 0.143.0 (`tracker-sync.version`), with the unit
  template `.backlog/supervisors/tracker-sync.service`. The supervisor is the
  systemd user service `iaam-tracker-sync`
  (`~/.config/systemd/user/iaam-tracker-sync.service`, written by
  `make backlog-connect`), running `tracker-sync.mjs watch --interval 10
  --debounce 2 --max-age 5`, restarted always, kept across logout and reboot by
  linger. A change is published within 2–5 s of the write; the store fetches
  and merges the remote every 10 s; a push is bounded at 10 s, a sync fetch at
  20 s, a recipe command at 120 s (the default `--recipe-timeout`). The merge is
  per item, never by text; the same field changed on two sides takes br's order
  for the field, then the order of publication on the ref, never a clock, and
  records only ever append. A claim (`tracker-sync.mjs claim`) counts once it is
  on the remote: of two simultaneous claims one lands and the other exits 3.
  Unreachable remote: writes continue in this store and are published on its
  return; a read that needs the remote says it is unreachable (exit 4).
  `tracker-sync.mjs status [--expect-version <v>]` says how it stands and
  whether this copy is older than the set.
- **Jev:** the owner consented on 2026-09-29, route `openrouter`, sure 0.9 on
  jev-1.13. The key's place is this machine's
  (`~/.config/shady2k-skills/jev.json`), never the repository's. The config's
  `idPattern` masks item ids but not crate names (`iaam-http` stays), and
  `maskPatterns` adds card- and account-number shapes. Masking goes by shape: a
  counterparty's name or an amount in a session transcript is not masked, so
  text known to carry the owner's statement data is not sent. Check with
  `jev.mjs status --config .backlog/config.json` from the set's skill directory.
- **Owner pushes:** the run script's `notify` reads the owner's channel from
  `~/.config/shady2k-skills/notify.json` (Bark, this machine's); the config's
  `notifyPushes` is on: the owner confirmed a test push on 2026-11-11.
- **Bulk-edit age correction:** `gate.mjs` passes `--ages-from` and
  `--ages-through` from the two `.git/shady2k/` snapshots when the clone has them.
- **Static checks:** `make fmt lint arch privacy skill-doc`.
- **Related tests:** `nix develop -c cargo nextest run -p <crate>` for each crate
  the change touches and each crate that depends on it.
- **Full stage checks:** `make check`, `make diff-lint BASE=origin/main` and
  `make diff-coverage BASE=origin/main` (inside `nix develop`). `make check`
  includes neither of the other two, and CI runs both: diff coverage refuses a
  branch whose added lines are less than 90% covered. The diff lint refuses new `allow(...)`, `expect(...)`, `#[ignore]` and `todo!` in
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
- **Landing:** `github-flow` (the owner's decision of 2026-11-11): a branch per
  change, a pull request for each, merged by the run once this project's review
  and CI are green; no approval is required (the main line is unprotected), and
  a run may push.
- **Parallel execution:** one git worktree per worker (`make sweep` retires merged
  ones), up to the config's `maxWorkers`; each reaches the one store through its
  `.beads` link. Workers are **omp agents in herdr worktrees**
  (`herdr worktree create`, `herdr agent start --kind omp`), the owner's choice
  of 2026-09-27; they never touch the tracker. Claims are exclusive and land on
  the remote (`tracker-sync.mjs claim`); the stage's coordinator merges and
  records `implemented`.

### Tracker operations

The tracker's own reference is `br robot-docs guide` and `br <command> --help`;
only what this protocol adds is listed. `MECH` below is
`node .backlog/rules/tracker-sync.mjs <command> --recipe .backlog/rules/trackers/br.mjs`.
No operation runs `br sync`: the sync owns import and export.

| operation | project implementation |
| --- | --- |
| create | `br create -t task\|bug\|epic --parent <id> -l <milestone>,<area> --acceptance ... --silent "<title>"`; a feature's epic carries `## Success Criteria` or `--acceptance` |
| publish | the user service `iaam-tracker-sync`: on a database change, after 2 s of quiet or 5 s at most, commits the store's state to `refs/tracker/state` and pushes it; a refused push fetches, merges and pushes again. Proof: `MECH status`, `systemctl --user status iaam-tracker-sync` |
| sync | the same service, every 10 s: fetch the ref, merge per item, rebuild what changed in the database; a read through the adapter syncs first |
| link / unlink | `br dep add <needs> <produces>` (`blocks`) with the reason in a comment; provenance is `discovered-from`, which the adapter never reads as a dependency |
| claim | `MECH claim --item <id> --holder '<harness>-<role>:<person>@<machine>:<branch>#<session>'` (fetch, merge, write, push; exit 3 when another holder landed first: read again and take other work), then `br update <id> --status in_progress` and the record `runs.mjs claim` prints. It takes a same-stage dependant of an implemented prerequisite too, with the edge kept |
| release | `br update <id> --status open --assignee ''`; submitted and implemented work keeps its status |
| implemented | coordinator: `br update <id> --status implemented --assignee '' --transition-comment 'implemented: <rev> -- <checks>'` |
| submitted | worker: `br update <id> --status submitted --assignee '' --transition-comment 'submitted: <branch>@<rev> -- <evidence>'` |
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
| at a revision | `node .backlog/adapter.mjs --at <rev>` (`MECH export --at <rev>`) |
| authorized sync | the code: push the branch and open its pull request; the tracker needs no step |

When a skill reports the installation is out of date, run `setup-shady2k-skills`. An explicit
setup invocation rechecks everything even if its recorded version matches.
