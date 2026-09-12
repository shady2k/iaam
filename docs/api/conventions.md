# HTTP API conventions

This document is about **IAAM's own HTTP API**, the one rooted at `/v1` and
described by `/v1/openapi.json`. The contracts under `tinkoff-invest/` are
somebody else's API, read here as a reference; nothing in this file applies to
them.

The reader is a client author. Each section states a rule that every existing
route already follows, so that the shape of a route can be predicted before it
is called rather than discovered by calling it.

---

## 1. The shape of a list response

> **A list is a bare JSON array, unless the response carries something about the
> list itself — a page cursor, a version the items were computed under, a
> statement of what the list covered — in which case it is an object with
> `items` and that something beside it.**

### 1.1 Why the bare array is the default

A wrapper is not free. `{"items": […]}` costs the client one indirection on
every read, and it costs it forever: once published, the key cannot be removed
without breaking every caller. What a client buys for that price is a place to
put a fact that belongs to the answer as a whole. Where there is no such fact —
`GET /v1/accounts` returns the accounts, all of them, in one response, and there
is nothing true of the set that is not true of each member — the wrapper is an
empty box, and an empty box that can never be thrown away.

So the bare array is not laziness and the wrapper is not ceremony. The shape
reports whether the answer has something to say about itself.

### 1.2 Why a wrapper, when there is one

**`GET /v1/journal/events` → `{"rows": […], "next": …}`.** The journal is read a
page at a time, and the position to resume from is a property of the page, not
of any row in it. It is also the one field that cannot be reconstructed from the
rows: an absent `next` means "this was the last page", which no row states.

Journal rows do not publish a computed total. `legs` carries posted movements,
`amount` carries the one self-stated sum of a legless fact, and `basis_fee`
carries a trade's basis-only fee because it is a stated figure that is not a
leg. The latter is not a second total: it is present only where the trade
states that fee.

History `changed` calls this aspect `source_description`: it compares the
source description field, whose value may be the source's description or its
printed counterparty text.

The same reasoning already appears in the code, at the type that first needed it:
`BalancesReportDto` is an object rather than an array of account rows because
`negative_cash` is one fact about the whole answer, and `MarketPriceSeriesDto`
carries `complete_through` for the same reason — it is returned even when `rows`
is empty, which is the only way a client can tell "this instance holds nothing
for the series" from "the series is complete and holds nothing in this interval".

### 1.3 The field beside the list is named for what the list is

The rule fixes that there **is** a field holding the list, not one universal
spelling of it. `items` is the default and the name to reach for; a route whose
payload has a more precise word uses it — `rows` for the journal page and for
the market series, `accounts` for the balances report, `statuses` and `gaps` for
reconciliation, which carries three lists and could not have named any of them
`items` without lying about the rest.

A client should therefore read the shape as: array at the top level, or an object
whose documented list field it looks up once.

### 1.4 A statement of what the list covered is list-level information

`GET /v1/reports/balances` and `GET /v1/reports/returns` both carry a
`population` block: the accounts the report covered and the known accounts it did
not. This is the third case named in the rule, and it is the clearest of them.
It cannot be a field on a row, because the accounts it reports on are precisely
the ones that have no row — the report left them out, and that silence is what
the block breaks. A report over part of the owner's money that did not say so
would read as an answer about all of it.

So `population` is exactly the kind of fact the wrapper exists for.

### 1.4a A wrapper emptied, and the fact that filled it again: `GET /v1/actions`

`GET /v1/actions` wrapped, and it was cited here as the clearest case: the items
are computed under a policy, and a policy has a version, so `policy_version` sat
beside `items` as a fact about the whole computation.

It never was one. The field was the literal `1`, written at the single place the
response was built; nothing derived it and nothing bumped it, and it never
changed across any release. A client told to compare it between two responses
would have found them equal forever, which is worse than having nothing to
compare: it reads as evidence that the rules did not change, and it was never
evidence of anything. The three other responses that carry the same items — the
reconciliation answer, the broker sync outcome, the money-flow report — carried
no version at all, so the promise was not even made consistently.

The route was a bare array for exactly as long as that stayed true. This is §1
working rather than an exception to it: the question in §1.5 is whether there is
a fact about the answer as a whole that no item can carry, and the honest answer
for this route was no. A `policy_version` may come back, and if it does it comes
back derived from something that moves and published everywhere `ActionDto` is
published — not beside one of the four.

**The fact arrived, and it is not a version.** Every required item declares which
of the four reports its outstanding work stands between the owner and. Nobody
published the inverse, so a client holding the queue could say how many things
were outstanding and could not say which questions about his money this instance
was able to answer today. The route now returns `{"items": […], "reports": […]}`,
where `reports` is those same four names with the items standing in the way of
each — folded from the items in the same response, in the same reading, so the
two halves cannot describe two states of the instance.

It is §1.5's question answered the other way, and it is the *absence* that makes
it one: a report with nothing outstanding is stated by no item appearing, and an
absence is the one thing an item cannot carry. Compare `population` in §1.4 —
there too the fact is about the rows that are not there.

**It is on this route and on no other.** Three responses embed `Vec<ActionDto>`
bound to what was asked of them — the reconciliation answer's account and range,
the broker sync outcome's own verdicts, the money-flow report's own diagnostics.
A standing folded out of one of those slices would say "nothing stands in the way
of this report" on the strength of a slice that was never the queue, which is the
one sentence this surface must not produce by accident. So `reports` is published
where the queue is published whole, and the three slices are unchanged.

**`items` and not `actions`.** §1.3 asks for the name the list deserves, and the
other three call theirs `actions` because they sit inside a response about
something else, where `items` would name nothing. Here the response *is* the
queue, `actions.actions` says the word twice, and `items` is §1.3's default. It
is also the key the emptied wrapper used, so a client written against that
wrapper reads the list where it always was.

### 1.4b A count is named as a count

Where a response does publish a count — because the thing counted is not in the
response and the client therefore cannot count it — the field is named so that it
cannot be read as a list. `ImportSessionContentsDto.row_count` and
`SourceInventoryDto.row_count` are both `row_count` and not `rows`, and the
suffix was bought with a client's mistake: `rows` sat beside `questions`, a list
of one-row-shaped items, and an external agent wrote `len(rows)` against it twice
before reading the field description.

The rule has a companion in §1.3: `rows` is the right name for a list of rows,
and `POST /v1/import-sessions/{session}/commit` uses it for exactly that. One
word cannot be both, and the count is the one that gives way — a client that
indexes into a list it was given is doing the ordinary thing.

### 1.4c Two coverage statements, and why they are two

`population` says **which accounts** a report's figures are about. `held_rows`
says **which rows**: the journal alone, which is the default, or the journal plus
the held rows of import sessions the request named. They are two coordinates of
one answer and neither substitutes for the other — a report can cover every
account the system knows of and still be folded over rows nobody has confirmed.

`held_rows` is published on all four reports whatever was asked for, and the
empty block is a statement rather than a placeholder: it says these figures are
the journal and nothing else. It carries `requested` for the reason
`MarketPriceSeriesDto` carries `complete_through` — an empty `sessions` list
under `all` means the owner is holding nothing, and the same empty list under
`none` means nobody asked, and a reader that could not tell those apart would
read the first as the second.

It also carries `retained_unrecorded`: how many held rows produced no fact at
all. That count is what keeps the block honest. A row with an unanswered question
becomes nothing, so a figure "including held rows" is short exactly where
attention is owed, and a §1.4-style coverage statement that named only what it
included would be the confident half of a partial answer. Decision 0018 records
why it is a field and not a caveat in prose.

