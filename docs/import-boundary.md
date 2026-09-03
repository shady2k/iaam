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

Two of those rows are read wrongly often enough to be worth naming.

**`POST /v1/ingest/csv` does not accept a bank's export.** Its columns are
iaam's — `date`, `type`, `account`, `currency` and the optional rest — and its
accounts are resolved by *name* through the directory. It is a hand-writable
format, not a bridge from anybody's institution. It also declares no source, so
its rows arrive under an identity minted for that one request and
`POST /v1/corrections/imports` cannot reach them afterwards. Sending a bank
export to it does not half-work; it rejects every row.

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
A declaration may name the account by the number the bank prints, and the
response hands back the identifier the rows must carry. `--account-map` is the
pre-0004 workaround kept alive, and that decision named this exact situation as
its own falsification: *"the owner finds himself maintaining a file that maps a
source's identifier to a `provider_account_id`."*

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

## 6. Where the line should be, and the two things that must move first

**A converter translates a format. The API reaches conclusions.** That is the
line.

It could not simply be moved, because the conclusive channel used to be strictly
more expressive than the observation channel. Decision 0005 closed that, and what
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

The consequence was an incentive pointing the wrong way. An agent that obeyed the
rule it is given — do not conclude what you were not told — produced a *poorer*
journal than a converter that concluded well. That is why the external agent
reached for `build()`, and it was the defect rather than the agent's mistake.

**One gap of the same shape is still open, and it is named rather than hidden:
tax.** `Classification` has no outcome for it, on the deliberate ground that
`classification_of` puts a recorded tax outside rule recalculation altogether, so
an observed tax payment still resolves as a withdrawal. A converter that says
`tax` is still saying something the observation channel cannot.

So the arrangement is no longer "the owner's converter concludes, because it is
the only place that can". It concludes because it is the place that *has* the
statement, and for the cash outcomes it is no longer the only place that could.
What must not continue is a tool that keeps its own copy of a directory the
server now holds.

## 7. What this does not settle

- Whether a tax outcome enters the classification vocabulary. Decision 0005
  settled `refund` and the income kind and deliberately left this one standing:
  `classification_of` answers `None` for a recorded tax, so admitting `Tax` here
  would overturn that in passing rather than by decision.
- Whether `--account-map` and `--counterparty-map` are retired against the
  identity decision 0004 gave an account, or kept as the owner's private
  shorthand. Retiring them moves the owner's judgement into rules he can read
  and retire; keeping them leaves it in two files nothing versions.
- Whether the tool should feed a session rather than the conclusive route. It
  concludes every row, so no question would be raised and the session would buy
  it only the assessment before commit — which is not nothing, and is the whole
  reason the assessment exists.
- Whether `POST /v1/ingest/csv` should accept a declared source, so its rows
  become retractable as an import like every other channel's.

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
