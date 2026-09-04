# 0012. A question names its row, and every answer names its price

Date: 2026-09-04 · Status: proposed · Beads: `iaam-3ewp`, `iaam-pzm9`

## Context

One month of one bank's statement, fifteen rows, imported through a session.
Eleven settled without anyone being asked. Four were movements the source
labelled only as internal to itself — a day, a sum, no direction word and no
counterparty — so `Question::UnresolvedDirection` was raised four times.

Two things went wrong for the person being asked.

### The four questions were the same sentence

`Resolver::render` builds the wording from the `Question`, and a `Question`
carries the account, the word the source printed and the party it named. All
four rows carried the same word and named nobody, so all four sentences were
identical to the character. The row number was published beside each one and is
a perfectly good identifier — it is what the answering call takes — but it is
not what a person recognises a line on a statement by.

So the owner matched question to row by counting down the list, got the offset
wrong, and answered some for rows he had not read. **That is worse than not
answering.** An answer the question admits is accepted: it settles the row, it
may be generalised into a standing rule that decides rows nobody has looked at,
and no later call revisits it. Nothing in the system can detect it afterwards,
because nothing else knows what the row was either.

This is not the identifier defect earlier waves fixed. Nothing here is
unaddressable; the row is addressable and the sentence about it is not
recognisable.

### The question did not say what turned on the answer

The wording says what the row leaves open — for this variant, that no
counterparty was named, so neither the direction nor the other side can be read
— and stops. It does not say what the answer decides.

The alternatives are not shades of one another, and the journal knows it.
Following `Answer::classification` and `Answer::movement` through
`ObservedRow::resolve`, `normalize` and `MoneyFlow::absorb`:

| Answer | Event | Where the money lands in the money-flow report |
|---|---|---|
| `sent_to_own_account` | `CashTransfer` | transfers between the owner's own accounts (or an unexplained outflow, where the far account is outside the contour) |
| `received_from_own_account` | `CashTransfer` | the same, recorded from the sending side |
| `paid` | `CashOut` | what went out, under the category his rules give the row |
| `received` | `CashIn` | what came in from outside |
| `fee` | `Fee` | fees, on a fee leg of its own |
| `income` | `Income` | what the capital earned |
| `refund` | `Refund` | **subtracted** from what went out, in the category the money was spent in |

Answering `received` where the truth is `received_from_own_account` moves an
amount out of «between my own accounts» and into «money that came in» for the
whole year. In the owner's words, he was choosing blind: a materially different
picture of his year, chosen from a sentence that never mentioned it.

## Decision

### The row's day and sum go into the sentence, and nowhere else

`Resolver::render` takes the `ObservedRow` beside the `Question` and opens every
one of the four wordings with the row as a person finds it: the day the source
dated it and the amount with the sign the source printed.

**Not as published fields beside the question.** The prompt is the one carrier
every surface shares — it is the ingest verdict's sentence, the session's
`prompt`, and the outstanding-work item's `reason`. Fields on the session's
question would leave the queue publishing four items nothing tells apart, and
the queue is where the owner is told there is work waiting at all.

A published amount would also be a second rendering of figures the assessment
route already computes by planning the commit. That is the pair of readings that
can disagree which the session's `row_count` documents refusing, and this
amount is a recognition aid rather than a figure to compute with: §3.3 puts
legibility on the output side, and this is the output side.

**Two facts and no more.** The description would narrow it further and is left
out deliberately: it is the row's whole text, written by the source, and pasting
it into a sentence the owner reads is how a statement's own words end up quoted
in a queue item, a log line and an agent transcript. A day and a sum point at a
line on a month's statement; that is enough.

### The consequence goes on the alternative, not into the prompt

`AnswerShape::consequence` is one static sentence per answer, saying what
recording it does to the money-flow report. It is published on the session's
alternatives, on the ingest verdict's alternatives, and on the queue item's
`/answer` input, all from that one function.

**Structure, not prose.** Seven consequences gathered into the prompt would be a
mapping from a word to its effect encoded as a string — a structure sent as
prose, which `docs/api/conventions.md` §5 refuses, and for exactly its stated
reason: the caller that has to show the owner one alternative would have to take
the sentence apart again. Attached to the word, the consequence travels in the
same object the caller already reads to find out what may be said.

The prompt keeps one clause, identical on all four questions, saying that the
answer decides which figure of the report the row moves and that the
alternatives say which. That clause is what a surface carrying only the prompt
still gets.

**Optimised for the agent, so that the human is served.** The prompt is read by
an agent and relayed to the owner, and these are not the same reader. A human
judging a choice wants the effect beside each option; an agent acting on it
wants a value it can key on. Prose in the prompt serves neither — the agent must
parse it, and the human gets it only if the agent parses it correctly. A
sentence per alternative gives the agent something to lay out item by item and
gives the human the effect against the word it belongs to, which is the layout
he would have asked for.

**The money-flow report and not the returns path.** They disagree about one
member: a refund is `InboundFromOutside` for the contour classifier, because a
returns calculation must see the cash cross the boundary, while the household
report subtracts it from spending. `EventKind::flow_endpoints` records the
disagreement in its own comment. Naming both in every sentence would make each
alternative read as a caveat; the household report is what the owner reads a
month of statements against, so it is the one named, and the sentence that is
affected says which report it means.

## Consequences

- A question raised **before** this change keeps the wording it was stored with.
  The prompt is rendered once, when the row is parked, and there is no migration:
  rewriting stored prose would be the system changing what it asked after the
  fact. Open questions from an earlier import stay recognisable only by their row
  number, and are answered as they always were.
- `AnswerAlternativeDto` gains a required `consequence`, and the queue's
  `InputAlternativeDto` gains an optional one — absent for vocabularies whose
  words explain themselves, present for these seven.
- The sentences are claims about a projection that cannot be run where the
  question is asked: there is no journal, no contour and no category index at
  that moment. They are pinned by test instead — `iaam-ingest`'s observation
  suite walks answer → operation → event → `flow_endpoints` and asserts each
  answer produces the fact its sentence claims.
- The answer vocabulary is untouched. Whether these four rows have an answer at
  all is a separate question and is decided separately.