`GET /v1/journal/events` takes no such parameter and carries no such block. It
publishes no figure — it computes no total and derives none — so there is nothing
on it for a population statement to qualify, and a held row printed among its
rows would carry an event identifier that names nothing and differs on the next
read. The rows of a held session are published, per row and with what each would
become, by that session's own assessment.

### 1.5 What this means when a new list route is added

Decide the shape when the route is published, because it cannot be changed
afterwards without breaking callers. The question to answer is not "might this
list grow?" but "is there, or will there be, a fact about the answer as a whole
that no item can carry?" A page cursor, a version, a coverage statement, a
completeness boundary — those are the facts. A count is not one: a client that
holds the whole array can count it.

An example, invented: a route returning the owner's accounts at `Bank One` and
`Bank Two` is a bare array. The same route, if it reported which banks it managed
to reach and which it could not, is an object — because "Bank Two did not answer"
is not something an account row can say.

---

## 2. The lists as they stand

Every route whose success response is, or contains, a list of the owner's
things. Read it as the lookup table for §1.

| Route | Response | Shape | Why |
|---|---|---|---|
| `GET /v1/accounts` | `[AccountDto]` | bare array | whole list, retracted included |
| `POST /v1/accounts/batch` | `[AccountBatchResultDto]` | bare array | one outcome per request row — `created`, `existing`, `applied` or `rejected` — with exactly one of `account`, `declarations` and `error` set; the alias, title and declaration batch routes answer with the same shape |
| `GET /v1/instruments` | `InstrumentListDto` | object, `instruments`, `missing` | requested instruments plus identifiers not found; the wrapper is required because a bare array cannot say which requested identifiers were missing |
| `GET /v1/categories` | `[CategoryDto]` | bare array | whole history; each item carries its own retirement |
| `GET /v1/category-groups` | `[CategoryGroupDto]` | bare array | whole list |
| `GET /v1/category-rules` | `[CategoryRuleDto]` | bare array | whole history; the version is per rule, not per list |
| `GET /v1/classification-rules` | `[ClassificationRuleDto]` | bare array | whole history; the version is per rule, not per list |
| `GET /v1/journal/source-categories` | `[string]` | bare array | exact source vocabulary for the optional account and effective-date scope |
| `GET /v1/tokens` | `[TokenDto]` | bare array | whole list, revoked included |
| `GET /v1/broker-access` | `[BrokerAccessDto]` | bare array | whole list, revoked included |
| `GET /v1/contours` | `ContourListDto` | object, `contours` | `default_contour` — the contour the owner's reports are about, and `null` when he has declared none. Which contour his reports are about is a fact about the whole list that no contour row can carry, and an owner who has declared none has to be told exactly that rather than left to infer it from rows that say nothing about it (§1.4a, iaam-14is). **A caller reads it and names it; nothing resolves it for him** — `contour` stays required on the reports that take one, no report reads the declaration, and a request that omits the parameter is refused as it always was. The field is the declaration itself and not a copy of the contour: its name is the `title` of its own item in this same response, so a title copied beside the identifier would be a second place one title lives |
| `POST /v1/contours` | `ContourVersionDto` | object, `accounts` | `created` — whether this call brought the contour into existence, which no account in the composition can say; `POST /v1/contours/{contour}/versions` answers the same shape for a later version of one already there |
| `PUT /v1/report-default-contour` | `ContourListDto` | object, `contours` | the same object and the same reason as the read above: declaring which contour the reports are about leaves a state to publish, and `default_contour` is that state. `DELETE /v1/report-default-contour` answers the same shape with `default_contour` `null`, which is how a caller reads back that nothing is declared |
| `GET /v1/import-sessions` | `[ImportSessionSummaryDto]` | bare array | whole list, newest first; each entry carries `row_count` and `unanswered` beside the header, so «which import is still waiting on me» is one request rather than one per session |
| `GET /v1/decisions` | `[DecisionDto]` | bare array | the owner's own after-the-fact audit; nothing is true of the whole trail that is not true of one entry — each names its own actor, what it settled and what undoes it |
| `GET /v1/actions` | `ActionsResponseDto` | object, `items` | `reports` — where each of the four reports stands, which is stated for an unobstructed one by the absence of items and so can be carried by none of them (§1.4a) |
| `GET /v1/journal/events` | `JournalPageDto` | object, `rows` | `next` — the position to resume the page from |
| `GET /v1/journal/events/{event}/history` | `OperationHistoryDto` | object, `steps` | `current` — the fact that counts now, which no step can state on its own, since a step knows only what it left behind and not whether a later act displaced it; and which is absent for an operation ending in a retraction, a fact about the whole history that no step carries either. This is a correction history, not a transfer pairing view: if the event is a paired leg, the counterpart is not included, and its absence does not mean there is no counterpart |
| `GET /v1/market/prices` | `MarketPriceSeriesDto` | object, `rows` | `complete_through` — how far the series is known |
| `GET /v1/market/fx` | `MarketFxSeriesDto` | object, `rows` | `complete_through` |
| `GET /v1/market/key-rate` | `MarketKeyRateSeriesDto` | object, `rows` | `complete_through` |
| `GET /v1/reconciliation` | `ReconciliationResponseDto` | object, `statuses` | three lists — `statuses`, `gaps`, `actions` — none of them a property of another's rows |
| `GET /v1/transfer-pairings` | `CrossSourceMatchingDto` | object, `candidates` | `without_counterpart` — the legs nothing paired with, which no candidate can carry |
| `GET /v1/accounts/{id}/transfer-partners` | `AccountTransferPartnersDto` | object, `partners` | `stated` — whether the owner has ruled at all, which an empty array cannot say |
| `GET /v1/accounts/{id}/scope` | `AccountScopeDto` | object, `contours` | `disposition` — `inside`, `outside` or `undecided` — and `reason`, present for `outside`; `contours` is empty unless `disposition` is `inside`, since membership is a fact of contour composition and not a stored flag |
| `GET /v1/import-sessions/{session}` | `ImportSessionContentsDto` | object, `questions` | the session it belongs to, `row_count`, and how many questions are unanswered |
| `POST /v1/import-sessions/{session}/questions/{question}/answer` | `ImportQuestionDto` | object, three lists — `alternatives`, `accounts`, `also_settled` | `also_settled` — the other rows this one answer reached, a property of the call and not of the question, so it is absent everywhere a question is merely read |
| `POST /v1/import-sessions/{session}/answers` | `[ImportAnswerVerdictDto]` | bare array | one outcome per submitted answer, in request order; an applied answer carries the exact single-answer success body beside its verdict, a refused one the exact error body, so neither need be re-fetched |
| `POST /v1/import-sessions/{session}/questions/{question}/answer/preview` | `AnswerRuleForecastDto` | object, three lists — `in_this_import`, `already_recorded`, `undecided` | `state` says whether answering writes a standing decision at all; the two reasons a line has no future match both publish empty lists, and `state` is what tells them apart |
| `POST /v1/import-sessions/{session}/control-figures` | `[ControlSectionDto]` | bare array | every control section the session now holds, replacing what it held before |
| `GET /v1/reports/balances` | `BalancesReportDto` | object, `accounts` | `negative_cash`, `population`, `held_rows` |
| `GET /v1/reports/balances/series` | `BalancesReportSeriesDto` | object, `reports` | one complete `BalancesReportDto` per requested date, beside its date, in request order |
| `GET /v1/reports/assets` | `AssetSnapshotDto` | object, `accounts` | `total`, `population`, `held_rows` — the same three facts `GET /v1/reports/balances` publishes beside its rows, for the same reason |
| `GET /v1/reports/assets/series` | `AssetSnapshotSeriesDto` | object, `reports` | one complete `AssetSnapshotDto` per requested date, beside its date, in request order |
| `GET /v1/reports/returns` | `ReturnsAnswerDto` | object | not a list at the top level; `population` and `held_rows` sit beside the report's own figures |
| `GET /v1/reports/flow` | `MoneyFlowReportDto` | object, `currencies` | the interval, the scope version, `population`, `held_rows`, `actions` |
| `POST /v1/ingest/operations` | `[VerdictDto]` | bare array | one verdict per submitted row, in the caller's own order |
| `POST /v1/ingest/journal-events` | `[VerdictDto]` | bare array | one verdict per submitted row |
| `POST /v1/ingest/csv` | `[VerdictDto]` | bare array | one verdict per parsed row |
| `POST /v1/corrections` | `[CorrectionVerdictDto]` | bare array | one verdict per submitted correction, each carrying `standing_rule` where the corrected row was filed by a standing rule of the owner's — the rule survives the correction untouched, and the answer that says a fact was wrong is where that is worth naming |
| `POST /v1/reconciliation/balance` | `OwnerBalanceOutcomeDto` | object, `statuses` | `control_assertions` — whether each claim reached the journal or was already held there, which no status row can say |
| `POST /v1/import-sessions/{session}/rows` | `[ImportRowDto]` | bare array | one outcome per fed row; nothing was recorded |
| `POST /v1/documents` | `DocumentDto` | object, `rows` | the document hash, source, parser version and period |
| `POST /v1/import-sessions/{session}/document` | `SourceDocumentDto` | object, `rows` | the document hash, the source it is kept under, which source profile read it, and `unresolved_accounts` — the distinct account names the document asked for and this instance does not hold, each with the number of records it accounts for. None of the first three is a property of a row; the fourth is a fold over every row, and a caller that had to compute it would have to walk the whole list to find out that two hundred refusals are seven accounts |
| `GET /v1/source-profiles` | `SourceProfileCatalogueDto` | object, `profiles` | `refused` — the files this instance would not read and why, which no installed profile can say; a profile that merely failed to load is otherwise indistinguishable from one nobody wrote |
| `POST /v1/brokers/{broker}/sync` | `SyncOutcomeDto` | object, `recorded` | the duplicate and assertion counts for the run, and `actions` |
| `POST /v1/import-sessions/{session}/commit` | `ImportCommitDto` | object, `rows` | the session and the revision the commit was planned from |
| `POST /v1/classification-rules` | `ClassificationRuleChangeDto` | object, `plan.corrections` | `applied: false` — that the plan was not carried out |
| `POST /v1/classification-rules/batch` | `[VerdictDto]` | bare array | one verdict per submitted rule, in the caller's own order; the plan for the rule set the batch leaves behind is read once afterwards, from the route below, rather than recomputed per row |
| `GET /v1/classification-rules/plan` | `RecomputePlanDto` | object, `corrections` | `applied: false`; the plan the active rule set implies over the recorded journal, asked for without writing a rule to provoke it |
| `GET /v1/journal/aggregate` | `JournalAggregateDto` | object, `groups` | the grouping the request asked for, which no group can state on its own |
| `POST /v1/category-rules/preview` | `CategoryRuleImpactDto` | object, `preview_rows` | the rule's own reach. It used to carry `rows` beside `preview_rows`, a count that was identically `len(preview_rows)` — §1.4b justifies a count only when the counted thing is not in the response, and this one was, so `iaam-k3gh.16` removed it rather than renaming it. `CategoryMoveDto.row_count`, nested under `months[].moved[]`, and `BatchTotalDto.row_count` are the two genuine counts the same bead renamed, alongside `MarketSyncOutcomeDto.row_count` from `iaam-k3gh.15` |
| `POST /v1/category-rules/preview/batch` | `[CategoryRuleImpactDto]` | bare array | one impact per submitted rule, each shaped as the single form above |
| `PUT /v1/accounts/transfer-partners` | `AccountTransferPartnersBatchDto` | object, `statements` | one complete `AccountTransferPartnersDto` per account in the batch, each carrying its own `stated` |
| `DELETE /v1/classification-rules/{id}` | `RecomputePlanDto` | object, `corrections` | `applied: false` |

