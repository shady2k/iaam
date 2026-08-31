# T6: a securities transfer is not cash — design

Bead: `iaam-e00u`. Parent epic: `iaam-zn38`. Date: 2026-08-31.

Parent design: `.internal/specs/2026-08-31-tinkoff-execution-facts-design.md`
§10. Preceding tasks T1–T5. Line numbers are against the
`t1-order-state` worktree, which carries them uncommitted.

Reviewed adversarially by codex on 2026-08-31, together with T5. Three
findings on this document, all accepted, one of them a factual error that
changed the decision: `origin = 'owner'` does not mean *an* owner's
decision, because the dictionary has no owner in its key at all — so
preserving such a row would have left every other owner of the same
installation receiving fabricated cash. The repair predicate promised in
§5 was also not recoverable from stored facts, and the migration's
promised reporting is not something a migration can do here.

## 1. Problem

`OPERATION_TYPE_INPUT_SECURITIES = 17` is *«Перевод ценных бумаг из
другого депозитария»* and `OPERATION_TYPE_OUTPUT_SECURITIES = 3` is
*«Вывод ЦБ»* (`operations.proto:305,320`). Both move **securities**.

The seeded dictionary maps them to `deposit` and `withdrawal`
(`iaam-broker/src/tinkoff/dictionary_seed.rs:50,56`), and the adapter
builds a cash movement from each. Money that never moved enters an
append-only journal, where the only correction is a reversal.

## 2. Why deleting the seed entries is not the fix

The seed is explicitly not the source of truth after first insertion
(`dictionary_seed.rs:9-11`), and `extend_broker_operation_kinds` inserts
with `ON CONFLICT (broker, source_kind) DO NOTHING`
(`iaam-store/src/broker_operation_kinds.rs:86-92`). An installation that
already configured broker access keeps the wrong mapping for ever, and
the seed change silently protects only installations that do not exist
yet.

## 3. Why hard-coding the two codes in the adapter is also not the fix

The parent design said the refusal "belongs in the adapter, where these
kinds are refused with a named reason regardless of what the dictionary
says". Read as a string comparison on `OPERATION_TYPE_*_SECURITIES`, that
is the mistake T2 already made once and had to undo: the mapping from a
broker's code to a meaning lives in the dictionary, in data, and not in a
`match`, because the code set is open and belongs to the broker
(`iaam-broker/src/operation_kind.rs:9-14`). A hard-coded pair would also
miss every future alias — and this contract already has five spellings of
"deposit".

## 4. Decision: a meaning, a migration, and a refusal

Three parts, and all three are needed.

**A meaning.** `ChannelOperationKind` gains two members —
`SecuritiesTransferIn` and `SecuritiesTransferOut` — with dictionary
names `securities_transfer_in` and `securities_transfer_out`. Two rather
than one because `Deposit` and `Withdrawal` are already two, because the
refusal reason can then say which direction was refused, and because a
future securities-transfer fact will need the direction: collapsing them
now would mean migrating the dictionary rows a second time to get it
back.

Adding a member is intended to break compilation wherever parsing is
incomplete, which is the property that enum documents
(`operation_kind.rs:13-15`).

**A migration.** A store migration rewrites existing
`broker_operation_kinds` rows for these two source kinds from
`deposit`/`withdrawal` to the new names — **regardless of origin**,
including `origin = 'owner'`.

That needs justifying, because it is the only place in this epic that
overwrites an owner-origin row, and the write path for those rows says
the opposite: "updating the dictionary from the contract has no right to
overwrite the owner's decision" (`broker_operation_kinds.rs:25-36`).

Two facts make this the exception rather than a violation.

First, the table is not per owner and was never meant to be. Its key is
`(broker, source_kind)` with no owner column, deliberately: "Словарь —
факт о брокерском API, а не о владельце… Владельческий столбец сделал бы
один и тот же факт разным у разных владельцев"
(`iaam-store/migrations/0009_broker_operation_kinds.sql:11-15`), and
`set_broker_operation_kind` accepts no owner identifier
(`broker_operation_kinds.rs:116-119`). So an `owner`-origin row is not
*an* owner's decision, it is *the installation's* decision, and
preserving it would keep fabricating cash for every other owner on that
installation because of a mapping one of them once set. An earlier
revision of this document said such an owner "keeps it"; that was wrong
in a way that mattered.

