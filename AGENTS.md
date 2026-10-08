# Agent Instructions

## Backlog workflow

All retained work and commits belong to tracked tasks. File discoveries through
`to-backlog`; implement through `take-task`; close only after stage acceptance
through `close-out`. Read Backlog integration in `docs/backlog-integration.md`
before writes. When a skill reports the installation is out of date, run
`setup-shady2k-skills`.

## The tracker: `br`

The backlog lives in **`br`** (beads_rust): one SQLite database per machine plus
the tracked export `.beads/issues.jsonl`. Filing, taking and closing work go
through the skills above; the commands below are how the tracker works, not a
way around them. `bd` (beads with Dolt) was this project's tracker until
2026-09-27 and is not used any more.

```bash
br ready --json                              # what can start (see ready.mjs below)
br show <id>                                 # one item, with comments
br update <id> --claim --actor "<agent>"     # atomic, exclusive claim
br close <id> --reason "..."                 # after stage acceptance only
br sync --flush-only                         # db -> .beads/issues.jsonl
```

**`br` never runs git.** Nothing commits, pushes or pulls the backlog for you.
The export is committed **on `main` only**, in `chore(beads): ...` commits: the
pre-commit hook refuses `.beads/issues.jsonl` staged on any other branch, so a
feature branch carries what it delivers and never the tracker. `br` resolves
the database of the **main checkout** even from a worktree, so every worktree
sees one tracker. A `git pull` that brings a newer export is imported by the
next `br` command on its own.

**Publish every backlog write promptly** — on `main`: `br sync --flush-only`,
commit `.beads/issues.jsonl`, push. An unpublished item exists for nobody else.

**Never run `br sync --merge` to catch up.** It tombstones every issue the
database holds and the export does not — measured in a sibling project on
2026-09-11: 70 records killed by one run after a fast-forward. For a database
that is merely ahead, `br sync --reconcile-additive` adds without deleting. A
tombstone is not undone by `br update`; the repair is to rebuild the lines from
`git show`, `br delete --hard` the ids, and `br sync --import-only`.

**Always claim with `--actor`**, the agent's full name
(`<harness>-<role>:<person>@<machine>:<branch>#<session>`); without it `br`
records the person. `claim_exclusive` refuses a claim while another actor holds
the item. A same-stage dependant of an `implemented` prerequisite is still
blocked to `br`: claim it with `--claim --force --actor ...`, which stays atomic
and keeps the edge. `br ready` alone does not know that case: the integration's
`node .backlog/ready.mjs --stage <id>` does.

**`submitted` and `implemented` are statuses of this project**
(`.beads/policy.yaml`), set with the evidence bound to the transition:
`br update <id> --status implemented --transition-comment "implemented: <rev> -- <checks>"`.

**TodoWrite, TaskCreate and markdown TODO lists are forbidden.** `br` is the
tracker for all work, including your own checklists.

A lesson every agent must follow goes into **Lessons** below, through a commit
someone reads; what you personally worked out stays in your own session notes.

## Session completion

1. File what remains through `to-backlog`.
2. Run the quality gates if code changed.
3. Update item status: close accepted work, release what you hold and did not finish.
4. On `main`: `br sync --flush-only`, commit `.beads/issues.jsonl`, `git push`
   (this repository opts in to committing and pushing; see `CLAUDE.md`).
5. Hand off: changes, validation, item status, and any blocked step with its exact error.

## Non-Interactive Shell Commands

**ALWAYS use non-interactive flags** with file operations to avoid hanging on confirmation prompts.

Shell commands like `cp`, `mv`, and `rm` may be aliased to include `-i` (interactive) mode on some systems, causing the agent to hang indefinitely waiting for y/n input.

**Use these forms instead:**
```bash
# Force overwrite without prompting
cp -f source dest           # NOT: cp source dest
mv -f source dest           # NOT: mv source dest
rm -f file                  # NOT: rm file

# For recursive operations
rm -rf directory            # NOT: rm -r directory
cp -rf source dest          # NOT: cp -r source dest
```

**Other commands that may prompt:**
- `scp` - use `-o BatchMode=yes` for non-interactive
- `ssh` - use `-o BatchMode=yes` to fail instead of prompting
- `apt-get` - use `-y` flag
- `brew` - use `HOMEBREW_NO_AUTO_UPDATE=1` env var

## Import Tools

The owner's data is imported by ordinary Python 3 scripts in `tools/`, not by
anything vendor-specific. They use only the standard library, take every input as
an explicit argument, and are run directly:

| Tool | What it moves |
|---|---|
| `tools/tbank-csv-import/import.py` | a T-Bank operations export into iaam operations |
| `tools/actual-budget-migrate/migrate_categories.py` | an Actual Budget category list into iaam categories |

Read `tools/README.md` first, then the tool's own `README.md`. Those files are
the only copy of the rules; `.claude/skills/` holds pointers to them so that
Claude Code lists them as skills, and holds no script and no fixture. Do not
duplicate either — two copies of an importer drift silently.