A batch response — a verdict per submitted row — is a bare array for the same
reason a full list is: the caller supplied the batch, so it already knows what
the array is about, and each verdict carries the row number that ties it back.
Where a batch response is an object, it is because the run produced a fact of its
own: a document hash, a set of counts, a revision.

---

## 3. Naming a thing the owner named

> **Wherever the API prints the identifier of a thing the owner named, it prints
> his own name for it beside it: `title`, and `institution` too for an account
> he gave one for. A name is never accepted as input.**

### 3.1 Why the name travels with the identifier

The whole interaction this API serves is the system asking the owner something
and the owner answering. `GET /v1/actions` is the clearest case: it returns one
item per account, and the items of a kind differ from one another in nothing but
a UUID. An agent that had run an import and asked what remained received a dozen
`record_owner_balance` items and could not tell which bank any of them was about
— not because the fact was unavailable, but because it was in a different
response. Reading the queue meant fetching `GET /v1/accounts` and joining.

A join every client must write is a join every client can get wrong, and a
question the owner cannot read is a question he cannot answer. The account
already holds everything needed to ask it properly — he supplied the title and
the institution once, when he created it — so making him supply the connection
again, every time, is asking twice.

The same reasoning is already written in the core, at the first type that needed
it. `PopulationAccount` carries a title beside the identifier because "the
manifest exists to be read: an owner asked to rule on an account cannot act on a
bare UUID, and a caller that had to fetch the names separately would be free to
render the manifest without them." An action item is that sentence with the same
reader.

`ImportQuestionDto.accounts` is the same reasoning reaching the last place on
the import path that had not had it. Two of the six answers to a classification
question name one of the owner's own accounts, and the question published only
`needs_account: true` — so a client answering one called `GET /v1/accounts` and
joined, which is the join §3.1 exists to remove. The candidates are the
question's own: the account the row is already on is not among them, because an
account is not the other side of itself, and they are built by the function that
builds the action queue's `/account` field so that the two cannot offer
different lists for one question.

`institution` travels with `title` for the case a title alone cannot settle. Two
accounts the owner calls `Savings`, at two banks, are one word apart in a list
and are not the same question. It is absent, never null and never guessed, when
he has not said where the account is held — an invented institution would tell
two accounts apart by a fiction.

### 3.2 Why a name is never accepted as input

A name is not an identity. The owner renames an account when the bank renames a
product; two of his accounts may carry one title; a title is a string a client
can autocomplete, mistype, translate or invent. A request that resolved an
account by name would have no way to fail on a plausible wrong answer — it would
address the wrong account and succeed, which is worse than a refusal.

The identifier has the properties the name does not: opaque, unique, stable
across every rename. And a client that took it from a response cannot have made
it up, which is the property that matters most when the client is a language
model. Every identifier a request carries was copied out of an earlier response.

