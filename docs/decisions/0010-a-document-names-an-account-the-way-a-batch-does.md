# 0010. A document names an account the way a batch does

Date: 2026-09-04 · Status: proposed · Bead: `iaam-w49n`

## Context

Two routes ask the owner's system one question — «which of his accounts is this
printed string» — and until now they answered it in two vocabularies, neither of
which said so.

`POST /v1/ingest/operations` resolves a row's account through the tiering of
decision 0004: iaam's own identifier for the account, then the identifier the
account's source prints for it and the dated aliases that reach the same
account, then the owner's title. A string that reaches nothing is refused with a
full sentence — *an account of the owner's, named by its iaam identifier or by
the identifier its source prints for it*.

`POST /v1/ingest/csv` did not. `build_directory` in `iaam-server` built the
parser's table by keying every account on `account.title`, and the parser's
generic `lookup` refused an unresolvable cell with `expected: "directory name"`.
That string names no vocabulary at all: a caller holding a statement learns from
it neither what failed nor what to send instead, and «this account does not
exist» is exactly what it sounds like.

So the same caller could name an account by the number its bank prints on one
route and had to use the owner's title on the other, and the refusal on the
route that accepted less was the one that explained less. This is the trap
`iaam-varx` closed on one route and left open on the other: an agent that learns
the printed identifier works will send it to the document route.

Three further facts shaped the answer rather than being incidental to it.

**The table is not the CSV route's alone.** `Directory.accounts` is also what
the two broker report parsers resolve against, through `resolve_named_account`
in `report/tinkoff.rs` and `report/finam.rs`. A broker report prints an
agreement number, not a title the owner typed, so the tier that was missing is
precisely the one those parsers most need. Whatever is decided here is decided
for `POST /v1/documents` too.

**The tiering already ends in a title.** `AccountDirectory::candidates` tries
the title last, trimmed and case-insensitively. Adopting the tiering therefore
*adds* two vocabularies to the document route rather than trading one for
another — a fact worth stating plainly, because it is what makes the change
nearly non-breaking.

**`lookup` was generic and served three tables.** Accounts, places of custody
and instruments all reached it, which is why two entirely different failures
shared one meaningless sentence. Instruments had already grown their own
refusals; accounts and places of custody had not.

## Decision

**One tiering, one wording, and it lives in the crate that parses documents.**

1. The tiering moves down into `iaam-ingest`, as
   `csv_source::AccountNames::{candidates, resolve}`, together with the sentence
   a refusal is worded in. `iaam-app`'s `AccountDirectory` keeps the owner's
   account views — a declaration is answered with one — and delegates the
   question. The direction is forced: a document is parsed in `iaam-ingest`,
   which cannot see `iaam-app`, so leaving the tiering above would have meant a
   second copy of it below. A second copy is what the defect was.

2. `Directory.accounts` stops being a `BTreeMap<String, Vec<AccountId>>`. A map
   pools every vocabulary into one key space: it cannot say which vocabulary
   matched, so it cannot let one beat another, and where a title happens to
   equal another account's printed identifier it turns the agreement into a
   collision. The vocabularies are searched in order instead.

3. The translation from a stored account to a vocabulary — which field is an
   identity, which is a name, which carries an interval — happens once, in
   `entry_for`, and `build_directory` takes the built table from
   `AccountDirectory::names` rather than assembling its own.

4. **The title tier survives, and is not advertised.** It resolves, so every
   document written before this change keeps parsing; the refusal names only the
   two identifiers, because `docs/api/conventions.md` §3.2 is that a name is not
   an identity, and a caller told to send a title has been told to depend on a
   string the owner may rename tomorrow.

5. Places of custody keep a title lookup and get their own refusal — *a place of
   custody of the owner's, named by the title he gave it*. There is no tiering
   there and that is not an omission: no source prints an identity for a
   depository, so a single vocabulary is the whole truth, and a tier structure
   over one vocabulary is a decision that never applies while looking like one
   that does. Instruments were left alone; they already refuse in their own
   words, and they distinguish an unknown code from a code outside its interval,
   which is a distinction neither of the other two tables has.

## Consequences

**A caller may now name an account in a document the way its source prints it,
and by iaam's own identifier.** That is the point, and it makes the document
route usable by an agent that read the account list once.

**It is breaking in two narrow ways, and both are behaviour changes rather than
errors.**

- A document cell that is one account's title *and* another account's printed
  identifier now resolves to the second. Before, it resolved to the first. The
  tiering is deliberate — an identity beats a name — but a caller relying on the
  old answer gets a different account, silently, which is the worst shape a
  change can take. Nothing in the codebase can detect the collision for him: it
  exists only in his data.
- Two accounts whose titles differ only in case, or in surrounding whitespace,
  used to be two distinct keys and now collide. The row is refused rather than
  guessed at, and the refusal names both accounts, so this one is visible.

**A title that reaches two accounts is refused where it used to be refused with
less to go on.** The new refusal names the accounts it reached and says what
would settle it. The failure mode is unchanged; what the owner can do about it
is not.

**The document route's refusal is now the batch route's refusal, word for word.**
A contract test asserts the two strings against each other rather than against a
literal, so a later change that rewords one and not the other reopens the trap
even if both wordings are individually good.

**What would falsify this.** If the owner finds that his documents now resolve
to accounts he did not mean — because a bank's printed identifier for one
account is another account's title in his own directory — then the title tier is
not the harmless compatibility layer this decision assumes, and the answer is to
retire it behind an explicit declaration rather than to reorder the tiers.
