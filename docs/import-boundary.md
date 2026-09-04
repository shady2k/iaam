# The import boundary

This document joins three others that each describe one piece of an import and
none of which describes the join: `tools/tbank-csv-import/README.md`, which
converts one bank's export; `docs/agent-skill/SKILL.md`, which tells an agent to
open a session and feed it rows; and the contract behind `/v1/openapi.json`,
which publishes both channels and says nothing about which is which.

It was written because the map had to be assembled by hand every time somebody
walked the path. An external agent doing so ended up importing `build()` out of
the owner's tool into its own process rather than restate that tool's rules —
which worked, and is not a boundary anyone designed. The reader here is whoever
is about to run, extend or document an import, and the question answered is
**which channel writes what, who runs it, and what the other one is therefore
not allowed to assume**.

---

## 1. The channels, and who runs each

Every route that puts a fact in the journal, and where the knowledge of the
source's format sits for each.

| Channel | Run by | Handed | Format knowledge lives |
|---|---|---|---|
| `POST /v1/brokers/{broker}/sync` | agent or owner | nothing; the server fetches | `iaam-broker`, in tree |
| `POST /v1/documents` | owner | a broker's XLSX report | `crates/iaam-ingest/src/report/`, in tree |
| `POST /v1/ingest/csv` | either | **iaam's own** CSV columns | `crates/iaam-ingest/src/csv_source.rs`, in tree |
| `POST /v1/ingest/operations` | owner's converter | already-converted rows | outside the repository |
| `POST /v1/import-sessions` … `/commit` | either | already-converted rows | outside the repository |
| `POST /v1/ingest/journal-events` | owner | corporate actions and offers | — |
| a document read by a **source profile** — decided, not yet built | owner | an institution's own export | `crates/iaam-ingest/schema/source-profile-v1.json` and one profile per document type, in tree or beside the deployment |

The last row is the only one that is not a route today. Decision 0019 settles
what a source profile is and what it may say; the channel that carries such a
document is a separate bead, and §9 below is what the row means until it exists.

Two of those rows are read wrongly often enough to be worth naming.

**`POST /v1/ingest/csv` does not accept a bank's export.** Its columns are
iaam's — `date`, `type`, `account`, `currency` and the optional rest. Its
`account` and `counterparty_account` cells are resolved through the same tiering
`POST /v1/ingest/operations` resolves a row's account with — iaam's own
identifier, then the identifier the account's source prints for it, then the
owner's title (decision 0010). It used to resolve the title and nothing else,
which made one flow answer «which account is this» in two vocabularies. It is a
hand-writable format, not a bridge from anybody's institution. Sending a bank
export to it does not half-work; it rejects every row. The path is what invites
the mistake: `csv` is the file extension of every statement any institution
emits, and the name says nothing about whose columns are expected.

Its rows **are** retractable, since iaam-0f8f. They used to arrive under a
source minted for one request, which `POST /v1/corrections/imports` could never
reach; they now arrive under the `csv` channel of the account each row names,
so the retraction is the ordinary one — that account, channel `csv`, and the
`label` query parameter the submission gave, if it gave one. A row that named
no `idempotency_key` is identified by the document's digest and its own line
number, so re-sending one document writes nothing the second time.

**Corporate actions and offers are declared the same way.** The journal-fact
channel had no declaration at all: it minted a source per request, so what it
recorded was reachable one event at a time and never as the batch it arrived in,
and a resubmission of the same facts was a second source rather than the same
rows. It now takes the declaration the conclusive route takes — account, channel,
label — and refuses a batch whose facts do not all name the declared account, so
a batch spanning two accounts is two calls. Omitting the declaration still
records the facts under a source minted for the request; that is what every
caller written before had, and it is not a default worth choosing.

**A session is not a second vocabulary.** `AddImportRowsRequest` carries the
same `OperationDto` the conclusive route takes. The difference between the two
channels is *when* the fact is written, not *what* a row may say. So anything
this document settles about the shape of a row settles it for both.

## 2. The channel with no parser is the bank export, and privacy is not why