There is a second cost. If a title were addressable, renaming an account would
break every stored request that named it, and the owner would be choosing
between a name that reads well and a name nothing depends on.

**Where a name is still resolved, and why it is not an exception in spirit.**
Every ingestion channel — a row of a JSON batch, a cell of a document, a batch's
declaration — reads its account through three vocabularies in order: iaam's own
identifier, the identifier the account's source prints for it (decision 0004),
and last the owner's title. The third accepts a name as input. It is kept
because every account that existed before an account could state an identity has
nothing else, so dropping the tier would silently stop recognising those
accounts and would refuse every document written before the change. It is last,
so an identity beats a title rather than tying with it; a title two accounts
share is refused rather than guessed at; and **the refusal never offers it**, so
a client reading one is steered to an identifier and nothing teaches it that a
name is addressable. Decision 0010 records the trade and what would falsify it.

### 3.3 Why the asymmetry is the protection

Reading and writing are not the same act, and the rule points the same way in
both.

Output is read by a person deciding something. It must be legible, so it carries
the name.

Input is composed by a machine acting on his behalf. It must be unambiguous, so
it carries the identifier and nothing else.

Made symmetric in either direction the pair loses one of those. Accept names as
input and the ambiguity of names — renames, duplicates, near-misses — reaches
the one place in the system that must have none. Withhold names from output and
the owner is asked to rule on something he cannot read. So the asymmetry is not
an inconsistency to be tidied away: it is what lets a client obey a single rule
with no exceptions — **it never composes an identifier and never resolves a
name.**

### 3.4 The name is paired with the identifier where the answer is made

The pair is built in the component that computed the answer, not joined onto it
by the transport. This is §1.4a's sibling: a second, independently-derived copy
of a fact is a fact that can disagree with itself.

`ActionSubjectDto::Account` is filled from `iaam_app::actions::AccountSubject`,
which the action policy builds at the moment it builds the item, out of the same
account list it wrote the item's `reason` sentence from — and that sentence names
the account too. Joined at the transport, the name beside the identifier would
come from a second reading of the store, and one response could name one account
two ways. The transport copies; it does not look up. `ConfidenceDto::from_domain`
carries the same note for the same reason: "a register assembled here could
disagree with the answer printed beside it."

That decision has a visible consequence. The diagnostic functions over a
reconciliation ledger and a money flow report are given the owner's accounts as
an argument, because a ledger holds identifiers and no names; and if they name
an account the list does not hold, they refuse the whole answer rather than
publish an item the owner cannot act on. Nothing deletes an account, so the
refusal reports the store contradicting itself and nothing else.

### 3.5 What carries the name, and what does not

Every published type that prints an identifier of a thing the owner named.

| Published type | Identifier | Name beside it |
|---|---|---|
| `AccountDto` | `id` | `title`, `institution` |
| `AccountCandidateDto` | `id` | `title`, `institution` |
| `ActionSubjectDto::Account` | `id` | `title`, `institution` |
| `AccountScopeDto` | `account` | `title`, `institution` |
| `AccountRetirementDto` | `account` | `title`, `institution` |
| `PopulationAccountDto` | `account` | `title` |
| `ImportQuestionDto` | in `prompt`, and `accounts[].id` | the titles, in the sentence; `title` and `institution` on each candidate |
| `PrintedRowDto` | `account` | `title`, `institution`, both optional — see below |
| `ForecastedMovementDto` | `account` | `title`, optional for `PrintedRowDto`'s reason |
| `UndecidedDto` | `account` | `title`, optional for `PrintedRowDto`'s reason |
| `ContourDto` | `contour` | `title` |
| `CategoryDto`, `CategoryGroupDto` | `id` | `title` |
| `InstrumentDto` | `id` | `symbol`, `title` |
| `TokenDto` | `id` | `label` |
| `BrokerAccessDto` | `id` | `broker` |

And the types that print a bare identifier on purpose.

| Published type | Identifier | Why no name |
|---|---|---|
| `ActionSubjectDto::Event` | `id` | nothing the owner said names an event; the identifier is the whole of its identity and the item's `reason` states what it was |
| `ForecastedMovementDto`, `UndecidedDto` | `event` | nothing the owner said names a recorded movement, for the reason above. It is published because it is what `POST /v1/corrections` takes, and the day, the amount and the account's title beside it are what he reads the movement by |
| `AccountBalanceDto`, `NegativeCashDto`, `AssetAccountDto`, `CashClassTotalDto`, `NotDecomposedAccountDto`, `AccountResidualDto`, `EarningSourceAmountDto`, `CaveatSubjectDto` | `account` | the answer these sit in carries `population`, whose `covered` and `outside` name every account it mentions and every account it left out. The join table is in the same response, computed by the same fold, and one report row cannot disagree with it |
| `HeldSessionDto` | `session` | nothing the owner said names an import session. He names an *import* by its label, and a session may declare no source at all; the identifier is the whole of the session's identity, and it addresses `GET /v1/import-sessions/{session}` and its assessment, where everything about it is published |
| `ReconciliationStatusDto`, `TaintDto` | `account` | the caller named the account in the request; the response answers about that one and no other |
| `JournalEventReadDto`, `JournalLegDto`, `OperationDto`, `VerdictDto` | `account` | a row-level echo of what the caller submitted or asked for |
| `BatchTotalDto`, `ControlComparisonDto`, `PlannedOriginDto` | `account` | the import assessment they sit in carries `account_resolution`, whose `resolved` and `missing` name every account the rows are on and say which of them the owner's directory holds. The join table is in the same response, computed by the same fold. It is also why a name cannot simply be printed: a total over rows on an account the directory has never heard of is precisely what these sections must be able to publish |
| every request body | — | §3.2 |

**A name that is sometimes absent is still the name.** `PrintedRowDto` is the
first entry above whose `title` is optional, and the option is not a weakening of
the rule. A row of an import session may name an account by identifier that the
owner's directory has never held — `account_resolution.missing` is exactly that
list — and a question about such a row is still a question he has to answer. That
case is the reason the second table gives for `BatchTotalDto`'s bare identifier,
and on inspection it justifies only not making the name **required**: an optional
title publishes the name where there is one and the absence where there is none,
which is strictly more than an identifier alone and true in both cases. What it
is never is the identifier printed where a name belongs. Decision 0035.

Two things follow for a client. Where a response carries a `population` block, it
is the name table for every account named anywhere in that response — look the
account up there rather than calling `GET /v1/accounts`. Where a response carries
neither the name nor a population, the account is the one the request named.

A caller of the import session assessment should read one more warning here.
`interpretation.answer_accounts` looks like a name table for that response and is
not one: it is published only where some open question admits an answer that
names one of his accounts, and it is his whole directory rather than the accounts
this session's rows are on. The question carries its own account's name for that
reason.

### 3.6 Where the rule is not yet kept

Named here rather than left to be discovered, in the spirit of §1.4a. Each is a
surface that asks the owner to act on an account, a contour or a category and
prints only its identifier, with no name table beside it in the same response:

- `AccountTransferPartnersDto` and `AccountTransferPartnersStatementDto` —
  `account` and `partners` are bare, and `partners` is precisely a list of the
  owner's own accounts he is being asked to confirm.
- The import session assessment — `SourceInventoryDto.accounts`,
  `AccountResolutionDto.resolved` and `.missing`, `ScopeAssessmentDto`'s three
  lists, `PlannedFactDto.account`, `TransferLegDto.account`. The session's
  questions do keep the rule; the assessment printed beside them does not.
  `AccountResolutionDto.unrecognised` is **not** one of these and is not an
  exception to the rule: it carries no identifier, because the accounts it names
  do not exist. It is the string a document printed, which is the whole of what
  is known about them.
