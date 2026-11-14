# Answers are ready, and iaam does not break (m-ready-answers)

- **Milestone label:** `m-ready-answers`
- **Date:** 2026-11-14
- **Agreed:** by the owner, 2026-11-14.
- **Vision:** `docs/vision.md`

## Outcomes

### O1. iaam survives being killed at any instant

No silently partial write, no duplicate on retry, an interrupted attempt
visible and resumable, the broker allowance recovering without manual repair.

- **Check:** a fault-injection suite in CI — the process killed at named
  points, on invented data.
- **Success signal:** no manual repair or re-run needed after a crash on the
  live instance.
- **Serves:** P3
- **Feature:** iaam-mkbfh

### O2. Current assets in one REST answer

One amount per actual account, with source, `as_of` and status.

- **Check:** the agent builds the owner's assets report from that call alone.
- **Success signal:** the owner's assets report needs no local assembly.
- **Serves:** J1, P4
- **Feature:** iaam-v46qs

### O3. A ready report of monthly and yearly investment contributions

- **Check:** on the owner's data it matches the report assembled by hand.
- **Success signal:** the owner gets the yearly contributions from one call.
- **Serves:** J2, P4
- **Feature:** iaam-s1rsj

### O4. Reports answer whole on the live data and count nothing twice

- **Check:** the yearly returns report answers in full on the live instance;
  CBR rates read back; legacy aggregate balances are not counted beside broker
  accounts.
- **Success signal:** no 500 and no double count in the owner's reports.
- **Serves:** J1, J2
- **Feature:** iaam-1rr51

## Out

Next milestone, by name:

- PDF statement readers (Ozon, MTS);
- batch statement intake;
- a browser statement collector;
- the Ozon JSON reader;
- the CSV «Ошибка» status.

## Carried over from m-broker-api

- iaam-ge08, iaam-y07x — into O1;
- iaam-2y5k, iaam-9wh2 — into O4;
- iaam-ogog5 — into O3.

The other 18 open items of m-broker-api are deferred to 2026-12-15, their label
kept.

## Finding budget

20 (the value lives in `.backlog/config.json`, `findingBudget`). The previous
milestone used 27 over a wider scope (live brokers); this one is narrower but
rewrites write paths, where surprises are likely.

## Target date

None set by the owner. The agents' own estimate is about a week of agent work;
the outcomes are independent and can run in parallel.

## Next milestone

"Bank statements arrive in iaam without hand work".

Status comes from the tracker, never from this document.
