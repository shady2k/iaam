# 0042. A write is one transaction after the network, and a retry is recognised

Date: 2026-11-14 · Status: accepted · Beads: `iaam-mkbfh`

## Context

The owner's principle P3 (`docs/vision.md`, `docs/architecture.md`): the
application survives any interruption, and no timer is the safety. SQLite in WAL
with `synchronous=FULL` already makes a committed transaction survive a kill and
a power loss, and migrations already commit one transaction per version
(`crates/iaam-store/src/schema.rs`).

What breaks the principle is where transactions are opened:

- `Store::append_events` (`crates/iaam-app/src/adapters/sqlite.rs`) commits each
  event of its batch in its own transaction;
- broker sync, import intake, answers and commit, correction batches, operation
  ingest, document upload, category rules with their projection rebuild, account
  creation with its decision history and market price publication each write
  through several calls, so a kill between them leaves part of the request
  written and nothing marking it;
- a request whose commit succeeded and whose response was lost is repeated by
  its client; where the write has no natural key (account creation draws a
  random id) the repeat writes a second time. `OperationKey` in `iaam-server`
  names the authority a call needs (0021, 0025), not a retry.

Files outside SQLite have their own gaps: the outbound tally renames its
generation record before the tally, so a kill between the renames is read as a
rollback and stops broker traffic until a manual repair; the response cache
renames without `fsync` and carries no length or checksum.

## Decision

The owner agreed the direction on 2026-11-14: «Если мы ходим в сеть, то
транзакцию открывать не нужно. Открывать транзакцию нужно когда мы уже получили
данные из сети».

1. **One transaction per request, opened after the network.** A scenario first
   does every network call it needs, then hands the store one closure that
   performs all of the request's reads-for-validation and writes. The port is
   one async method, `Store::transact`, which runs the closure on the store's
   blocking thread inside `BEGIN IMMEDIATE … COMMIT`; the closure receives a
   synchronous `StoreTx` handle. `AppServices` holds `Arc<dyn Store>`, so the
   method takes boxed, type-erased work and returns a boxed result. The store's
   operations that open their own transaction today (journal append, documents,
   categories) gain variants that run inside the caller's. A returned error or a
   panic inside the closure rolls the transaction back (an unwind guard, not
   only the error path). The closure being synchronous makes an `await` on the
   gateway impossible inside it; it does not stop a blocking call, so review
   still reads each closure for one.
   Every port the server wires (`Store`, reference directory, classification
   rules, categories, import sessions, documents) is the one `SqliteAdapter` on
   one connection, so one transaction covers them. The market adapter holds a
   second connection to the same file: market publication and the ingest reads
   that consult the schedule move to the main connection's transaction.
2. **Reads a write depends on run inside the same transaction** and see what it
   already wrote (sync's transfer validation and full-redemption arrival;
   import commit's session state and revision). A plan computed before the
   transaction is revalidated inside it, and a changed state refuses the request
   rather than writing on a stale reading.
3. **`append_events` commits its whole batch at once**, as a thin call of
   `transact`; order within the day, duplicate outcomes and assigned sequences
   stay as they are for a clean batch.
4. **A retry is recognised, not repeated, and gets the first answer.** Where a
   write has a natural key — the broker's operation id, an import session's
   commit, a correction's target, a document's content hash — a repeat after a
   lost response finds it before any state-dependent refusal, checks that the
   payload is the same command, and answers with the result the first call
   returned (stored or reconstructed), not with `Duplicate` or "session not
   open"; a different payload under the same key is refused. A broker row with neither id nor key gets a
   derived key from its content and its occurrence index within a stated
   statement boundary, only where the channel guarantees a stable order of
   identical rows across overlapping fetch windows; otherwise the row stays
   refused as ambiguous, never guessed. Where no
   natural key exists (account creation, today), the route takes an
   `Idempotency-Key` header; the key, a hash of the body and the response are
   stored in the same transaction as the write, a repeat with the same key and
   body returns the stored response, and the same key with another body is
   refused.
5. **The outbound tally** renames the tally before its generation record. On
   read, a tally exactly one generation ahead of its record is an interrupted
   write: the record is completed and made durable before any further update,
   under the same exclusive lock, identity checks and process high-water mark.
   The directory is `fsync`ed after the tally rename and again after the record
   rename or repair, so the order survives a host crash too. A first write
   interrupted with the record still empty is handled the same way. A tally
   behind its record stays a refused rollback. No allowance is ever reset.
6. **A response-cache entry** carries a format version, its length and a
   checksum over metadata and body, checked on read; a mismatch or an entry in
   the old format is discarded; the file is `fsync`ed before its rename and the
   directory after it.
7. **The proof** is a child-process kill harness: test-only barriers at named
   points (inside a transaction before commit, after commit before the
   response, between the tally renames, during the tally repair, during cache
   publication, inside a migration) where the child touches a file and waits,
   and the parent kills it, reopens the store, repeats the identical request and
   compares both the state and the response with a clean run's, by meaning:
   generated ids and timestamps are matched, not compared, and retained leases
   and outbound-attempt accounting are expected to differ. Malformed cache
   entries and a tally repair killed repeatedly are tested directly. A kill
   proves recovery from a process crash, not from a power loss: power-loss
   ordering rests on the `fsync` points above. Invented data and test doubles
   only.

What stays as decided before: the one-hour host closure after an unresolved
outbound attempt and the market sync lease are availability limits, not data
safety, and are kept. The lease is therefore the one write before the network:
market sync takes its lease in its own transaction, fetches, then publishes
prices, accrued interest and the run's completion in one transaction.

Reviewed by codex against the code on 2026-11-14 (verdict: matches with
changes); the changes are folded in above.

## Rejected

- **A run journal** (started / completed / interrupted) closed at restart: more
  states, and a restart that must clean up after itself. Needed only where one
  request's write cannot fit one transaction; none here does.
- **A transaction held across network calls:** the write lock would be held for
  the duration of a broker call, every other writer would wait or time out, and
  a timeout would again be the safety.
- **Committing a large batch in chunks:** each chunk is a partial write by
  another name.

## Consequences

- A killed request leaves nothing, and its repeat starts clean or is recognised.
- Scenarios are reordered: network first, then one `transact` call. The pure
  core is unchanged.
- A large import commit is one longer transaction; other writers wait for it.
  Input size stays bounded by the import session's own limits.
- `Idempotency-Key` is new API surface on creating routes without a natural key;
  `docs/api/conventions.md` describes it.
- Restoring both tally files from an old backup is not detected, as before; the
  generation record detects a tally restored alone.