- Contour references: `AccountScopeDto.contours`, `ContourDto.accounts`,
  `ContourVersionDto.accounts`, `PopulationDto.contour`,
  `MoneyFlowReportDto.contour`, `AppliedRulesDto.contour`.
- Category references: `CategoryDto.group`, `CategoryRuleDto.category`,
  `CategoryAmountDto.category`, `ClassifiedAsDto.to`, `CategoryMoveDto`.
- Instrument references outside the catalogue: `HoldingValueDto`,
  `PositionQuantityDto`, `CaveatSubjectDto::Instrument` and the position types.
  An instrument is named by a catalogue rather than by the owner, so this is the
  weakest of the five; it is listed because the reader's difficulty is the same.

Each of these is a shape change to a published type, so each is its own decision
about breaking a client, and none of them is a reason to publish a new type that
prints an identifier alone. A type added after this section carries the name.

### 3.7 The rule §3 is one instance of

§3 is about a name. The rule it is an instance of is about every value published
to be read out, and it was stated once the API had gone on to publish closed
vocabularies and counters that no name rule covers (decision 0035):

> **Nothing this API publishes in order to be read out to the owner is stated
> only in this system's own vocabulary.**

§3.3's asymmetry is this rule's two faces and not two rules. What he must
**supply** carries the question to put to him, because a JSON pointer is mute —
that is decision 0027, and it is satisfied by writing a sentence. What is
**published** carries the words he reads, because an identifier is opaque and a
code word is ours — that is this section, and it is almost always satisfied by
publishing beside it a value the system already holds.

Three obligations follow for anything new, and the first is §3:

- An identifier of a thing the owner named carries his name for it (§3.5).
- A word out of a closed vocabulary that a client may show him carries one static
  sentence saying what it means for him, published from one function so that two
  publishers of one word cannot disagree — `AnswerShape::consequence` is the
  shape.
- A decision number and a bead identifier are this project's own bookkeeping,
  and **a doc comment on a published type is not a private place to put one**:
  it is rendered into the contract as the field's description, where a client
  reads it and repeats it. So a published description says what a caller must
  know; why we decided it stays in the decision record, which the contract does
  not cite. Everywhere else in the source, cite freely.

A count carries what it counts, which is the same rule about a number rather than
a word: `SourceDocumentRowDto.row` and `locator` are two counters over one
document, and the wording on each says which.

---

## 4. What gates an act

> **What gates an act is whether a wrong performance can be undone. An act is
> owner-only when it cannot be put back; otherwise it is available to an agent
> as well.** The operation's contract names the undo, and every route that
> performs that operation publishes the same floor.

### 4.1 Why the endpoint is the wrong unit

There are three scopes — owner, agent, read-only — and the temptation is to
read them as three lists of routes. That reading fails because a route is a
name, not a consequence.

One route can perform two acts of different consequence and therefore needs two
authority checks. Conversely, a route can expose a reversible act through a
side effect or a differently named endpoint. The check belongs to the operation
whose undo the owner would use, not to whichever route name carried the request.

### 4.2 The rule

Ask whether a wrong act can be put back. An operation is owner-only when
performing it wrongly cannot be undone. It is the agent's otherwise: the owner
can revisit a reversible decision, and a decision the agent cannot make is a
decision the owner never gets to revisit.

The undo must be part of the operation's contract:

- `CreateAccount` is undone by retirement; an account with no facts is inert.
- `CreateContour` and `AddContourVersion` are undone by their versioned contour
  history.
- Rules are undone by retirement, and retirement reports what the rule reached.
- Transfer partners and account scope are restated under their existing keys.
- Retirement is withdrawn under the same account key.
- Retraction is withdrawn under the same account key, the same shape a
  retirement's own undo has. §6.6 draws the line between the two.
- A name disposition is undone with `disposition: undecided`.
- Corrections are append-only facts and are undone by another correction.
- A control balance is restated under the same account and period. It is
  admitted even though it is the owner's assertion about the outside world:
  unlike a credential, it is still a reversible record.
- The contour the owner's reports are about is declared, restated and withdrawn
  by `PUT` and `DELETE /v1/report-default-contour`. It carries no
  `OperationKey`: the queue speaks in work the owner owes, and this is guidance
  a caller reads rather than work — an item that stood until he declared one
  would be this system asking him to settle a question none of his figures
  leaves open. The act is reversible, so it is not owner-only either; the two
  routes read the same agent floor the keyed acts beside them publish, and the
  register in `iaam-server::routes` carries that argument beside the route
  names. It is a statement a caller reads and then names in a report request,
  not a substitution the server performs: no report reads it, and §2's row for
  the contour list says so where a client will meet it.

Issuing a token, issuing broker access and changing the encryption key remain
owner-only because they cannot be put back. Read-only is the absence of every
write above.

### 4.3 What each scope may do

| Act | Owner | Agent | Read-only |
|---|---|---|---|
| Read any report, the journal, the action queue, the profile catalogue | yes | yes | yes |
| Submit rows, open and feed an import session — by row or by export | yes | yes | no |
| Settle one row by answering its question | yes | yes | no |
| Generalise that answer into a standing rule | yes | yes | no |
| Commit or abandon an import session | yes | yes | no |
| Synchronise a broker or the market reference | yes | yes | no |
| Repair custody, upload and reparse a document | yes | yes | no |
| Retract an import the caller declared, untouched | yes | yes | no |
| Retract any other import | yes | no | no |
| Retract an account the caller declared, empty of business facts | yes | yes | no |
| Retract any other account | yes | no | no |
| Reverse or replace a named journal event | yes | yes | no |
| Confirm a transfer pairing (writes two corrections) | yes | yes | no |
| Record a control balance | yes | yes | no |
| Write classification, category and account rules | yes | yes | no |
| Record or withdraw a product's retirement | yes | yes | no |
| Record or withdraw that a printed account name is not the owner's | yes | yes | no |
| Record or withdraw the contour the reports are about | yes | yes | no |
| Create accounts, contours, categories, instruments | yes | yes | no |
| Issue and revoke tokens and broker access | yes | no | no |

The last row is the remaining authority boundary: credentials cannot be put
back. Every other write is admitted because its contract names an undo.

Two rows of that table are the ones §4.1 got wrong, and each is stated below with
the line drawn where it is.

### 4.4 Answering a question, and generalising the answer

Answering an import question does two things of different consequence, and they
are still gated separately inside one route.

Settling the row is mechanics: it disposes of one line the caller already
submitted, and the portfolio changes by exactly that line. Writing the
classification rule generalises the settlement into a standing decision that
can later be retired, so `POST /v1/classification-rules` is available to an
agent as well as to the owner.

The answer route retains its deliberate split: an owner answer may mint the
durable rule as a convenience, while an agent answer settles the row and
returns the rule that could be written by a separate call. That is not a second
authority grade for the rule; it keeps this route from silently combining two
acts under one answer.

The route was not closed to the agent, because the agent's half of it is
legitimate and closing it would stop the import. What comes back instead of a
bare `rule` identifier is `generalisation`, which names its own state:
`recorded` when the answer created a rule, `available` when one was possible and
the answer route deliberately left it for a separate call, `impossible` when
the row offers nothing a matcher could match on, and `unanswered` while the
question is open.
The identifier alone could not separate the middle two. Both were an absent
field, and only a client could tell which applied, because only a client knows
which token it holds. The owner reading a session back could not — he saw a
question answered and no rule — and he is the one for whom the difference is
actionable.