The repository holds two broker report parsers and a CSV parser. Format
knowledge is therefore not the thing kept outside: a parser is written from a
format specification and a fixture invented end to end, and `CLAUDE.md` has
never objected to either.

What keeps a bank export outside is that converting one needs a second kind of
knowledge, and until recently the server had no home for it.

The reason a *third* parser was not simply written is §8's second ground: every
institution is another format, and every format would be another release. §9 is
how decision 0019 answers that without moving the format back outside.

## 3. The conversion needs three kinds of knowledge, and they belong in three places

**Format.** Which column holds the posted amount, that a negative sign means
money left, that the timestamp is `DD.MM.YYYY HH:MM:SS`, that the two legs of an
internal transfer are posted seconds apart rather than at one instant. All of it
is recoverable from the export alone by anybody who has one. It lives in the
owner's tool today; it could live anywhere.

**Which account a printed name is.** This used to be pure owner knowledge and is
not any more. Decision 0004 gave an account the identity its source prints —
`provider_account_id`, plus aliases with validity intervals for the cards over
it — and resolution now tries iaam's identifier, then the printed identity and
its aliases, then the title, stopping at the first tier that matches anything.
A declaration may name the account by the number the bank prints, and — since
`iaam-varx` — so may every row. `--account-map` was the pre-0004 workaround kept
alive, and decision 0004 named this exact situation as its own falsification:
*"the owner finds himself maintaining a file that maps a source's identifier to
a `provider_account_id`."* Decision 0005 finds that the test did not trip — the
map resolves to a **title**, which is the pre-0004 shape — and retires it.

**What the row was.** Nothing in an export distinguishes a payment to a stranger
from a top-up of the same person's account at another bank; both are a name and
an amount. Nothing distinguishes a merchant returning money from money someone
sent. Nothing says that a positive row carrying the balance's own interest is
not an arrival from outside. This is the owner's judgement, it is what
`--counterparty-map` and the tool's refund and interest rules encode, and it is
the only one of the three that an export can never supply.

The server has a home for most of it and does not receive it. Counterparty
resolution reaches `Counterparty::OwnAccount` from the directory; a
classification rule written from the owner's answer settles the same
counterparty for every later import; his transfer statement withdraws a
resolution he says does not happen; `GET /v1/transfer-pairings` proposes the two
legs. Every one of those is bypassed by a converter that concluded first.

## 4. What an agent is handed, and what it is never handed

`CLAUDE.md` decides this and decision 0003 draws the credential half of the same
line. Stated for an import, so it need not be re-derived:

- **Never a statement.** Not the file, not a path to it, not his database, not
  the account map, not the counterparty map. The agent does not open the
  owner's export, and "just read the CSV so you can check the tool works" is the
  request this rule exists to refuse.
- **Never a credential but its own.** No broker token, no encryption key.
- **What it does get** is what the owner chooses to paste into the conversation
  and everything the API answers. Those are enough to open a session, feed it,
  read its assessment, relay its questions and commit it.

So an agent asked to import a month does one of two things: it works from rows
the owner put in front of it, or it hands him the run command for his own tool
and works from the summary he pastes back. It never becomes the converter by
opening the file itself.

## 5. What the queue's `start_account_import` presupposes and does not name

The item's own sentence is honest about the step before it — *"fetching the
statement out of the bank is a step outside this API"* — and then says *"feed it
the rows"*. Between those two clauses sits the conversion, and the item
attributes it to nobody.

It could not, with what it had. `MissingInput` publishes who supplies a field in
three words — the owner, an external document, the caller — and the rows are not
a missing field at all: they are the body of a later call that the item does not
describe. The two fields it does publish, the channel and the label, are exactly
the two that *are* fields. So the item presupposed a converter, and the
vocabulary it is written in has no word for one.

For the owner running his tool the presupposition held. For an agent holding
pasted rows it held only through the observation shape — a row submitted as what
the source stated, with the source's own sign and direction word, which the
server settles or asks about. The item never mentioned that shape, and the only
worked example in the repository, the tool, does not use it.

