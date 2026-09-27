# Project Instructions for AI Agents

This file provides instructions and context for AI coding agents working on this project.

## Backlog workflow

All retained work and commits belong to tracked tasks. File discoveries through
`to-backlog`; implement through `take-task`; close only after stage acceptance
through `close-out`. Read Backlog integration in `docs/backlog-integration.md`
before writes. When a skill reports the installation is out of date, run
`setup-shady2k-skills`.

## The tracker

The backlog lives in **`br`** (beads_rust). How it works here — the export on
`main` only, claims with `--actor`, the `br sync --merge` trap, session
completion — is in `AGENTS.md`, **The tracker: `br`**, and the lessons earlier
sessions paid for are in `AGENTS.md`, **Lessons**. Read both before tracker
writes. `bd` is not used any more.

## Active Agent Profile: team-maintainer

**This repository opts in to the team-maintainer profile.** Agents working here
may, without asking first:

- close tracker items and run the quality gates;
- `git commit`;
- `git push`, including the tracker export on `main`.

What still holds:

- A current instruction wins. If the user says "do not commit" or "do not push"
  in this session, that beats this section.
- Authority to commit is not authority to commit anything: branch first when the
  change does not belong on the default branch, and never commit work you have
  not read.
- If a push or sync fails, stop and report the exact command and its error rather
  than working around it.

## Build & Test

_Add your build and test commands here_

```bash
# Example:
# npm install
# npm test
```

## Architecture Overview

_Add a brief overview of your project architecture_

## Conventions & Patterns

### The owner's data never enters the repository

**Nothing that identifies the owner or his money is committed.** Not in code,
not in tests, not in fixtures, not in specs, plans, ADRs, skill documentation or
commit messages.

That means, concretely: no account name or number, no card number, no
counterparty name, no merchant he actually paid, no balance, and no amount taken
from a real statement. Not even as an illustration in prose, and not even in
`.internal/`, which is tracked like everything else.

Two rules follow, and they are the ones that get broken:

- **A fixture is invented.** Test data is written from scratch — `Main`,
  `Savings`, `Shop One` — never trimmed down from a real export. A file derived
  from real rows carries real rows.
- **A worked example is described by its shape, not its value.** "A counterparty
  who is in fact the owner's own account at another bank" belongs in a design;
  the name and the sum do not. The design point is always the shape.

Where a real value is genuinely needed to run something — an account map, a
counterparty map, a database path — it is **an input supplied at run time**,
living outside the repository, and the code reads it from a file or the API.
This is why the import skills take `--account-map` rather than knowing anything.

**This paragraph used to say the agent holds none of the operator's data — not
his statements, not his exports, not his database — because he ran the import
tools himself and the agent read only what he chose to paste. That is no longer
how this system is used, and saying it here protected nothing while making a
reader believe it did.**

The design is agent-first: the operator hands over a statement and the agent
converts it, runs the import, and reads the result. So the agent does hold his
data, in the moment, and the control that survives is a different one — the one
`docs/import-boundary.md` §4 already describes. **A row an agent converted
arrives marked `ingest/manual/1`, a row the engine read arrives marked
`profile/<id>/<version>`, and which reader read a row is recorded on the fact for
as long as the fact exists.** That is enforcement by attribution rather than by
prohibition: it does not stop a second reader, it makes every row one produced
findable and retractable as a set.

What did not change, and is the rule this section is actually about: **none of it
enters the repository.** Not a statement, not a value out of one, not a fixture
derived from one. `make privacy` enforces the shape of that below, and it is
independent of who is holding the file.

`make privacy` enforces the part a rule cannot: no data file outside the
synthetic-fixture directories, and no statement-shaped amount in a line the
change adds. It checks shapes, never values, so it needs no list of anybody's
accounts — an earlier draft kept such a list and had to assemble it from the
operator's own exports, which made the guard the leak it was meant to prevent.

`make hooks` installs the same check as `pre-commit`, because a value refused
before it enters a commit costs a keystroke and one caught afterwards costs a
history rewrite. This project has paid the second price once.

### Language

**Everything you write is in English: code and documents alike.**
Identifiers, test names, doc comments, inline comments, `#[error(...)]`
texts, API response messages, and any new document — specs, plans, ADRs,
guides.

One exception: **values that come from a source.** Strings compared
against an external source's response stay as they are — `"Оферта"` in
`iaam-market` is a MOEX ISS value, not our text, and translating it
breaks parsing. Same for broker report sheet and column names.

Existing Russian documents under `docs/`, `README.md` and `.internal/`
are left alone; they are not retranslated. Anything **new** goes in
English, including new sections of those files.

Take domain terms from `docs/glossary-ru-en.md`. If a term is missing,
add it before using it: a synonym invented on the spot breaks grep worse
than an awkward but shared one.