`available` therefore carries the rule itself, in the exact body
`POST /v1/classification-rules` takes, so adopting the agent's settlement is one
call with an object copied unedited. The cost of the split is still real — the
same counterparty is asked about again next month until the owner adopts it — but
it is a call rather than a reconstruction, and what he gets is a decision in his
own vocabulary that he can read back, edit and retire.

Nothing links the adopted rule back to the question. `generalisation` says what
*this answer* wrote, so it goes on saying `available` after the owner posts the
proposal: the rule he created is his own act, and it is read back from
`GET /v1/classification-rules` like any other.

The proposal is also an item in the action queue, kind
`adopt_classification_rule`, one per answered question that has one. A state the
system reports truthfully with no act that resolves it is a dead end dressed as
information, and `available` was one: the queue must show the owner the decision
still to be made and the exact call that carries it. The item is `recommended`
— the row it came from is settled and no report is short of anything — and its
target is `POST /v1/classification-rules` with the proposal preset as the body
and no missing field. The operation's `agent` floor is the scope of the call;
the item's state remains `needs_owner_input` because the owner decides whether
the proposal should stand, rather than the queue silently invoking it.

Because the question goes on saying `available`, the item's completion is read
from his rules instead: it disappears once a standing rule of his classifies a
row like that one the way he answered — whether he sent the proposal as it stood,
narrowed it first, or had written something covering it last month. An item whose
completion could not be observed would never leave the queue, which is how a
queue is learned to be ignored.

The condition a proposal asks about is one field, not every field the row
printed — see decision 0008. A client showing the proposal to the owner should
show him that condition, because it is the part he may want to change before he
sends it.

### 4.5 Retracting an import, and retracting anything else

Retracting an import the caller declared is a return to the state before it
acted. No decision of the owner's is reversed, because he made none: the rows
appeared because the agent put them there. Refusing it protects nothing and costs
the owner an interruption to undo somebody else's mistake — while the far more
consequential act of *writing* those rows was never gated at all.

Retracting anything else is a judgement about the owner's history: which of the
facts he holds should stop counting. There the old refusal is right and stands.

The line between the two is a bound, and a bound is only worth as much as the
evidence behind it. An agent may retract an import when all four hold:

1. it named a **label** — the account, channel and label it submitted under. A
   request without a label reaches the rows that named no import at all, and
   those are nobody's declaration to take back;
2. **every** row the target covers was submitted under this very credential — not
   under some agent token. An import both parties fed is one the owner has a
   stake in;
3. every row of it is still effective — none reversed, none replaced. One that
   is not is one somebody has already ruled on, and sweeping the remainder would
   finish a judgement the agent did not make;
4. no control assertion of the owner's covers those rows. An assertion was
   compared against the projection over an interval; retracting rows inside it
   changes what the comparison was made against, and a reconciliation status he
   has already read becomes a statement about a journal that no longer exists.

Three consequences a client should expect. The refusal names which condition
failed, because "no" without the reason sends an agent to ask the owner for
something it could have fixed itself. Retracting twice is refused by condition 3,
and that refusal is also the answer to "did my first call land". And the
acknowledgement is still required and the reversal is still a journal fact, so
the owner sees what was done either way.

### 4.6 What the bound rests on, and what it does not know

Condition 2 needs an answer to "did this caller declare that import", and a scope
cannot give it: a scope says what a caller may do and never what it did. Nothing
else recorded it either — provenance answered which source, which parser and
which import, never which principal — so the honest state of affairs was that the
system could not know.

It records it now. Every submission stamps the credential it arrived under into
the event's provenance, at the one place every caller-driven input already passes
through for its permission check, so an input that forgets to stamp cannot write
anything. Provenance is the right home for it and not a table beside the journal:
it already answers where a fact came from, and a second place recording that is a
second place that can disagree with the fact.

What this does **not** know is worth stating, because each is a way the bound is
narrower or looser than it looks:

- **It is the token, not the client.** Two programs handed one token are one
  principal, and either may retract what the other declared. That is a
  consequence of how the owner keeps his keys, and the system cannot see past it.
  It cuts the same way for the owner: rows he submits under the agent's token are
  the agent's to retract.
- **A rotated token forgets.** Issue the agent a new token and it can no longer
  retract what it declared under the old one. The retraction becomes the owner's
  again, which is the safe direction.
- **Facts recorded before the field existed name no declarer**, and every rule
  reading it refuses on absence rather than assuming. No import that predates
  this change is retractable by an agent, ever. That is fail-closed by
  construction, and the schema version is what tells a reader that "no principal"
  means "written before anyone was recorded" rather than "written by nobody".

### 4.7 Where the check goes

A gate that depends on the journal belongs inside the operation that reads the
journal, not in a step before it.

Every one of the four conditions above is a predicate over the same events the
reversal is computed from, so the cost is one pass over a slice already in
memory, not one query. That is the smaller reason. The larger one is that a check
performed beside the operation runs against its own read of the journal, and
between that read and the write the owner can record the very assertion condition
4 exists to protect. The reversal would then be written against a journal that no
longer satisfies the precondition it was checked under. One read, one decision.

The transport keeps only the floor — the scope that cannot be right under any
journal, which for a retraction is read-only. Everything narrower is decided
where the evidence is.

### 4.8 What is not changed by any of this

Corrections do not ride the ingest transport, and that is unaffected. Submission
admits an agent, so a relation field on an ingest row would turn every intake
handler into a retraction surface guarded by a per-row check one input could
forget. A separate route with its own gate is the right shape; §4.5 changed the
gate and not the shape.

And the rule cuts both ways by design. A new route that turns out to write a
standing decision as a side effect is gated on that decision, not on its own
name — which is the whole of §4.1 read forwards instead of backwards.

### 4.9 The floor is stated once, and it is published per call

The scope a call demands is written in exactly one place — a total function from
the operation to the narrowest scope that reaches it — and the route is gated by
*asking that function*. The handler does not restate it and the queue does not
grade it: both read the one statement.

That is not tidiness. It was written twice, and the two disagreed. An
outstanding-work item carried a single `required_scope` typed in beside it, and
the item for a retired product the journal disagrees with offers three ways out:
a reconstructed opening, which the agent may submit, and two rulings only the
owner may make. One field could not say three things, so it said `owner`, and an
agent that filtered the queue by the token it holds dropped the item and never
reached the call it could in fact have made. The queue told a client the server
would refuse a request the server would have accepted.

So the item's `requiredScope` is now derived and each **resolution** publishes
one of its own. A caveat's `closed_by` entries carry the same field, for the
same reason: a register that names a remedy whose floor is above the caller has
told it to make a call that will be refused.

Two things a client should read off that:

- **The item's value is the narrowest of its resolutions' values.** It answers
  «is there anything here I can act on», which is the question a client filtering
  a queue in one pass is asking. It does not answer «can I finish this alone» —
  no single value can, because whether a call succeeds depends on the body, and
  who holds a value the queue cannot supply is `provided_by` on the missing field
  (§4.4 makes the same split for one route: who decides an answer and who may
  transmit it are different questions).
