# Wave report: the door still open

## `iaam-ag7b` — channel choice belongs in the contract

Changed:

- Added caller-facing descriptions to the published route documentation in
  `crates/iaam-server/src/routes.rs` for broker sync, broker reports, own CSV,
  conclusive operations, import sessions, journal facts, and source documents.
- Made the source-document route explicit as the primary route for an
  institution's readable export.
- Identified the session rows route as the observation fallback, without making
  it an equal menu option.
- Reduced `docs/import-boundary.md` §1 to the maintainer map of runner, payload,
  and format-knowledge location; it now points callers to the route descriptions
  in `/v1/openapi.json` instead of duplicating their choice guidance.

Acceptance criteria answered:

- A caller can choose from the operation descriptions it actually reads, and
  the distinction is published in one caller-facing place.
- The boundary document remains useful to maintainers without being the only
  place that explains the channels.
- The source-document route is explicitly primary; the observation route is
  described only as the fallback after profile recognition cannot read the
  export.

Deliberately not done: no route was renamed, removed, or added; no fallback was
promoted to a peer of the primary document route; the agent skill was not made a
second copy of the route map.

## `iaam-wzon` — profile refusal names the open door

Changed:

- Added the fallback sentence at the application boundary in
  `crates/iaam-app/src/scenarios/source_profile.rs`, where the HTTP contract is
  known and `iaam-ingest` remains route-agnostic.
- Extended the existing server contract test to prove an unrecognised document
  refusal names `POST /v1/import-sessions/{session}/rows`.

Acceptance criteria answered:

- A no-profile refusal names the remaining observation route in the API's own
  vocabulary.
- The sentence preserves the primary choice: it recommends the observation
  route only when no installed profile can read the export.
- The end-to-end refusal test asserts the alternative is present.

Deliberately not done: no route knowledge was introduced into `iaam-ingest`, and
named-profile or malformed-document refusals were not given an unrelated
fallback sentence.

## `iaam-8hdk` — refusal explains the document-side mismatch

Changed:

- Replaced the boolean result of `engine::recognises` with the profile-side
  `Recognition` diagnostic, preserving only declared encoding, header-row, and
  header-cell shape information.
- Classified encoding mismatch, missing configured header row, missing named
  header cells, and malformed delimited input without retaining document cell
  values.
- Included each installed profile's profile-side mismatch in the catalogue
  refusal, while keeping the installed profile list.
- Added focused tests for wrong encoding, missing header row, missing header
  cells, and the refusal's lack of document content.

Acceptance criteria answered:

- The refusal names what the profile looked for and which profile-side shape did
  not match, rather than collapsing every failure into the installed-profile
  list.
- Recognition diagnostics carry no document values, and the catalogue test
  checks that the refusal does not echo the submitted document shape.

Deliberately not done: no source cell, amount, counterparty, or other document
content is copied into the diagnostic; row-level read refusals keep their
existing detailed `Rejection` path.

## `iaam-ftpz` — verify before writing

Evidence reviewed before changing anything:

- `docs/deployment.md` §2.1 already stated that `IAAM_SOURCE_PROFILES` is a
  read-only directory, that only `.json` files are considered, that the
  directory is read once at start-up, and that rejected files are published by
  `GET /v1/source-profiles` with their location and reason.
- `crates/iaam-ingest/src/profile/catalogue.rs` already carries `Refused` with
  `origin`, optional `id`, and `reason`, and publishes refused files separately
  from installed profiles.

All acceptance criteria were already satisfied, so no documentation or code was
changed for this bead. Deliberately not done: no duplicate deployment paragraph
was added and no reload or signalling mechanism was invented.

## Verification

`make check` was first run before the final type annotation and stopped in
workspace clippy with `error[E0282]: type annotations needed` in
`crates/iaam-ingest/src/profile/engine.rs`. After that fix, the prescribed
command was rerun and passed. The observed successful gate output included:

- `Architecture guards passed.`
- `Personal data checked (tracked files, and lines added since origin/main).`
- `Agent documents checked: 1.`
- `Fixtures checked.`
- nextest summary: `2887 tests run: 2887 passed, 0 skipped`
- workspace doc tests completed successfully with zero doc tests in each listed
  crate.

Additional commands run beyond the prescribed `make check`: `pwd`; the
mandated read-only `bd show iaam-ag7b iaam-wzon iaam-8hdk iaam-ftpz`; `git diff`
for review; the repository search for the report ignore rule; `git add` and
four bead/report commits (including the initial failed `git add REPORT.md`,
which reported that the globally ignored report needed `-f`). No bead was
closed, no Beads state was changed, and nothing was pushed.

No new bead-worthy follow-up was found.