**The item now names it, and no fourth word was added** (`iaam-tt71`). The reason
says that deciding what a row was is not a step between fetching the document and
feeding the session: a row whose direction or nature the reader cannot tell goes
in as `unresolved_direction`, and the session settles it or asks the owner.

The word was refused rather than forgotten, and the order of the two beads is the
argument. The case for `ProvidedBy::Converter` rested on the parity defect of §6:
while the observation channel could not express a refund or an income kind, a
converter that concluded first was the only thing that could produce a complete
import, and a contract that presupposes a participant should name it. §6 is
closed, so the participant is no longer required, and writing the word now would
record a workaround as the design at the moment it stopped being needed. Two
smaller reasons hold independently of that: each word in `ProvidedBy` names *who
supplies a value*, and a converter is a step rather than a source; and the rows
are not a field of the request the item publishes, so a pointer at them could not
be satisfied by filling that request in.

Naming the shape is not naming the tool, which §8 rejects and still should.
`unresolved_direction` is a value of this API's own contract, published in the
document the same caller is already reading.

**The three words are now published with their meanings** (`iaam-k6l7`). They
were not: `provided_by` reached the wire as a bare string, with the codes written
out in the transport and the contract saying neither what the list was nor what
any entry meant. That is why the fourth word kept being rediscovered — a reader
with three unexplained codes cannot tell that the axis is *who holds the value*
rather than *what it took to read it*, and a value the owner exported, converted
and restated looks like a case with no word for it. It has one, and
`external_document`'s published sentence now says so: fetching, converting and
restating are steps on the way to a value, not sources of it. The vocabulary is
expanded from `iaam_app::provided_by_vocabulary!`, so the code, the meaning and
the transport's conversion come from one list and cannot drift apart.

## 6. Where the line should be, and the one thing that must move first

**A converter translates a format. The API reaches conclusions.** That is the
line.

It could not simply be moved, because the conclusive channel used to be strictly
more expressive than the observation channel. Decision 0006 closed that, and what
it closed is worth stating precisely, because the same reasoning is what any
future outcome has to survive:

- `Classification` had four outcomes — internal transfer, external flow, fee,
  income — and **no refund**, while `OperationKindDto` had one. A row submitted
  as an observation could therefore never come out as the thing a positive row
  with a spending category is, and no question could repair it afterwards,
  because none was ever asked about a return. It now has five, `Answer` and every
  question that leaves an arrival open publish `refund`, and the money-flow
  projection subtracts the resulting fact from what went out rather than adding
  it to what came in.
- An observation resolved as income always carried **no income kind**, on the
  correct ground that the source named none. The ground is still correct about
  the source and was never correct about the owner: `Classification::Income` and
  `Answer::Income` now carry the kind, so he can name one, and the naming travels
  into the rule his answer becomes rather than stopping at the row he looked at.

**And one question asked about every row.** This is decision 0005's finding, and
it is stated here at length because it is larger than the two above and outlives
them: closing both gaps in the vocabulary leaves it standing.

`classify` in `crates/iaam-ingest/src/classification.rs` settles a row in
exactly two ways — the directory recognised the counterparty as one of the
owner's own accounts, or a rule matched — and answers `Ambiguous` otherwise.
`question_for` beside it is **total**: every combination of counterparty and
direction yields a question. There is no outcome meaning *nothing suggests
otherwise, so this is ordinary external flow*, and that absence is deliberate —
the alternative is a default, and a default is the guess the shape exists to
refuse.

The price is paid per row. A converter that concludes settles a month of
ordinary shopping without asking anything. The same month submitted as
observations raises one question per row that no rule already covers, which on a
real statement is most of them.

The loop that should absorb that price does not close. Answering a question
mints a rule from `matcher_for` in
`crates/iaam-app/src/scenarios/import_session.rs`, and that matcher fills all
three fields at once — the counterparty, the **whole** description, and the word
the source used — joined with "and". A rule made from one shop's row therefore
matches that shop's row and next to nothing else. Answering a hundred questions
produces a hundred rules, and the next statement asks a hundred more. The cost
does not amortise; it repeats.

So the honest path is not merely poorer than the concluding one. At the scale an
import actually has, it is unusable, and no amount of obedience by the caller
fixes that.