- **It is a floor, not a promise.** §4.7 says the transport keeps only the scope
  that cannot be right under any journal. A call the floor admits may still be
  refused for what the request says or for what the journal holds, and a route
  may gate a second act inside itself — answering an import question admits an
  agent and writes no standing rule for one.

The contract itself does not state the floor: every route declares the same
bearer requirement, and the prose beside a refusal already disagrees with the
handlers in places. Publishing it there as well would be a second statement with
nothing reading it back, which is the defect this section exists to remove. It is
published where a client chooses a call — on the resolution and on the remedy.
See decision 0021.

---

## 5. A structure is never sent as a string

> **A field whose value is a structure is a JSON object on the wire, in both
> directions. It is never a JSON document encoded inside a JSON string, and the
> shape a route prints is a shape the route that writes it accepts unchanged.**

### 5.1 Why the read shape and the write shape must be one

Every other rule in this file can be learned from documentation. This one cannot,
because the client is a language model and the way a language model discovers a
write shape is by copying the shape it just read. A field that reads as one thing
and is written as another has no signal a client can follow: it has to guess, and
each guess costs a rejected request.

`POST /v1/classification-rules` used to take `matcher` and `outcome` as **strings
containing** the JSON that the listing prints — and an external client needed two
attempts to compose the write shape after reading the read shape. Both are now
objects on both sides, and `RuleMatcherDto` and `ClassifiedAsDto` are the same
types in the request body and in the response, so the round trip is a property of
the type rather than of two definitions that agree today.

### 5.2 Why a string is worse than an object even when it round-trips

A structure inside a string does round-trip, in the narrow sense: copy the string
back and the server parses it. What it does not do is say what may be put in it.
An object publishes its members in the specification; a string publishes
`type: string`, and the members exist only in prose the client has to find. It
also puts the encoding in the client's hands — escaping, key order, whether a
member may be omitted or must be `null` — none of which the client can verify
before sending.

The store keeps such values as opaque text on purpose: `import_sessions` and the
rule tables document exactly that, and the reason is that the store must not know
the classifier's vocabulary. That is a storage decision, and it stops at the
storage boundary. What reaches the wire is the parsed structure, read by the same
function the classifier reads it with — one reader, so a rule cannot be printed
in a vocabulary it may not be written in.

### 5.3 The category matcher shape

`GET /v1/category-rules`, `POST /v1/category-rules` and
`POST /v1/category-rules/preview` use the same externally tagged object:

```json
{"row":"source-row-key"}
{"source_category":"Groceries"}
{"description_equals":"market"}
{"description_starts_with":"market"}
{"description_contains":"market"}
```

The matcher is a required object, and exactly one of those five keys is
accepted. The three description forms say, in owner-facing words, that the
payment purpose is exactly, begins with, or contains the supplied text. A
legacy stored `description_contains` string remains a `contains` rule. The old
stored-JSON string and the `kind`/`value` fallback forms are not part of the
contract and are refused. `valid_from` and `valid_to` are inclusive date bounds
on the same rule.


The batch forms use the same rule object inside `{"rules":[...]}`:
`POST /v1/category-rules/preview/batch` returns one
`CategoryRuleImpactDto` per matcher, while
`POST /v1/category-rules/batch` returns the existing §10.1 `VerdictDto`
shape, one-based `row` included, per rule. A refused row does not refuse the
others. The preview batch consumes the same one authenticated request window as
any other call; it saves that budget by reading the journal and active rules once.
---

## 6. Two axes: what a report folds, and what still exists

> **A contour says whose money is in the figures. A retirement says whether the
> product is still there. They are two questions about one account, and a client
> that answers the second with the first destroys the first's answer.**

### 6.1 The fork, before you walk into it

The case is a term deposit that was closed, its balance returned to another
account of the owner's, and the product then gone. Three things are wanted at
once:

1. the interest counted as what the capital earned, not as money arriving from
   outside;
2. the returned principal counted as a movement between his own accounts;
3. the closed product no longer listed among what he holds.

**Keeping the account in the contour** gives the first two — the closing movement
has both accounts inside the perimeter, so it is internal, and the interest is
folded as an earning — and leaves a row for the account in every asset snapshot
for ever, with every figure zero.

**Dropping the account from a new contour version** removes the row and destroys
both of the others. A report resolves **one** contour definition and hands it to
every event; nothing looks membership up by an event's date. So the closing
transfer becomes money arriving from outside the perimeter and its principal
lands in `came_in`, and the interest, which is a movement within a non-member
account, is not folded at all — not misclassified, absent.

That second behaviour is not a bug. Asking for a contour version means "restate
that history through this perimeter", and every report states the version it
answered under. It is simply the wrong tool for "this product no longer exists".

### 6.2 What a retirement changes

A retirement is recorded per account with the date the product ceased, on the
account's own retirement resource. From that date on:

- the asset snapshot **stops publishing the account's row**, and with it the
  account's membership of its cash class — but **only where every one of the
  account's figures is zero**. Such a row adds zero to every total, so the
  suppression removes a line and never moves a number;
- every report's `population` goes on naming the account, with `standing`
  unchanged and the date in `covered[].retirement`;
- `population.retirement_revision` advances. It is the second coordinate of an
  answer, beside `contour_version`: two asset snapshots over one contour version
  are comparable when their retirement revisions match.

### 6.3 What it deliberately does not change

- **Classification.** A retired account stays a contour member. The closing
  transfer stays internal and the interest stays an earning, whenever the report
  is run. Nothing on this axis is ever read while an event is classified.
- **Any figure.** Only an all-zero row may be suppressed. If a client sees a
  total change after a retirement, that is a defect, not the feature.
- **A period before the effective date.** A snapshot taken while the product was
  open is unchanged.
- **The balances answer.** It states what the journal holds per account, and a
  reader reconciling it against a statement needs the account there. Only the
  asset snapshot drops the row.
- **The account list and the outstanding-work queue.** Neither hides a retired
  account. Its money is still in every report over a period the product existed
  in, so a question about it still changes a reported number. A client that wants
  only currently-existing products reads any report's `population` and filters on
  `retirement` — there is no separate inventory route, on purpose, because the
  population must go on naming every account the fold covered.

### 6.4 A retirement never hides money

Where a retired account's figures are not all zero, the row stands, its class
membership stands, and `confidence` carries `retired_account_not_empty` naming
the account. The caveat's `closed_by` names three calls, in the order they should
be considered, because there are three things that can be untrue here.

The common cause is a deposit whose principal predates the imported interval, and
that is why `POST /v1/ingest/operations` is named first. Its cash figure is then
movement from an unknown start rather than a balance, the movements do not sum to
zero, and the row is right to stand — the missing principal is a real hole in the
owner's cash total. Recording the reconstructed opening (an `opening_cash`
operation, §10.7) is what closes it, and the retirement then removes the row on
its own. Second is `POST /v1/corrections`, for a fact recorded on the account
that should never have counted. Third is `POST /v1/accounts/{id}/retirement`,
and it is there as the **withdrawal**: the product had not in fact ceased on the
date the owner named. A second retirement over one that stands is refused (§6.5),
so the retirement route can only mean the other direction of itself here.

For a wave the field named only the last two, and the withdrawal came first. Both
calls exist and both resolve, so nothing in the workspace noticed; what a client
holding the register saw was an instruction to repeat the act that had produced
the caveat. `closed_by` is a machine-readable field and clients are told to
prefer it over prose, so its **order** is part of what it promises: the first
entry is the remedy for the ordinary case.