Second, an override exists for codes whose meaning the contract leaves
open — "the owner knows about their portfolio what the contract does not"
(`broker_operation_kinds.rs:112-115`). The contract does not leave these
open. It names them *«Перевод ценных бумаг из другого депозитария»* and
*«Вывод ЦБ»* (`operations.proto:305,320`). A mapping from those to cash
is not knowledge about a portfolio; it is a statement that securities are
money, and §4.9 does not allow the journal to record it.

The wider problem — that the table cannot express one owner's decision at
all — is filed as `iaam-8d7g`. T6 does not solve it; it declines to be
blocked by it.

The migration is plain SQL run through `execute_batch` like every other
(`iaam-store/src/schema.rs:49-61`), so it reports nothing and no
acceptance criterion may claim it does. What it changed is observable
where it matters — in the dictionary rows themselves, and in the
adapter's behaviour afterwards.

The seed entries change too, for installations that do not exist yet.

**A refusal.** The adapter refuses both new meanings with a reason naming
securities and the direction — an unsupported kind, quarantined per row,
not an error that aborts the batch.

## 5. What is deliberately not built

A securities-transfer **fact**. The journal's corporate-action variants
are redemption, amortisation and conversion only
(`iaam-core/src/event/corporate_action.rs:22`), and a transfer is none of
them. Modelling one is a change to the journal's vocabulary and cannot be
smuggled in through a broker adapter; it is filed as `iaam-2y5k`.

So T6 is a **loss of coverage and a gain in truth**: an installation that
today records a fabricated cash deposit will, after T6, record nothing
and show the owner a quarantined row saying why. That is the intended
outcome, and it is stated here so that the change is not mistaken for a
regression when the number of accepted rows falls.

Existing facts already recorded from these codes are **not** repaired by
T6, for the same reason T4 did not repair its own: the repair needs an
entry point that does not exist. Filed as `iaam-2y5k` alongside the fact
model, because repairing them without a fact to convert them into would
only delete them.

**And they cannot be found from the journal alone.** The adapter keeps
the operation id and discards `source_kind`
(`iaam-app/src/adapters/tinkoff.rs:223,264`); normalization reduces these
rows to `CashIn`/`CashOut` events indistinguishable from a real deposit
(`iaam-ingest/src/operation.rs:291`); and `Provenance` retains the
source, the raw hash, the parser version and the source operation id —
not the broker's operation code
(`iaam-core/src/event/provenance.rs:51-58`). So the repair cannot be a
predicate over stored facts the way T4's was. It has to re-fetch the
interval from the gateway and match on `source_operation_id`, or be
driven by the owner. `iaam-2y5k` says so; a bead that promised an exact
event predicate would have sent someone looking for data that is not
there.

## 6. Not changed by T6

- `TINKOFF_PARSER_VERSION` stays `tinkoff-api/3`: T6 changes which rows
  become facts, not how a fact is constructed.
- The dictionary mechanism itself, and every other seeded mapping.
- Owner-origin dictionary rows.
- `sync_broker`'s handling of quarantined rows.

## 7. Acceptance criteria

1. `ChannelOperationKind` has `SecuritiesTransferIn` and
   `SecuritiesTransferOut`, with names that round-trip through the
   dictionary's name mapping.
2. The seed maps `OPERATION_TYPE_INPUT_SECURITIES` and
   `OPERATION_TYPE_OUTPUT_SECURITIES` to them.
3. The adapter quarantines both, with a reason naming securities and the
   direction, and does not abort the batch.
4. A migration rewrites existing rows for those two source kinds
   regardless of origin, including `owner`.
5. After the migration, no dictionary row maps either code to `deposit`
   or `withdrawal`, whatever its origin was before.
6. No cash movement is produced for either code, on any installation.

## 8. Tests

- both members round-trip through `name()` and `from_name()`;
- the adapter quarantines an `INPUT_SECURITIES` row naming securities and
  the inbound direction, and an `OUTPUT_SECURITIES` row naming the
  outbound one;
- a quarantined securities transfer does not stop an accepted row in the
  same response;
- the migration rewrites a seeded `contract` row **and** an `owner` row
  for the same `source_kind` — the case §4 exists for, asserted on the
  store rather than argued;
- an `owner` row for an unrelated `source_kind` is left untouched by the
  same migration, so the exception is proven narrow rather than assumed
  to be;
- the seed contains no mapping from either code to `deposit` or
  `withdrawal`.

## 9. Gates

`cargo check` and the crates' own tests, plus `iaam-store` because the
migration lives there. The workspace gates run once at the end of the
epic.