**The consequence was an incentive pointing the wrong way.** An agent that obeys
the rule it is given — do not conclude what you were not told — produced a
*poorer* journal than a converter that concludes well, and the poorer journal was
the one that could not be repaired by answering a question, because no question
was asked about a refund. It also produces a questionnaire the length of the
statement. That is why the external agent reached for `build()`, and it is the
defect rather than the agent's mistake. Decision 0006 closed the first half; the
questionnaire is the half that remains.

**A third gap of the same shape is now closed: whose account the far side is**
(decision 0013). `ObservedDirection::Inner` — the value a converter had for a
row a source files as internal — means the movement did not leave the
institution, which is equally true of a payment to a stranger who banks there.
A source asserting the stronger claim, that the far side is an account of the
owner's, had it rounded off at intake, and nothing downstream could separate the
two again. `FarSide` carries it now, beside the direction and beside the
counterparty rather than inside either, and it reaches the observation channel's
published shape for the reason above: the parity rule is that the channel must
be able to say everything the conclusive one can.

It buys the questionnaire back on exactly the rows it is about. A row asserting
it and stating no direction is recorded as a movement between the owner's own
accounts with the far side unnamed, and raises no question — where before it
raised one per row and held the commit.

**One vocabulary gap of the same shape is still open, and it is named rather
than hidden: tax.** `Classification` has no outcome for it, on the deliberate
ground that `classification_of` puts a recorded tax outside rule recalculation
altogether, so an observed tax payment still resolves as a withdrawal. A
converter that says `tax` is still saying something the observation channel
cannot.

Until the questionnaire is settled, the honest arrangement is the current one
stated out loud rather than implied: **the owner's converter concludes about
what a row was, because it is the only place that can do so cheaply.** Not
because it is the only place that *could* — for the cash outcomes, since
decision 0006, it is not. What must not continue is documentation that reads as
though an agent with a CSV could do the same work at the scale a statement has,
and a tool that keeps its own copy of a directory the server now holds. That
second half is decision 0005, and it is done.

## 7. What this does not settle

- Whether an owner can *say* that the far side is his own account without
  naming which. The source can, since decision 0013; he cannot, because every
  answer states a direction and this claim states none. Nothing is blocked by
  the gap — such a row is recorded rather than asked about — but he cannot
  volunteer the assertion where his source failed to make it.
- How two unresolved own-account movements, printed by two banks for one
  economic movement, are related. `GET /v1/transfer-pairings` matches an
  outgoing leg against an incoming one, and neither of these carries a
  direction. Decision 0013 §5 says why admitting the oriented one alone would
  propose transfers nobody made.
- Whether a tax outcome enters the classification vocabulary. Decision 0006
  settled `refund` and the income kind and deliberately left this one standing:
  `classification_of` answers `None` for a recorded tax, so admitting `Tax` here
  would overturn that in passing rather than by decision.
- ~~What a rule minted from an answer should generalise on~~ — taken by
  decision 0008, and the answer is one field: the counterparty where the row
  named one, failing that the word the source used, failing both the whole
  description. Filling all three joined them with «and», so the rule recognised
  the row it was learned from and practically nothing else. The decision is
  proposed rather than accepted, because which of the three carries the
  classification is still the owner's call and 0008 only writes down what the
  code now does and why.
- ~~Whether `--account-map` and `--counterparty-map` are retired against the
  identity decision 0004 gave an account~~ — settled by decision 0005, and the
  answer is that the two files are two different things. `--account-map` is
  retired against that identity. `--counterparty-map` is not an identity file at
  all; it holds the owner's judgement about what a row was, and it is retired
  only once answering a question is cheap enough to be the way that judgement is
  recorded.
- Whether the tool should feed a session rather than the conclusive route. It
  concludes every row, so no question would be raised and the session would buy
  it only the assessment before commit — which is not nothing, and is the whole
  reason the assessment exists.
