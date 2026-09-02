---
name: actual-budget-migrate
description: Create iaam category groups and categories from an Actual Budget export.
---

# Actual Budget category migration

**This file is a pointer. The tool lives in `tools/actual-budget-migrate/`.**

Read `tools/actual-budget-migrate/README.md` before running anything: it carries
the export format, the tombstone and `is_income` rules, the run commands and what
`--exclude` is for. Nothing here restates it, deliberately — a second copy of the
rules drifts from the first without anything failing.

The tool is `tools/actual-budget-migrate/migrate_categories.py`, a
standard-library Python 3 script whose every input is an explicit argument. Start
with `python3 tools/actual-budget-migrate/migrate_categories.py --help`.

One thing that is yours to get right: which categories to exclude is the owner's
judgement, not yours. Guessing silently drops a category he uses. Dry-run, show
him the plan, and apply only what he agrees to.
