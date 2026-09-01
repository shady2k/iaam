---
name: actual-budget-migrate
description: Create iaam category groups and categories from an Actual Budget export.
---

# Actual Budget category migration

Reads an Actual Budget export and creates the same two-level category list in
iaam through the documented API. It moves the **reference list**, not
transactions.

## Export format

Actual exports a `.zip` containing `db.sqlite`; either may be given. The script
reads `category_groups` and `categories`, both of which carry `is_income` and a
`tombstone` flag.

- **Rows with `tombstone = 1` are skipped.** That flag is Actual's deletion;
  carrying such a row across would resurrect a category already retired.
- **`is_income` is carried through unchanged.** Actual models income as a flag
  on the group rather than as a separate concept, and so does iaam: one list,
  one mechanism, and cashback or interest on a balance is a category exactly as
  groceries is.

## Run

```bash
python3 .claude/skills/actual-budget-migrate/migrate_categories.py \
  --export /path/to/export.zip \
  --exclude "<name>" --exclude "<name>" \
  --dry-run
```

`--dry-run` reads the export and prints the plan without contacting a server.
Review it, then repeat with `--submit`-equivalent `--apply` and a token:

```bash
IAAM_TOKEN=... python3 .claude/skills/actual-budget-migrate/migrate_categories.py \
  --export /path/to/export.zip --exclude "<name>" \
  --base-url http://127.0.0.1:8080 --apply
```

Creation is idempotent by title: a group or category that already exists is
counted as `already_present` and not created twice.

## What `--exclude` is for

Some categories exist because the old tool needed them and mean nothing in the
new model. The script does not decide which — the names differ per person and
guessing would silently drop somebody's real category. State them per run:

- **a catch-all bucket.** iaam reports what no rule covers as its own line with
  a count and an amount, so a bucket named "other" only hides the same rows
  under a name that looks decided.
- **an opening-balance placeholder.** Actual records opening balances as
  transactions in an income category; iaam has an event kind for them.
- **a category for moving money into savings.** Under one contour that is an
  internal transfer, not spending — the defect this project exists to fix.

## What this skill must never contain

No category name, account name or amount from anybody's export. This file is
checked in and the repository is public. The list comes from the export at run
time and the exclusions are arguments.
