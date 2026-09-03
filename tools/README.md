# tools

Import tools the owner runs against his own files. They are ordinary Python 3
scripts using only the standard library; every input is an explicit argument, and
none of them belongs to any particular agent, editor or vendor.

| Tool | What it moves |
|---|---|
| [`tbank-csv-import`](tbank-csv-import/README.md) | a T-Bank operations export into iaam operations |
| [`actual-budget-migrate`](actual-budget-migrate/README.md) | an Actual Budget category list into iaam category groups and categories |

Each directory carries its own `README.md` with the export format, the run
commands and the rules that make the conversion correct. Read that file, not this
one, before running anything.

## This directory is the only copy

`.claude/skills/<name>/SKILL.md` exists so that Claude Code discovers these as
skills, and `AGENTS.md` names them for every other agent. Both are **pointers**:
neither holds a copy of a script, a fixture, or the rules.

That is a decision, not an oversight. Two copies of an importer drift, and the
drift is silent — nothing fails, and the first sign of it is an import that files
the wrong operations into somebody's journal. An awkward pointer is cheaper than
that, every time.

The privacy guard enforces the half a rule cannot: `scripts/check-no-personal-data.sh`
allows a committed data file under `tools/<tool>/fixtures/` and nowhere else
outside the test fixtures, so a fixture copied back under `.claude/` is refused
before it becomes a second source of truth.

## The owner runs these, not the agent

The founding design makes the agent an **external client**. It does not hold the
owner's statements, his budget export or his database; he runs these scripts
himself, against his own files, from whatever tool he happens to have open. That
is why every identifying value — an account map, a counterparty map, a database
path — arrives as a run-time argument, and why nothing here knows any of them.

Each tool's synthetic fixtures are invented end to end. Never point one of these
scripts at real data to "check that it works", and never trim a real export down
into a fixture: a file derived from real rows carries real rows.

Where his channel ends and the API's begins — which route each tool writes to,
what a converter is responsible for knowing that the server cannot know, what an
agent is handed instead, and where that line is currently drawn wrongly — is
`docs/import-boundary.md`.
