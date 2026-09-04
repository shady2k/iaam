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
- `Описание`: copied verbatim to `description` and used to identify internal
  transfer legs.

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

With `--account-map` instead, the preview contacts nothing at all, because the
file is its own contour:

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
