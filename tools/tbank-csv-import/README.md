# T-Bank CSV import

This tool converts a T-Bank operations export into iaam operations. It knows
only the institution's export format. Account names and identifiers are runtime
inputs, not tool data.

It is a single Python 3 script using only the standard library, and every input
is an explicit argument. No agent, framework or editor is required to run it:
`python3 tools/tbank-csv-import/import.py --help` is the whole interface.

## Export format

The export is semicolon-separated CSV with UTF-8 headers. The importer reads
these source columns:

- `Имя счёта`: account name used only to look up the runtime account mapping.
- `Дата операции`: `DD.MM.YYYY HH:MM:SS`, converted to the operation date.
- `Сумма в валюте счёта`: signed account-currency amount; negative is a
  `withdrawal`, positive is a `deposit`.
- `Категория по-умолчанию`: copied verbatim to `source_category`.
- `Ваша категория`: copied verbatim to `owner_category` when present.
- `Описание`: copied verbatim to `description` and used to identify internal
  transfer legs.

`source_category` and `owner_category` are evidence from two different
vocabularies. The importer does not translate, normalise or map either value;
the owner's rules decide what the recorded words mean.

All other export columns are retained only as part of the raw row text used for
idempotency. The importer does not translate or normalise source category or
description values.

## Which account a printed name is

**Declare it once, on the account, and pass no file.** The export prints a name
in its account column. Give that exact string to the matching iaam account as
its `provider_account_id`, or as an alias if the account already has an
identity — an alias carries a validity interval, which is how a card that was
replaced is recorded. From then on this tool recognises the export's accounts by
asking `GET /v1/accounts`, and the accounts it does not recognise are outside
the contour.

An export account name that identifies none of your accounts is reported on
stderr before anything is submitted, and its rows are counted as
`skipped_outside_contour`. **Read that list.** A statement's whole month is
dropped in silence otherwise, and the fix — declaring the identifier on the
account — takes one call.

`--account-map` still works and is the fallback: it maps export names to iaam
**titles** and resolves by title, so it breaks on a rename at either end. That
is the defect decision 0004 was written about and decision 0005 retires. It is
kept for an offline preview, which has no directory to read, and for an operator
who has not finished declaring identities.

## Run

Preview. Nothing is written; the directory is read to work out the contour:

```bash
IAAM_TOKEN=... python3 tools/tbank-csv-import/import.py \
  --export /path/to/export.csv \
  --base-url http://127.0.0.1:8080 \
  --token-env IAAM_TOKEN \
  --channel file \
  --dry-run
```

Submit after reviewing the dry-run summary:

```bash
IAAM_TOKEN=... python3 tools/tbank-csv-import/import.py \
  --export /path/to/export.csv \
  --base-url http://127.0.0.1:8080 \
  --token-env IAAM_TOKEN \
  --channel file \
  --submit
```

If that import was retracted, do not retry `--submit` with another channel.
Use replacement mode. It filters the journal by the original declaration —
account, channel and label — and reads that import's rows to recover each
event identity. It then sends the same converted rows to
`POST /v1/corrections` as `relation: replacement` with fresh idempotency keys:

```bash
IAAM_TOKEN=... python3 tools/tbank-csv-import/import.py \
  --export /path/to/export.csv \
  --base-url http://127.0.0.1:8080 \
  --token-env IAAM_TOKEN \
  --channel file \
  --replace-retracted
```

The original channel is never changed to make withdrawn keys available again.
The declaration is the identity the importer already holds; the published
event and import UUIDs remain available for callers that hold either one.
A replacement is a correction fact, so its journal provenance records the
correction route while the withdrawn source fact remains the one it replaces.

Each replacement key is `correction/replacement/<withdrawn event id>`, so a
repeated replacement run addresses the same correction keys and cannot add a
second replacement.

`--replace-retracted --dry-run` performs the same journal reads without posting
corrections and adds a `replacements` summary with per-account row and event
counts. It must resolve the live account directory even when `--account-map` is
present; only an ordinary offline `--dry-run` avoids the directory.

With `--account-map` on an ordinary dry run, the preview contacts nothing
because the file is its own contour:

Journal reads request the route's maximum page size of 200. If the server
returns `429`, the importer reads `Retry-After`, says that it is waiting while
keeping the rows already fetched, then retries the same cursor and continues
without restarting the traversal.

```bash
python3 tools/tbank-csv-import/import.py \
  --export /path/to/export.csv \
  --account-map /path/to/account-map.json \
  --channel file \
  --dry-run
```

Operations are sent to `POST /v1/ingest/operations`, batched by resolved
account, with the declared source `{account, channel, label}`. The label is
`tbank-export <file name>`: it names this import within the account and
channel, so `POST /v1/corrections/imports` repeating the same three values
retracts exactly this run and leaves other months imported through the same
account and channel in force. Two runs of the same export file are one import
and the second adds nothing.

The JSON summary includes an `accounts` list for both dry runs and submissions.
Each entry names the destination account, counts its converted rows, and gives
money in (`inflow`), money out (`outflow`), and net movement (`net`). The row
count is intentionally the converted-row count; a paired transfer's discarded
second leg remains visible in the top-level `dropped_second_leg` counter. The
`operations` entries also carry the same display name, so a preview shows both
the per-row destination and the per-account movement.

### Locating a folded or unpaired transfer leg

A counter says how many rows were affected; it does not say which rows, and a
search by date and amount in the source file is not a reliable way to find out.
Two lists exist so that never has to happen:

- **`dropped_legs`**, one entry per row `dropped_second_leg` counts: the second
  leg of a matched internal-transfer pair, which is folded into a single
  `transfer` operation rather than submitted on its own. Each entry gives
  `line`, `account`, `date`, `amount` and `description` from the dropped row
  itself, `folded_into` — the `idempotency_key` of the operation it was folded
  into — and `event_id`, which is `null` on a dry run and filled in with that
  operation's own verdict once the batch is submitted. `folded_into` and
  `event_id` locate the *surviving* operation in the journal; the other fields
  locate the *dropped row* in the source file.
- **`unmatched`**, one entry per row `unmatched_legs` counts: an
  internal-transfer leg with no partner within the pairing tolerance, dropped
  rather than guessed into an operation. Each entry gives `line`, `account`,
  `date`, `amount`, `description` and `idempotency_key` — the identifier the
  row would carry were it ever submitted on its own. An unmatched leg is never
  submitted, so this key never appears in a verdict; it is here so the row is
  still identifiable, beyond date and amount, once the pairing tolerance is
  corrected and the export is run again.

## Checking the tool

The fixtures are invented from end to end and exercise every rule below. A
change to the importer is wrong until this reproduces `expected-summary.json`
field for field:

```bash
python3 tools/tbank-csv-import/import.py \
  --export tools/tbank-csv-import/fixtures/synthetic-export.csv \
  --account-map tools/tbank-csv-import/fixtures/account-map.json \
  --counterparty-map tools/tbank-csv-import/fixtures/counterparty-map.json \
  --dry-run
```

The expected summary was derived from the rules, not from the code. If they
disagree, the importer is wrong until proven otherwise — do not edit the
expectation to match the output.

Three things in the fixture exist to catch specific mistakes, and removing any
of them makes it easier than reality:

- **two identical rows on one day.** They are legitimately two facts, and they
  are what the ordinal in the row key exists for.
- **transfer pairs one second apart.** A bank posts the two legs separately;
  matching on an exact timestamp finds almost nothing.
- **a positive row carrying a spending category.** That is a refund, not an
  arrival.

## Internal transfer legs

The bank emits an internal transfer as two rows with `Описание` equal to
`Между своими счетами`: one negative leg and one positive leg. The importer
matches equal absolute amounts to the nearest opposite-sign leg within five
seconds and emits one `transfer` operation. The second leg is counted as
`dropped_second_leg`.

If only one side of a pair is inside the runtime contour, the inside leg is
emitted as its ordinary `withdrawal` or `deposit`; the outside leg is counted as
`skipped_outside_contour`. Rows belonging entirely to outside accounts are
also counted and skipped. An internal-transfer leg with no match is reported on
stderr and counted as `unmatched_legs`; it is never guessed into an operation.

## Idempotency

For every submitted row, the importer uses exactly:

```text
<account-id>/<channel>/<sha256 of the raw row line>/<ordinal within the day>
```

The hash covers the raw CSV row text, encoded as UTF-8. The ordinal is
one-based within the `(account, day, raw-row-sha256)` group. It disambiguates
only genuinely identical rows: two identical purchases on one day receive
ordinals `1` and `2`, while an unrelated row on that day receives ordinal `1`
and does not change either purchase's key. Account and channel are required
because iaam compares idempotency keys globally per owner; omitting either can
make identical rows from two institutions collide. The key is stable when the
same export is imported again, including when an overlapping export adds an
unrelated row.

## Verdicts, duplicates and collisions

`--submit` and `--replace-retracted` add a `verdicts` count for every verdict
kind the API returned (`accepted`, `provisional`, `duplicate`,
`possible_duplicate`, `discrepancy`, `needs_reconciliation`, `rejected`), plus
two summaries carried forward from that same tally:

- **`already_known`** is the plain count of `duplicate` verdicts — how many
  submitted rows the journal already held.
- **`duplicates`** is that same set of rows, named rather than counted: one
  entry per duplicate verdict, each an `{idempotency_key, event_id}` pair. The
  API already returns the event a duplicate collided with on every such
  verdict; this is that value kept rather than thrown away.
- **`duplicate_collisions`** calls out the case a count hides: many different
  rows answered `duplicate` against the *same* `event_id`. One export
  genuinely re-imported is one row per collision; many rows collapsing onto
  one event is almost always an idempotency-key defect — a carried or `None`
  component that came out the same for rows that are not in fact the same
  operation — and this list, plus a matching line on stderr, exists so that
  does not have to be inferred from `already_known` after the fact. Each entry
  gives the `event_id`, the `row_count` that collided with it, and the
  `idempotency_keys` involved.

All three are absent from a `--dry-run` summary: no verdict exists until
something is submitted.

## What this tool must never contain

No account name, account identifier, balance, counterparty or row of anybody's
export. All of those are run-time inputs: `GET /v1/accounts` supplies the
identity each account was declared under, and `--counterparty-map` supplies the
one judgement an export cannot.

The reason is not tidiness. This file is checked in and the repository is
public, so a value written here is published, and a value published in a commit
outlives the commit that removes it.

## Income the balance earned, not money that arrived

Two of the bank's own categories mark money the balance produced rather than
money someone sent: `Проценты` and `Бонусы`. They are submitted as `income`, not
as a deposit. Counting them as arrivals overstates what actually came in from
outside, and a returns calculation that reads them as contributions is corrupted
outright. `Проценты` carries the income kind `deposit_interest`; `Бонусы` carries
none, because the export does not say enough to name one and inventing a kind
would record an invention in the journal.

## Counterparties that are the operator's own accounts elsewhere

`--counterparty-map` takes `{"<description in the export>": "<account title>"}`.
A row whose description matches becomes a transfer between two accounts inside
the contour instead of money crossing its boundary: money leaving the
statement's account goes to the named one, and money arriving came from it.

This is **operator knowledge, not bank knowledge**. Nothing in an export
distinguishes a payment to a stranger from a top-up of the same person's account
at another bank — both are a name and an amount — so the answer arrives per run
and is never written into this tool.

The effect is not marginal. Where someone moves money between their own banks, a
single such counterparty can carry more than the month's entire genuine inflow,
and without the mapping every rouble of it is reported as income and spending
that never happened.

The row key still carries the **statement's** account, because it identifies the
row, which belongs to that statement whichever account the resulting operation
is filed on.

## A positive row is not automatically money arriving

The export marks a merchant's refund with the merchant's own spending category
and a positive amount. Only `Переводы` and `Пополнения` mean money somebody
actually sent; every other spending category on a positive row is the merchant
giving money back, and it is submitted as `refund`.

A refund reverses the purchase: the flow report subtracts it from what went out
and from the category it was spent in, and never adds it to income.

The small case is a card's one-unit authorisation check and its same-day
reversal, which every card issuer performs; without the rule the report claims
money arrived from outside when nothing did. The large case is a returned
purchase, where the same mistake is worth the price of the goods.
