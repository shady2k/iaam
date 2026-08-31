# T7: the channel's money arithmetic moves into the core — design

Bead: `iaam-0y8q`. Parent epic: `iaam-zn38`. Date: 2026-08-31.

This task changes **no rule**. T2 §6 and T3 §2–§4 decided what the
arithmetic computes, and those decisions stand. T7 moves where it runs.

## 1. Problem

`scripts/check-architecture.sh` guard 9 refuses monetary arithmetic in
`crates/iaam-app/src` and `crates/iaam-server/src`: "every number in an
API response comes from core (§3.1, §13)". `iaam-ingest` is excluded on
purpose — it collects a fact from source fields rather than calculating a
result, "and prohibiting addition would make it impossible to implement"
(`check-architecture.sh:204-219`).

T2 and T3 put the channel's arithmetic in
`crates/iaam-app/src/adapters/tinkoff.rs`. Eleven hits: summing trade
quantities (`:370`, `:614`), the per-fill product (`:459`, `:525`,
`:629`), the completeness comparison (`:466`), and the largest-remainder
allocation (`:638`, `:644`, `:648`, `:666`).

The guard is **textual** — it greps for `.checked_add(`, `.checked_sub(`,
`.checked_mul(`, `.try_add(`, `.try_sub(`, `.checked_negate(`. So calling
a core primitive from the adapter still trips it, and that is the
intended reading rather than a limitation of the grep: the shell must not
**compose** arithmetic. A composed calculation is a rule, and rules live
in the core where they can be tested without a broker.

## 2. Decision

Four named operations move to `iaam-core`. The adapter calls each once
and performs no arithmetic itself.

Placement: a new module `iaam-core/src/rules/trade_allocation.rs`, except
where an operation is plainly a money primitive, which goes to
`money.rs` beside its siblings. The rules directory is where a
computation that encodes a decision already lives; `money.rs` is where a
representation-level operation lives.

1. **Total executed quantity** — sum of a slice of quantities, refusing
   overflow. A primitive: `money.rs`, beside `Money::sum`.
2. **Per-fill gross** — exact `price × quantity` as `CalcMoney`, no
   rounding. A primitive: `money.rs`, beside `CalcMoney::checked_mul`
   and `checked_mul_quantity`, which it will use.
3. **Order completeness** — given the per-fill grosses and the order's
   accrued interest, produce the expected order total and compare it
   with the reported payment, returning the two amounts on a mismatch so
   the caller can quote both. A rule: it encodes T3 §2's equation,
   including that the comparison is on magnitudes.
4. **Largest-remainder allocation** — distribute a `PostedMinor` total
   across weights, giving each its floor and handing the remaining units
   one each to the largest fractional remainders, ties by input order.
   A rule: it encodes T3 §3.1's decision, and the caller pre-sorts by
   `trade.num` so "input order" is the deterministic order T2 §7
   established.

The sign rule of T3 §2 stays in the adapter: it is a check on the
channel's own field against the operation family, not arithmetic.

## 3. What must not change

- No rule, threshold, rounding mode, or tie-break changes. If a test in
  `iaam-app` has to change its expected number, that is a bug in the move,
  not a consequence of it.
- The existing tests in `iaam-app` stay and keep passing unchanged. They
  are the proof the move preserved behaviour.
- `iaam-core` may not gain a dependency on any channel type. The
  operations take `Quantity`, `CalcMoney`, `PostedMinor` — nothing from
  `iaam-broker`.
- No `#[allow]` or `#[expect]` anywhere. Suppressing the guard is a
  policy change an agent may not make.

## 4. Acceptance criteria

1. `grep -rnE '\.(try_add|try_sub|checked_add|checked_sub|checked_mul|checked_negate)\(' crates/iaam-app/src crates/iaam-server/src --include='*.rs'`
   returns nothing outside comments.
2. `./scripts/check-architecture.sh` passes.
3. The four operations live in `iaam-core` and each has its own tests
   there, including the cases the adapter's tests exercise indirectly:
   an allocation whose units do not divide evenly, an allocation with
   fewer units than weights, an exact product finer than the currency's
   minor unit, and an overflow refusal for each.
4. Every existing test in `iaam-app`, `iaam-broker`, `iaam-ingest` and
   `iaam-store` passes with its expectations unchanged.

## 5. Gates

`./scripts/check-architecture.sh` plus the four crates' tests and
`iaam-core`'s. The coordinator re-runs the full `make check` afterwards.
