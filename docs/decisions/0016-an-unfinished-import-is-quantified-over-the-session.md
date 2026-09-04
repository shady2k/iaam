# 0016. An unfinished import is quantified over the session

Date: 2026-09-04 · Status: proposed · Beads: `iaam-8ano`

## Context

An import session holds rows before anything is written. Nothing it holds
reaches the journal until the commit writes it, and nothing that reaches the
journal is provisional — that is the line the whole session design rests on.

The action queue is where the system says what is outstanding. It raised an item
for a session only through the session's **unanswered questions**: the loader
read every session, asked for its questions, and skipped the session outright
when there were none.

```rust
let held = store.list_import_questions(owner, session.id).await?;
if held.is_empty() { continue; }
```

So a session holding readable rows whose questions were all answered — or that
raised none at all, which is the ordinary outcome of a clean statement —
appeared in no queue item. `GET /v1/actions` is this system's published answer
to «what next», and a caller that reads it and finds nothing outstanding is
entitled to conclude the import finished. The rows were in no journal, so the
next act is to import the same statement again. A queue that is merely
incomplete manufactures duplicate work.

`GET /v1/import-sessions` did not close the gap either. It returned headers only
— `state`, `source`, `import`, `opened_at`, `assessment` — with no count of what
each session held and no count of what it was waiting on. Finding out which
import was still under way cost one request per session, and a caller had no
reason to think it should make them.

## Decision

### 1. The item is about the session, and its goal is that the session ended

A sixteenth action kind, `import_session_unfinished`, one item per session:

- **eligibility** — the session holds rows;
- **gap** — it is still open;
- **completion** — it is no longer open.

The completion is **quantified over the session and not over its questions**,
and that is the opposite shape to the one beside it.
`classification_question_completion` is deliberately quantified over questions:
«this session has no open question» is not a property a new question preserves,
so a session-wide predicate there would close the moment the last question was
answered and stay closed when the next row raised another.

Here the reverse holds. A question answered does not end the session and a
question raised does not reopen it, so a completion asked of the questions would
report an import finished the moment the last one was answered — which is the
exact reading this defect let a caller make. The two items therefore stand
together on a session with an open question, and they say different things:
«this row is unclassified» and «this import has not ended».

### 2. Abandoning satisfies the goal

«The owner committed the rows» and «the owner threw them away» are different
facts. The question item is right to refuse to read the second as the first:
there, abandoning would stand in for the owner saying what a row was, which he
never said.

This item makes no claim about any row. It says the session is open, and
abandoning ends it as finally as committing does — after which the rows were
never facts and no report is short of them.

### 3. The target is the two calls that end a session, and only those

`commit_import_session` first, `abandon_import_session` second. That is what
those two keys already say about themselves: they are the only ways an open
session ends, and a refusal that offers one without the other tells the owner he
must finish an import he may have decided against. The order is the one the
half-imported refusal already uses — abandoning is the way out rather than the
way on, and leading with «throw this away» invites a caller to discard rows the
owner spent an evening answering questions about.

**Answering a question is not among them**, although the commit is refused while
one is open. A resolution is a call that closes *this* item, and an answer leaves
the session exactly as open as it found it. The unanswered count goes in the
item's sentence instead, beside the item that does close on an answer.

**The assessment is named in prose and is not a target.** An item's target is an
`OperationKey`, and `assess_import_session` is a GET with no request body; the
session responses publish its address in their `assessment` field, which is
where that reasoning is written down. The item's sentence names the field rather
than spelling a path, so the queue does not become a second place the route's
address is written.

The item is `required_for_goal` over all four reports, on the same reading the
question item is graded under: while it stands, every report is computed as
though those rows did not exist, with nothing on the figure saying so. It is not
`blocking`. What an open session prevents is one call — opening another session
for the same declared import — and that refusal is this defect's own remedy
rather than the system declining to accept work.

### 4. The session list carries two counts, and not the rows

`row_count` and `unanswered` are read in the same store statement as the headers
— two correlated counts in the listing query — so every reader of the list has
them and none of them pays per session. The action queue is one such reader:
that is what lets it decide this item for every session without a request each.

`row_count` and not `rows`, and the suffix is the point: a plural noun standing
beside `questions` reads as the list of rows, and an external client wrote
`len(rows)` against such a name twice. The same two names already mean the same
two things on the session contents, so a caller that walks the list and then
opens one session reads one vocabulary.

**The rows themselves stay unpublished.** They are published, per row and with
what each would become, by the assessment, which computes them by planning the
commit. A second rendering built from the stored observations alone would call a
row `held` that the assessment calls unreadable, and two readings of one session
that can disagree is the defect the single-planner design exists to prevent. A
count cannot disagree with a row about what that row would become.

## Consequences

**What a reader gains.** An open session holding rows is now impossible to lose:
it is in the queue with its own identity for as long as it is open, whatever its
questions are doing, and it leaves only when the owner commits it or abandons
it. One request to the session list answers which import is still waiting and on
how much.

**What a reader loses.** Three things, stated plainly.

1. **The queue is longer while an import is under way.** A session with an open
   question now raises two items rather than one. That is the intended reading —
   the row and the session are two facts — but a client that counted items will
   count more.
2. **A commit offered on a session with an open question would be refused.** The
   item publishes it anyway, because it is the ordinary way out and it becomes
   callable the moment the questions are answered; the sentence says so, and the
   count says how many. An item that named only abandoning would have led with
   «throw this away».
3. **An empty open session is still invisible.** That is deliberate: it is what
   a caller retrying the open call is handed back, and there is nothing in it to
   lose. If a session that holds nothing ever becomes work — a declaration made
   and never fed, say — the eligibility is where that is argued, not the gap.

**Not breaking for a published client.** Both schema changes are additive:
`row_count` and `unanswered` are new fields flattened onto the same session
objects the list already returned, and a new action kind joins a set clients
already switch on open-endedly. No field changed shape, moved or went away.

**What would falsify this.** If a session is ever ended by something other than
committing or abandoning — a timeout, an expiry, a supersession by a later
import of the same statement — then «the session stopped being open» has stopped
being a goal the owner reaches by acting, and the completion above becomes a
fact about the clock rather than about him. That would need re-arguing here, not
widening quietly.