The same state is also an item in the action queue, kind
`retired_account_not_empty`, one per retired account the journal still shows a
figure on. It carries the same three calls in the same order, and adds the
request each of them wants. It is required for the asset snapshot and for no
other goal — a retirement is read in the snapshot's row suppression and nowhere
else — and its state is `needs_owner_input`, because the amount of a
reconstructed opening is what the account held before this system knew anything
about it and no guess belongs there.

The item's completion is read from the journal, not from the declaration and not
from the caveat: it disappears when the account's figures are zero, whoever
brought them to zero and however. The retirement is untouched by that, and stays
standing.

### 6.5 What refuses

- a second retirement over one that stands — withdraw first, so that the change
  is a revision a reader can see, rather than a date silently moved under
  snapshots already taken;
- a withdrawal when nothing stands — every accepted call mints a revision, and a
  revision that changed nothing is a coordinate that means nothing;
- an effective date after today;
- an account the owner does not hold — not found, because an identifier is not an
  access right.

Retiring an account that still holds money is **not** refused. It is his
statement about his product, and refusing it because a fold disagrees would make
his word conditional on how much of his history has been imported. §6.4 is what
makes that safe.

### 6.6 A third axis: a row that should never have existed

A retirement and a scope exclusion both presuppose an account: something real
that either stopped existing or that the owner's reports leave out on purpose.
`iaam-o0oj` found a case neither answers. A probe rebuilding one instance from
another minted three accounts the owner confirmed he never created. Retiring
them said "these products existed and ended" — false, nothing ever existed.
Ruling them outside every scope said "I have decided my reports do not want
this money" — also false, and worse: it makes an account the owner
*deliberately* excluded indistinguishable from leftover test data, which is
exactly the confusion §6.1's fork exists to prevent one axis over.

**Retraction is the third act, and it removes rather than annotates.** A
retirement and a scope exclusion both add a fact *beside* the account inside a
report's population — a date, a standing. Retraction takes the account out of
the population outright: `population.outside[]` does not name it, no caveat is
raised about it, and the outstanding-work queue names it nowhere. Nothing was
omitted, because there was never an account to leave out — which is a sentence
neither neighbour can make true, because both keep the row standing.

**It is refused wherever the account carries a business fact.** You cannot
un-exist a thing money moved through — the same restraint §6.4 places on a
retirement's own refusal, read the other way: there `RetiredAccountNotEmpty`
lets the row stand and names the remedy, here the call itself refuses and
names the remedy — retract a fact that should never have counted with a
correction, or record a retirement instead where the fact is real and the
product simply ceased.

**Its authority is attribution, not a flat scope.** The owner may always
retract. An agent may retract only an account it declared itself —
`accounts.declared_by` (`iaam-7ffl`) is what makes that checkable, the exact
doctrine §4.5-§4.7 already state for retracting a declared import, carried
over to an account instead of a journal fact.

**It is itself a standing decision, and it is withdrawable** under the same
account key, §4.2's own list. A wrong retraction is recoverable — the account
returns to standing, and every report and the queue see it again — because an
act this consequential must not be more dangerous than the mess it cleans.

---

## 7. A session that has not ended is an item of its own

A session raises an item per unanswered question, and one more for itself, kind
`import_session_unfinished` — one per session that holds rows and has not been
committed or abandoned. The two say different things: «this row is
unclassified» and «this import has not ended», and a session with an open
question raises both.

The item exists because the queue used to say the first and not the second. A
session whose questions were all answered, or that raised none — the ordinary
outcome of a clean statement — held rows that were in no journal and appeared in
no item, and a caller reading an empty queue is entitled to conclude the import
finished. It had not: the next act was to import the same statement again.

The goal is that the session stopped being open, and **both** of the calls that
end one satisfy it. Its target publishes them in the order the refusal on the
open route uses: `commit_import_session` first, `abandon_import_session` second.
Answering a question is not among them, because a resolution is a call that
closes the item and an answer leaves the session as open as it found it; the
sentence says how many answers are still outstanding, and the commit is refused
until they are given. The item is `required_for_goal` over all four reports —
while it stands, the reports are computed as though those rows did not exist —
and it names `agent` as the scope, because both routes accept an agent token.

Reading the queue is not the only way to see this. `GET /v1/import-sessions`
carries `row_count` and `unanswered` on every entry, which is the same question
answered for every session in one request. Decision 0016 has the argument.

---

## 8. A figure the owner asserts, and how it stops being read (`iaam-k3gh.11`)

`POST /v1/ingest/operations` accepts a `stated_securities_value` operation: a
figure the owner states for what an account's securities are worth, with no
instrument named and no composition. It exists for the ordinary state of a
broker he has not connected — before it, the only way to say "there is money
invested here, composition not broken out" was `opening_cash` with a note,
which puts a securities figure in the cash dimension and overstates cash by
exactly the amount that is not cash at all.

### 8.1 Where it appears, and what it does not become

The figure never enters `cash`. It reaches `GET /v1/reports/assets` two ways:

- `accounts[].stated_securities` echoes the owner's latest assertion at or
  before the report date, on every account that has one — the raw fact, exactly
  as `accounts[].cash` states a figure whether or not an opening anchors it;
- `positions.stated` lists it again, per account, **only while it is still
  read into a total** — see §8.2 — and `positions.stated_totals` is that list
  added up per currency. Both are kept apart from `positions.holdings` and
  `positions.totals`: one is a sum of quotes, the other a sum of assertions,
  and folding an assertion into a total of quotes would make an owner-stated
  number read as a system valuation. Both still reach the snapshot's `total`,
  which is how the account's total comes out right without a composition.

`GET /v1/reports/balances` carries the same raw echo, at
`accounts[].stated_securities_value`, for a caller that reads the balances
answer rather than the asset snapshot.

It is **not a position**. It carries no instrument, no quantity and no lot,
and it is not returned as one: `positions.holdings` and `accounts[].positions`
are unaffected by it.

### 8.2 A second assertion replaces the first; a real position retires it

Two different owner-asserted facts on this API accumulate when submitted
twice — `opening_cash` and `opening_position` both do, deliberately (§6.4's own
`opening_cash` reference is one such reconstructed opening). `stated_securities_value`
does not, and the difference is structural rather than a special case: it
posts no leg, because it names no instrument to post a security leg against,
so there is nothing for two submissions to accumulate through. The report
instead reads the assertion dated at or before the report date that is
latest — exactly as it reads the latest price for an instrument — so a second
`stated_securities_value` on one account **supersedes** the first rather than
adding to it.

A full position synchronisation supersedes it too, and by a different
mechanism: presence, not date. The moment the account carries a real position
fact — an `opening_position` operation or a trade naming an instrument —
`positions.stated` stops listing it, however recently it was asserted. Nothing
retracts the assertion and nothing links the two events; the presence of a
real position is what retires it. `accounts[].stated_securities` keeps echoing
the raw fact regardless — it is the fold behind `positions.stated` that stops
reading it, not the row that stops carrying it.

### 8.3 What it does not silence

The account this covers is still one whose composition the journal does not
know. `confidence` carries `position_facts_missing` for it exactly as it would
without the assertion — an asserted total is not a coverage answer — and, only
while the assertion is active, a second caveat, `securities_value_asserted`,
saying the total includes a number the owner stated rather than one the system
derived. Both are closed the same way: `submit_operations`, because a real
position synchronisation is what makes `positions.accounts_without_position_facts`
stop naming the account and what makes `positions.stated` stop naming it too.
