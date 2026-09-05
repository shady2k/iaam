# 0037. The skill keeps the process, and the reference moves beside it

Date: 2026-09-05 · Status: superseded · Bead: `iaam-arad`

**Superseded the same week, by `iaam-1pfo`.** The four companion files named
below no longer exist: what they held is either a copy of what a payload already
carried, or a rule that now lives on the published description of the field or
the response it is about, and `docs/agent-skill/SKILL.md` is the whole skill
again — under 200 lines, because the reference it used to carry is served by the
running instance. The record below is kept for the reasoning that produced the
split, and the file map in it is history rather than a route to anything.

## Context

`docs/agent-skill/SKILL.md` had grown to 1127 lines over 34 sections. Two
trimming passes this week removed the padding, and what was left was dense:
almost every paragraph carried a rule, a refusal, a vocabulary or a consequence
that had been paid for by a defect. There was nothing left to cut.

The problem was never the length as such. It was that one file was doing two
jobs.

A skill is loaded into a model's context **whole**. Every one of those lines
arrived whether the assistant was about to convey a bank statement or about to
answer «how much went on pharmacies last month». So the file was simultaneously
the process — what this is, how to get in, how to speak to the owner, and the
shape of the work — and the reference an assistant consults once it has reached
a particular part of the domain. The second job does not need to be present at
turn one, and paying for it at turn one is what pushed the first job down the
page.

`iaam-arad` states the failure this produces from the other end: a cold agent
that reads the file comes out knowing and not started. A live agent asked the
**owner** how to import a month — a question the outstanding-work queue is the
published answer to — because the path into the work was behind the teaching.

The frontmatter `description` had the same defect in miniature. It read as a
list of what the system has — a contour, two input channels, per-row verdicts,
multi-dimensional reconciliation, data quality — in this project's own
vocabulary. That string is the only thing a model reads to decide whether to
load the skill at all, and none of those words is one the owner would use
unprompted. A skill that fires on nothing he says is not loaded.

## Decision

### 1. `SKILL.md` keeps the process; the rest moves beside it, whole

Nothing is deleted. Six sections stay, four files beside `SKILL.md` take the
other twenty-eight, and every moved section moves with its own heading and its
own text unchanged.

The criterion is one question asked of each section:

> Does an assistant need this **before it knows what it is doing**, or only once
> it is doing a particular thing?

What answers «before» stays: the frontmatter and the opening; `Bootstrap`, which
is how the contract, the queue and a queue item are read, and is the process
itself; `The overriding rule`; `The agent is an external client`; `What is
published is what to convey; the words are yours`, because how to speak to the
owner applies to every conversation and not to a phase of one; `Where an import
begins: carry the document, do not read it`, which is the boundary and the shape
of an import end to end; and `What the system does not do`, which is a refusal
that has to fire before anything is promised.

What answers «only once it is doing a particular thing» moves. The four files
are named for the work, not for the section that happens to be first in them:

- **`importing.md`** — the row shapes, the question as a durable thing, what a
  first import can settle, held sessions, amounts, transfers, idempotency keys
  and the reconstructed opening. Read once a document has been conveyed and
  there are rows to dispose of.
- **`the-money-and-the-perimeter.md`** — the contour, retirement, categories,
  how a string naming an account is read, and how an instrument's code resolves
  as of a date. Read when the question is about the money itself.
- **`correcting.md`** — retraction and replacement. Read before anything already
  recorded is undone.
- **`reading-the-reports.md`** — what makes a confirmation independent, cash
  figures and anchoring, populations, the return report, unconfirmed postings,
  and what may be quoted. Read before any figure is quoted back.

Within each file the sections keep their original relative order, so a reader
who knows the old document finds them in the sequence he learned them in.

### 2. The pointers are instructions with a trigger, and they sit where the work is

This is the risk the split creates, and it is the same shape as every defect
this project fixed this week: **an agent had to guess, because something was not
written down.** A reference file an assistant never opens is that defect with
one extra step between the agent and the guess.

So `SKILL.md` says, for each file, what is in it **and when to go there**. «Read
`reading-the-reports.md` before you quote any figure of a report back to him» is
a rule; «see also `reading-the-reports.md`» is not, and would have been the
whole defect.

Each pointer is placed where the assistant will already be when it needs it:

| Pointer | Where it sits | What triggers it |
|---|---|---|
| `reading-the-reports.md` | `The overriding rule` | about to quote a figure |
| `correcting.md` | `The agent is an external client` | about to undo or re-send something recorded |
| `the-money-and-the-perimeter.md` | `The agent is an external client` | a question about what he holds, or «this product is closed» |
| `importing.md`, on questions | `What is published is what to convey` | about to put a question to him |
| `the-money-and-the-perimeter.md`, on identifiers | `Where an import begins` | about to send a string naming an account |
| `importing.md`, whole | `Where an import begins` | the document has been conveyed |

A list at the end repeats all four with the same triggers, for a reader arriving
with no particular task. The list is the redundancy, not the mechanism.

### 3. What cannot be pointed to stays where it is

A rule that fires **before** the assistant realises it is in that part of the
domain cannot live in a file it has not opened. So the boundary rules stay in
`SKILL.md` however many other places they also appear: you may convey the
owner's document and you may not interpret it; a missing value is asked of him
and never filled in; never answer in his place; nothing of the machinery reaches
him, a value already filled in included; arithmetic of your own is forbidden;
never a credential but your own.

Each companion file opens by restating the boundary that governs it, in two or
three lines, and says that the process is in `SKILL.md`. Restating a rule is not
duplicating a decision — the decision is still recorded once, and the restatement
is there so that a file opened in the middle of the work does not read as though
the boundary were somewhere else.

### 4. The `description` says what he asks, not what the system has

The frontmatter now names the questions rather than the machinery. No internal
word survives: not «contour», not «channel», not «verdict», not «reconciliation»,
and not «data quality», which is not a thing anybody asks about. It fires on
spending, on where the money went, on what is held and what it is worth, on how
the money has done, and on having a statement or an export that needs sorting
out.

### 5. The guard follows the split

`scripts/check-agent-skill.sh` refuses a versioned route path, an HTTP method
written as an instruction, and a status code, because a prose claim about a
route has nothing checking it and this document once told agents for weeks that
working routes were unimplemented. It checked one file by name. The moment there
were companion files, that guard covered the entry file and about a fifth of the
skill.

It now walks every markdown file under `docs/agent-skill/` and holds each to the
same three refusals, naming the file that failed in the message it already had.

## Non-vacuity

The guard was run against a probe file placed beside `SKILL.md` and containing a
status code. It failed, named the probe by path, and printed the offending line —
so the extension is load-bearing rather than a loop that happens to find one
file. The script keeps its own three boundary probes, which still pass.

The move itself was checked mechanically rather than by reading: every non-blank
line of the previous `SKILL.md` was compared against the union of the five new
files. The only lines that do not appear verbatim are the old `description` and
the sixteen lines re-wrapped by the five cross-reference edits below. Nothing
else moved, and nothing was dropped.

Five references would have stopped resolving across the split and were repaired,
which is the whole of the editing done inside moved text:

- `The agent is an external client` → «A mistake is retracted, not erased» now
  names `correcting.md`;
- `Where an import begins` → «the shape is the next section» named a section
  that is no longer the next one, and now names it;
- the retirement caveat → «What to assert for a reconstructed opening» now names
  `importing.md`;
- `Idempotency keys` → «A mistake is retracted, not erased» now names
  `correcting.md`;
- the question section's «the act forbidden one level down» now names the
  section in `SKILL.md` that forbids it.

## Consequences

`SKILL.md` is 327 lines against 1127. An assistant that loads the skill now
carries the process and pays for the reference only when it reaches it.

A caller that opens no companion file is worse off than one that read the old
document end to end — that is the cost of the split, and the pointers are what
buys it back. Whether they are enough is not decidable by a rule; the evidence
will be the next cold agent that imports a month without asking the owner how.

An assistant reading only `reading-the-reports.md` and never `SKILL.md` would
not meet the arithmetic ban. That is why every companion file restates the rule
that governs it in its opening lines, and why the files are reached through
`SKILL.md` rather than named anywhere else.

## What this does not settle

`iaam-arad` asks for a **checkable** rule for what belongs in the skill — every
domain word it teaches appearing in some item's reason, a caveat, or a refusal
an agent can meet. This decision moves material and does not build that guard;
the criterion here is a question asked of a section, and it is answered by
judgement.

`iaam-xc01` — that nothing checks the skill and `docs/import-boundary.md` agree —
is untouched, and the split gives it four more files to disagree with.

Whether the four groupings are the right ones will only be known from which file
a caller opens first for a question it turned out not to answer.
