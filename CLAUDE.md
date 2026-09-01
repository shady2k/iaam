# Project Instructions for AI Agents

This file provides instructions and context for AI coding agents working on this project.

<!-- BEGIN BEADS INTEGRATION v:1 profile:minimal hash:6cd5cc61 -->
## Beads Issue Tracker

This project uses **bd (beads)** for issue tracking. Run `bd prime` to see full workflow context and commands.

### Quick Reference

```bash
bd ready              # Find available work
bd show <id>          # View issue details
bd update <id> --claim  # Claim work
bd close <id>         # Complete work
```

### Rules

- Use `bd` for ALL task tracking — do NOT use TodoWrite, TaskCreate, or markdown TODO lists
- Run `bd prime` for detailed command reference and session close protocol
- Use `bd remember` for persistent knowledge — do NOT use MEMORY.md files

**Architecture in one line:** issues live in a local Dolt DB; sync uses `refs/dolt/data` on your git remote; `.beads/issues.jsonl` is a passive export. See https://github.com/gastownhall/beads/blob/main/docs/SYNC_CONCEPTS.md for details and anti-patterns.

## Agent Context Profiles

The managed Beads block is task-tracking guidance, not permission to override repository, user, or orchestrator instructions.

- **Conservative (default)**: Use `bd` for task tracking. Do not run git commits, git pushes, or Dolt remote sync unless explicitly asked. At handoff, report changed files, validation, and suggested next commands.
- **Minimal**: Keep tool instruction files as pointers to `bd prime`; use the same conservative git policy unless active instructions say otherwise.
- **Team-maintainer**: Only when the repository explicitly opts in, agents may close beads, run quality gates, commit, and push as part of session close. A current "do not commit" or "do not push" instruction still wins.

## Session Completion

This protocol applies when ending a Beads implementation workflow. It is subordinate to explicit user, repository, and orchestrator instructions.

1. **File issues for remaining work** - Create beads for anything that needs follow-up
2. **Run quality gates** (if code changed) - Tests, linters, builds
3. **Update issue status** - Close finished work, update in-progress items
4. **Handle git/sync by active profile**:
   ```bash
   # Conservative/minimal/default: report status and proposed commands; wait for approval.
   git status

   # Team-maintainer opt-in only, unless current instructions forbid it:
   git pull --rebase
   git push
   git status
   ```
5. **Hand off** - Summarize changes, validation, issue status, and any blocked sync/commit/push step

**Critical rules:**
- Explicit user or orchestrator instructions override this Beads block.
- Do not commit or push without clear authority from the active profile or the current user request.
- If a required sync or push is blocked, stop and report the exact command and error.
<!-- END BEADS INTEGRATION -->

## Active Agent Profile: team-maintainer

**This repository opts in to the team-maintainer profile.** That is the explicit
opt-in the managed Beads block above asks for, and it overrides the
"Conservative (default)" line inside it.

Agents working here may, without asking first:

- close beads and run the quality gates;
- `git commit`;
- `git push`;
- `bd dolt push`.

This section lives **outside** the managed block on purpose: the block carries a
generated hash and is rewritten by the beads tooling, so an opt-in edited into it
would be silently lost on the next regeneration.

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