**The owner's data never enters the repository.** The agent may run these
tools on his files when he hands them over (see "The owner's data never enters
the repository" in `CLAUDE.md`), but account maps, counterparty maps and database
paths are run-time arguments living outside this repository; never commit one,
and never point a script at real data to check that it works. Each tool's
fixtures are invented end to end and are what you test against.

## Lessons

What earlier sessions paid for, kept where every agent reads it. Each entry
stays only while its cause does: a change that removes the cause removes the
entry in the same commit. Moved here from the tracker's memory store on
2026-09-27.

### Tracker items

- **Title:** one sentence naming the defect or the goal, with no type or
  priority prefix; those are fields.
- **Substance goes in the description**, never only in notes: search reads the
  description, and evidence kept in notes is what produced duplicate items here.
  Notes are a running log.
- **Criteria go in `--acceptance`**, not a heading typed into the description; a
  bug also carries `## Steps to Reproduce` in its description.
- **Labels are only the config's vocabulary** (`.backlog/config.json`); never a
  label for what a field already says (type, parent, priority).
- **Defer, do not block, on an epic or on work not filed yet:** a dependency
  names a real task.

### What a worker's check must include

- `cargo clippy --workspace --all-targets`, `cargo nextest run --workspace` and
  `./scripts/check-architecture.sh`. The last runs in seconds and catches what
  clippy and the tests pass: a crate depending on a higher layer, and checked
  arithmetic in the shell crates, where every number comes from core. A type
  both the store and ingest consume belongs in `iaam-core`.
- A crate subset is not enough: `SCHEMA_VERSION` of `iaam-core` is pinned in
  `crates/iaam-server/tests/contract.rs`.
- `cargo check -p` does not build test targets: a task touching `EventKind` runs
  the tests of every crate with integration tests on that variant.
- A brief for a change to a public type or signature lists files by where the
  type is **used** (grep the constructors), not only where it is defined, and
  separates mechanical adaptation of a test (allowed) from changing what it
  asserts (not allowed).
- A plan's code blocks name only functions that exist: grep each name before
  the plan is handed over.

### Domain traps

- **A new `EventKind` variant breaks seven exhaustive dispatchers**
  (`event/kind.rs` discriminant and `flow_endpoints`, `event/mod.rs`
  `validate_structure`, `projection/lots.rs` `LotBook::apply`,
  `iaam-app` `jobs.rs` `active_instruments` and `scenarios/classification.rs`
  `subject`, `iaam-ingest` `classification.rs` `classification_of`). Three need
  a decision and corrupt figures silently when guessed: `flow_endpoints`,
  `classification_of` and `subject`. An event that moves money but is not the
  owner's decision (amortisation, say) returns `None` from both classifiers.
  Adding the variant is one task, or the workspace stays red for several commits.
- **Migration `0008_quotation_basis`** gives every older observation the basis
  `unknown` with an empty `basis_evidence`. That is a valid unproven record, not
  a defect: a check that rejects the empty evidence breaks every migrated
  database, and tests seed fresh rows, so they do not see it.
- **One document, one `SourceId`** for all its events in reconciliation tests,
  or grouping by document falls apart silently (`crates/iaam-core/tests/support/mod.rs::TestChannel`).
- **A defective past fact is found by the defect's own shape**, never by parser
  version or source channel: versions are bumped by sibling work, and a
  channel's `SourceId` names the currently active broker access.
- **A broker-channel fact is not repaired by importing it again:** the new
  reading differs by fingerprint and is inserted beside the old one. Repair is a
  reversal or a maintenance entry point.
- **MOEX ISS gives no cursor and no count**, so completeness is proved by
  structure (`crates/iaam-market/src/schedule/completeness.rs`): a closed chain
  of coupon periods, a tail matching the last principal repayment, repayment
  shares summing to exactly 100%. The second is the only one that catches a
  truncated page.
- **utoipa 5 registers nested schemas transitively:** removing a type from
  `components(schemas(...))` does not remove it from the document while another
  schema reaches it, and nothing in `contract.rs` checks for dangling `$ref`.
- **T-Invest's sandbox needs its own token** (a production token gets
  401/40003 there); the sandbox returns no coupons, dividends or taxes, and the
  token expires three months after last use. `GetBrokerReport` is the reports
  channel, not a second independent channel.

### Tests and tools

- **A literal moment in a test is a time bomb** when it is compared with
  `now_utc()`: build it as `OffsetDateTime::now_utc() + Duration`. Before
  touching code, `grep -rn 'datetime!(20' crates/*/tests/`. A red test on a clean
  tree is checked against `git stash` before blaming your change.
- **A SQLite `STRICT` primary key is implicitly `NOT NULL`:** for a key with an
  optional column use a `UNIQUE INDEX` over `ifnull(col, '')`.
- **Never copy a live iaam database with `cp`:** it runs in WAL mode. Use
  `sqlite3 <db> ".backup <dest>"`, or checkpoint first and move `.db`, `-wal`
  and `-shm` together.
- **`scripts/check-mutants.sh` writes per module** under
  `target/mutants/<module>/mutants.out/`; the survivors are
  `cat target/mutants/*/mutants.out/missed.txt`. Judge a run by its exit code
  and those files, never by the tail of its output.
- **Every checkout builds into the main checkout's `target/`**: the dev shell
  sets `CARGO_TARGET_DIR` from git's common directory (a value you set wins),
  turns incremental compilation off, and the dev profile keeps only line
  tables of debug information (iaam-eoji9). Builds running at once wait on
  cargo's lock; that is the price. A full disk still shows up as an opaque
  rustc exit 101: remove merged worktrees (`make sweep`) and, when
  `target/` itself has grown, `cargo clean` costs one full rebuild.
- **`POLICY_CHANGE_APPROVED` goes back to `0` as soon as the pull request it
  was set for is merged** (`gh variable set POLICY_CHANGE_APPROVED --body 0`).
  While it is `1`, CI lets any branch change a quality-policy file; it was once
  left at `1` for two days after a merge.