- Whether `POST /v1/ingest/csv` should be renamed, or deleted. It now declares
  its own source, so the retraction question is closed; what is left is the
  name, which reads as «send your CSV here» and is the reason a bank export
  keeps arriving at it. Renaming breaks every caller to fix a word, so the
  documentation above was tried first. If exports still arrive, the answer is
  deleting the route rather than renaming it: a hand-writable format nobody
  hand-writes is a route with no user. Decision 0019 changes the arithmetic
  rather than the question: once an export has a channel that reads it, sending
  one here is a mistake with an obvious remedy instead of a mistake with none.

## 8. What was rejected

**Teaching the server a bank's export format.** Not rejected on privacy grounds
— two broker parsers already live in tree. Rejected because a bank export needs
the owner's judgement as well as its format, so a server-side parser would have
to take his maps in the request or guess without them; and because every
institution is another format, and every format would be another release of the
server.

**Restating the tool's rules in the agent skill, so an agent could apply them
to pasted rows.** Rejected: `tools/README.md` already fixes that there is one
copy of an importer's rules, because two copies drift and the drift is silent
until an import files the wrong operations. A copy in the document an agent
reads is the worst place for the second one.

**Letting the agent open the export "just to check".** Rejected by `CLAUDE.md`,
and the check it would buy is already bought by fixtures invented end to end.

**Making the queue item name the tool.** Rejected: the queue is computed from
the instance's own state and resolves its addresses from its own contract. An
item that named a Python script in this repository would be a fact about the
owner's laptop published by a server that cannot know it.

This paragraph used to end «what the item can honestly gain is a word for the
converter in the vocabulary of §5, and that is a change to `MissingInput`, not to
a sentence», and that was wrong on both halves. It was written while §6 stood, so
it took the converter for a fixture of the arrangement rather than a symptom of
the parity gap; and it treated *naming the tool* and *naming a shape this API
publishes* as one move. `iaam-tt71` settled it the other way: the item gains a
sentence naming `unresolved_direction`, and `MissingInput` is unchanged. §5
carries the reasoning.

## 9. The third place format knowledge may live

Every row of §1 puts a format in one of two places: in the tree, compiled into
the server, or outside the repository entirely. Decision 0019 adds a third, and
it is not a compromise between them — it is a different answer to the question
§8 asked.

§8 rejected teaching the server an institution's export format, on two grounds.
The first — that a bank export needs the owner's judgement as well as its format
— was answered by decisions 0006 and 0013, which gave the observation channel
everything the conclusive one can say. The second stands and is what 0019 is
built around: **every institution is another format, and every format would be
another release of the server.**

A source profile answers that objection by moving the format out of the release
without moving it out of the product. It is a JSON file validated against
`crates/iaam-ingest/schema/source-profile-v1.json`, and it names columns and
translates the source's own words into iaam's own words. It computes nothing, it
concludes nothing, and it cannot: the engine's output is an `ObservedRow` — the
row as the source stated it — which has no operation kind and no classification
to reach for. So the *format* becomes data and only the *engine* is released.

Three consequences belong in this document rather than in the decision.

**§4 stops being a handicap.** The agent is still handed no statement, no path
to one, and no map. What changes is that the document now travels from the owner
to his own server rather than to a converter on his laptop, so an agent holding
nothing can open the session, read the assessment, relay the questions and
commit. Until now that arrangement bought a poorer import than the one where he
pasted rows; after 0019 it buys a better one.

**The row keeps the identity it used to lose.** `csv_source::parse` derives a key
for an unidentified row from the document digest and the row's own locator, and
decision 0017 refused to do the same inside a session because the "document" a
session knows is a name a caller typed. An engine that reads the bytes has a true
digest and a true line number, so the protection §1's converter rows never had is
restored — and the bytes are kept, so a corrected profile can read the same
document again.

**§8's other two rejections are untouched.** The rules are still in one copy —
one profile per document type, in the tree or beside the deployment, and never a
second copy in a document an agent reads. And the agent still does not open the
owner's export "just to check".

What 0019 does **not** settle is §6's questionnaire. A profile improves the
evidence a row carries — the source's word for the operation, its own category,
its description, the counterparty it printed, its claim about the far side — and
improves nothing about how many questions a row nothing matches will raise. That
remains decision 0008's ground.
