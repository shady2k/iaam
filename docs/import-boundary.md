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

It cannot, with what it has. `MissingInput` publishes who supplies a field in
three words — the owner, an external document, the caller — and the rows are not
a missing field at all: they are the body of a later call that the item does not
describe. The two fields it does publish, the channel and the label, are exactly
the two that *are* fields. So the item presupposes a converter, and the
vocabulary it is written in has no word for one.

For the owner running his tool the presupposition holds. For an agent holding
pasted rows it holds only through the observation shape — a row submitted as
what the source stated, with the source's own sign and direction word, which the
server settles or asks about. The item never mentions that shape, and the only
worked example in the repository, the tool, does not use it.

## 6. Where the line should be, and the three things that must move first

**A converter translates a format. The API reaches conclusions.** That is the
line, and it is not the line today.

It cannot simply be moved, because the conclusive channel beats the observation
channel in three ways. Two are about what a row may *say*; the third is about
what saying it *costs*, and the third is the one that decides the question.

**Two outcomes the observation channel cannot express.**

- `Classification` has four outcomes — internal transfer, external flow, fee,
  income — and **no refund**. `OperationKindDto` has one. A row submitted as an
  observation can therefore never come out as the thing a positive row with a
  spending category is, and the tool's refund rule has nowhere to go.
- An observation resolved as income always carries **no income kind**, on the
  correct ground that the source named none. The tool names one where the bank's
  own category says so, and that naming is lost on the observation path.

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

**The consequence is an incentive pointing the wrong way.** An agent that obeys
the rule it is given — do not conclude what you were not told — produces a
*poorer* journal than a converter that concludes well, and the poorer journal is
the one that cannot be repaired by answering a question, because no question is
asked about a refund. It also produces a questionnaire the length of the
statement. That is why the external agent reached for `build()`, and it is the
defect rather than the agent's mistake.

Until all three are settled, the honest arrangement is the current one stated
out loud rather than implied: **the owner's converter concludes about what a row
was, because it is the only place that can do so cheaply.** What must not
continue is documentation that reads as though an agent with a CSV could do the
same work. It no longer keeps its own copy of the directory: that half is
decision 0005, and it is done.

## 7. What this does not settle

- Whether `refund` and an income kind enter the classification vocabulary. A
  rule outcome that carries a direction must answer `implied_movement`, and a
  refund's direction is not the same question as a fee's; that is its own
  decision and probably its own record here.
- ~~Whether `--account-map` and `--counterparty-map` are retired against the
  identity decision 0004 gave an account~~ — settled by decision 0005, and the
  answer is that the two files are two different things. `--account-map` is
  retired against that identity. `--counterparty-map` is not an identity file at
  all; it holds the owner's judgement about what a row was, and it is retired
  only once the observation channel can carry that judgement, which is the three
  gaps in §6.
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
owner's laptop published by a server that cannot know it. What the item can
honestly gain is a word for the converter in the vocabulary of §5, and that is a
change to `MissingInput`, not to a sentence.
