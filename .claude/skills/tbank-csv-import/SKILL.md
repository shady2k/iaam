---
name: tbank-csv-import
description: Import a T-Bank operations CSV into iaam through the canonical operations API.
---

# T-Bank CSV import

**This file is a pointer. The tool lives in `tools/tbank-csv-import/`.**

Read `tools/tbank-csv-import/README.md` before running anything: it carries the
export format, the run and verification commands, the internal-transfer and
refund rules, and the idempotency key. Nothing here restates it, deliberately —
two copies of an importer's rules drift, and the drift is silent until somebody's
import files the wrong operations.

The tool is `tools/tbank-csv-import/import.py`, a standard-library Python 3
script whose every input is an explicit argument. Start with
`python3 tools/tbank-csv-import/import.py --help`.

Two things that are yours to get right, and are covered in full by the README:

- The account map and counterparty map are **run-time inputs supplied by the
  owner**, never files in this repository and never values you infer. A
  counterparty that is in fact the owner's own account elsewhere cannot be
  recognised from the export; only he knows.
- Always dry-run first and show the owner the summary. `--submit` writes to his
  journal.
