# T3: the money of a trade — design

Bead: `iaam-0gbe`. Parent epic: `iaam-zn38`. Depends on `iaam-dz94` (T2,
closed). Date: 2026-08-31.

Parent design: `.internal/specs/2026-08-31-tinkoff-execution-facts-design.md`
(revision 3), §5, §6, §7. Preceding tasks:
`.internal/specs/2026-08-31-t1-order-state-design.md`,
`.internal/specs/2026-08-31-t2-trade-becomes-fact-design.md`. T2 §6 drew
the boundary this task starts from: T2 produces a per-fill gross, T3 owns
everything that reasons about the result.

Line numbers are against the `t1-order-state` worktree, which carries T1
and T2 uncommitted.

Reviewed adversarially by codex on 2026-08-31. Five findings, all
accepted, and two of them were this document being wrong rather than
incomplete: §3.1 claimed an impossibility that is only a trade-off, and
§4.2 claimed an unknown accrued interest would stay visible when
normalization substitutes zero into the cash leg. §3 also departed from
the repository's own quantity-weighted allocation without saying so, §3.2
was missing entirely, and §4.1 had no rule for the empty-currency shape
the recorded gateway actually sends.

## 1. What T2 left

After T2 a fill becomes a fact with `quantity`, `gross_minor` and its own
date. Three money fields are still empty or wrong:

- `fee_minor` is `None`, correctly — the parent design §6 established the
  commission reaches the lot basis and never the cash leg for this
  channel;
- `basis_fee` is the order's whole commission on a single-fill order and
  has no rule for several fills;
- `accrued_interest_minor` is hard-coded `None`
  (`adapters/tinkoff.rs:358,369`), so a bond purchase records no accrued
  interest at all, and a **known zero** cannot be expressed.

The contract's money fields on `OperationItem` (`operations.proto:499-507`):
`payment = 41` *«Сумма операции»*, `price = 42`, `commission = 43`,
`accrued_int = 46`, and `quantity_done = 53`.

## 2. The completeness equation, and its sign rule

One equation, checked once per **order**, not per fill:

```
|payment| == Σ(trade.quantity × trade.price) + |accrued_int|
```

with the commission excluded from both sides. The recorded response
proves the exclusion in its degenerate form: the BUY row's `payment` is
exactly `-270.130000000`, which is `1 × 270.13`, while `commission` is
reported separately (`tests/fixtures/api/tinkoff-operations.json`).

**The sign rule, stated because comparing a positive product with a
negative payment needs one.** The comparison is between magnitudes, and
direction is asserted separately, as a second check:

| Family | Required sign of `payment` |
|---|---|
| Buy | strictly negative |
| Sell | strictly positive |

A zero `payment` on a row with fills is refused: it is neither, and no
reading of it produces a fact. A sign that contradicts the family
quarantines the row with both the family and the value — that is evidence
we have misread the channel, not evidence about the trade.

This is the direction convention the codebase already holds:
`OperationKind` stores a positive magnitude and the variant encodes
direction (`ChannelMoney::magnitude`), and `validate_trade` requires a
positive `gross` and a positive `quantity`
(`iaam-core/src/event/mod.rs:394-395`).

**The accrued-interest term is a reading, and this is the evidence for
it.** Every operation in the recorded response is a share with
`accruedInt` zero, so what is verified is the degenerate case
`|payment| = Σ(q × p)`. The support for including the term is the
broker's own model of a trade in the same contract file: a broker-report
trade carries `order_amount` *«Сумма без НКД»*, `aci_value` *«НКД»* and
`total_order_amount` *«Сумма сделки»* (`operations.proto:276-278`). A
"сумма сделки" that is distinguished from a "сумма без НКД" is a total
that includes accrued interest, and `payment` is *«Сумма операции»*.

That is a strong hint and not a proof: those fields belong to
`BrokerReportTrade`, not to `OperationItem`. Including the term is the
reading under which a bond purchase reconciles at all, and if it is
wrong the check quarantines the row with both amounts rather than
recording a distorted fact — the correct failure direction. Confirming it
against a live bond purchase stays filed as the parent design's §13
follow-up.

A mismatch quarantines the row with both amounts.

## 3. Commission allocation across fills

The commission is one amount for the whole order and must reach the
basis of each fill's lot.

**The weight is quantity.** Not because a per-unit fee is proven — it is
not — but because this repository already allocates an actual payment to
lots in proportion to quantity (`iaam-core/src/projection/lots.rs:381`
and its doc comment), and with no evidence either way, matching the
convention already in the code beats introducing a second one.

The alternative reading is real and unresolved: a brokerage commission is
conventionally a percentage of turnover, which would weight by
`quantity × price`. The two agree whenever every fill of an order shares
one price, and every fill in the recorded response does, so nothing here
distinguishes them. Filed as `iaam-jsvd`; until it is answered, quantity
is the weight and this paragraph is why.

### 3.1 Allocation is done in minor units, and what that trades away

Allocate the order's **posted** commission — `commission` converted once
with `CalcMoney::rounded_minor` — across fills in minor units, by
quantity, using the largest-remainder method: give each fill the floor of
its share, then hand the remaining units one each to the fills with the
largest fractional remainders, ties broken by `trade.num` as bytes.

Each fill's `basis_fee` is its allocated whole minor units, and its
`basis_fee_exact` is that same amount as an exact decimal.

This holds both invariants at once:

- **per event:** `basis_fee_exact` rounds to `basis_fee` trivially, since
  they are the same number, so every event satisfies the core's rule
  (`iaam-core/src/event/mod.rs:413-424`);
- **per order:** the posted parts sum exactly to the posted commission,
  by construction of the largest-remainder method.

An earlier revision of this document claimed those two invariants were
mutually impossible. That was wrong, and the correction is the point of
this section: what is impossible is holding both **while also** keeping
each fill's exact share at its mathematically exact proportional value.
Something has to give, and the proportional sub-minor share is the right
thing to give up, because it is not a source fact: the broker reported
one commission for the order, not a fraction of a kopeck per fill.

What is given up, stated: the sub-minor part of the order's commission —
strictly less than one minor unit — does not reach any lot's basis,
because the allocation starts from the rounded aggregate. The exact
commission stays on the row's raw JSON, which is retained.

### 3.2 A share of zero is not a fee

`validate_trade` requires every present `basis_fee` to be strictly
positive (`iaam-core/src/event/mod.rs:396`) and requires `basis_fee` and
`basis_fee_exact` to be present or absent together (`:413-417`). An
order whose posted commission has fewer minor units than it has fills
therefore cannot give every fill a positive share.

A fill allocated zero units records `basis_fee: None` and
`basis_fee_exact: None`. It is not an error and does not quarantine the
row: a fee below the smallest recordable unit is not a fee, and the
largest-remainder method already concentrates the available units on the
largest fills, which is where the basis effect belongs.

The order's whole posted commission still reaches the basis in total —
the zero shares are zero, not discarded remainders.

## 4. Accrued interest

### 4.1 A known zero, and the currency that decides whether it is known

`accrued_interest_minor` reaches the event through `fee_money`, which
routes the value through `positive()` and refuses zero
(`iaam-ingest/src/operation.rs:270-280`, `:506-511`). So a bond whose
accrued interest is genuinely zero cannot be distinguished from a bond
whose accrued interest we never learned — the §4.9 collapse of absent and
zero that revision 1 of the parent design committed.

Fix: accrued interest gets its own conversion that accepts zero and still
refuses a negative value, separate from `fee_money`. The two are not the
same rule and sharing one function is what merged them. `fee_money` keeps
refusing zero — a zero fee is a fee that was not charged, which is
`None`.

**But the gateway's own zero is not a known zero.** The recorded response
carries `"accruedInt": {"currency": "", "units": "0", "nano": 0}`
(`tests/fixtures/api/tinkoff-operations.json:175-179`), and
`parse_optional_money` already maps exactly that shape to `None`
(`iaam-broker/src/tinkoff/parse.rs:460-465`). An empty currency is not a
currency, and the same reading was taken in T2 §3 for a placeholder trade
element. Applying two different meanings to one shape in one response
would be indefensible.

So the rule is:

| `accruedInt` in the response | Recorded |
|---|---|
| absent | `None` |
| empty currency, zero amount | `None` — not reported |
| a real currency, zero amount | `Some(0)` — a known zero |
| a real currency, non-zero | `Some(n)` |

A currency-bearing value must match the trade's currency; a mismatch
quarantines the row, as a currency mismatch does everywhere else.

Consequence, stated because it makes a criterion weaker than it looks:
**every row in the recorded response yields `None`, not `Some(0)`.**
Whether this gateway ever emits a currency-bearing zero is unknown, so
the known-zero path is a capability the recorded evidence does not
exercise. A test may only assert it against synthetic JSON, and a second
test must assert the recorded shape gives `None` — otherwise the pair of
them proves nothing about the gateway we actually talk to.

### 4.2 A multi-day order is quarantined

`accrued_int` is one aggregate for the order, accrued to one moment.
Accrued interest grows with time, so when an order's fills fall on more
than one date, no split of that aggregate is a fact about any individual
fill.

An earlier revision of this document left it `None` on each fill and
claimed the gap would be "visible as unknown". **That was wrong.**
Normalization substitutes zero for an absent accrued interest when it
builds the cash leg — `settlement += accrued.map_or(0, …)`
(`iaam-ingest/src/operation.rs:355-357` for a purchase, `:392-394` for a
sale) — and the resulting leg is consumed downstream as known money. Only
the zero-reinvestment cohort metric refuses on missing accrued interest
(`iaam-core/src/returns/zero_reinvestment.rs:372`). So "unknown" would
have been recorded as a cash movement that did not happen.

Policy:

- **no accrued interest to split** — `accrued_int` absent, or a known
  zero: the fills' dates are irrelevant. Every fill records `None` or
  `Some(0)` as §4.1 decided, and a multi-day order produces its facts
  normally. A share order that filled across two days is an ordinary
  event and must not be refused.
- **a non-zero accrued interest, all fills on one date:** allocate it
  across the fills in minor units by quantity, by the same
  largest-remainder rule as §3.1, so the posted parts sum to the posted
  aggregate;
- **a non-zero accrued interest, fills on more than one date:** the row
  is **quarantined**, with the dates named. A fact cannot be built
  without asserting a cash movement the source did not report, and this
  journal is append-only.

The condition is the accrued interest, never the dates alone. Gating on
the dates alone would refuse the parent design's own §14 case 5 — "a
two-day order produces two facts with two different trade dates" — which
is a case this epic is required to support.

**The dates compared are the UTC dates**, `at.to_offset(UtcOffset::UTC).date()`,
the same derivation the fact itself uses (T2 §9). Comparing the dates as
parsed while recording them in UTC would let an order be judged
single-day on one calendar and recorded across two on another.

The order-level completeness check of §2 is unaffected either way: it
compares the order's own `payment` against the order's own `accrued_int`,
before any split.

**Note on the wider defect.** The zero-substitution above is not specific
to multi-day orders: any trade with `accrued_interest_minor: None`
records a cash leg as if accrued interest were zero, which is correct for
a share and wrong for a bond whose accrued interest was never reported.
That is a pre-existing property of `normalize`, not something T3
introduces, and it is filed as `iaam-9wh2` rather than changed here.

## 5. Not changed by T3

- `fee_minor` stays `None` for this channel. The commission reaches the
  basis and never the cash leg (parent design §6), and the settlement
  rule that would justify posting it is unknown — summing every `payment`
  in the recorded response gives `199729.734935` against the broker's
  reported `199729.73`, which proves a settlement rule exists and does
  not reveal it.
- `TINKOFF_PARSER_VERSION` stays `tinkoff-api/3` (T2 §11), on the same
  condition: the epic lands as one branch with no release between tasks.
- Custody, pagination, securities transfers — T4, T5, T6.
- The core's per-event basis-fee invariant (§3.1).

## 6. Acceptance criteria

1. An order whose `|payment|` differs from `Σ(quantity × price) +
   |accrued_int|` quarantines with both amounts; an order that matches
   produces its facts.
2. A `Buy` with a non-negative `payment`, or a `Sell` with a
   non-positive one, quarantines with the family and the value.
3. The posted commission shares of one order sum exactly to the order's
   posted commission, and each fill's `basis_fee_exact` equals its
   `basis_fee`, so every event satisfies the core's basis-fee invariant.
4. A fill allocated zero minor units of commission records `None` for
   both basis-fee fields and does not quarantine the row.
5. An `accruedInt` with a real currency and a zero amount records
   `Some(0)`; an `accruedInt` with an empty currency, and an absent one,
   both record `None`. A currency that differs from the trade's
   quarantines the row.
6. The recorded response's rows record `None` for accrued interest — the
   criterion that pins §4.1's reading against the gateway shape we
   actually have, rather than against synthetic JSON.
7. An order with a non-zero accrued interest whose fills fall on one UTC
   date splits it across them in minor units by quantity, summing to the
   posted aggregate; the same order with its fills on two UTC dates is
   **quarantined**, with the dates named.
7b. An order with no accrued interest, or a known zero, produces its
   facts regardless of how many dates its fills fall on — including the
   parent design's §14 case 5, a two-day order producing two facts with
   two different trade dates.
8. `fee_minor` is `None` on every fact this channel produces.

## 7. Tests

Inline JSON in the crates' own test modules, as in T1 and T2:
`tests/fixtures/api/` is a policy directory
(`scripts/check-diff-lint.sh:80-83`) and an agent may not approve a
change to it. The one exception is reading the existing recorded
response, which several cases below require.

- the recorded SBER purchase still reconciles — the degenerate
  `|payment| = 1 × 270.13` case — and still produces its fact;
- a two-fill order whose `Σ(q × p)` differs from `|payment|` quarantines
  with both amounts;
- a bond buy with a non-zero `accrued_int` reconciles only when the term
  is included, and quarantines when it is not — the test that pins §2's
  reading, so that changing the reading breaks a test rather than a
  balance;
- a `Buy` with a positive `payment` quarantines naming the family;
- a `Sell` with a negative `payment` quarantines naming the family;
- a three-fill order with a commission that does not divide evenly: the
  posted shares sum exactly to the order's posted commission, and the
  remainder lands on the fills with the largest fractional shares, ties
  by `num` — fed in both response orders, so the split does not depend on
  arrival order;
- an order whose posted commission has fewer minor units than it has
  fills: the short fills record `None` for both basis-fee fields, the
  row is not quarantined, and the shares still sum to the posted
  commission;
- `accruedInt` with a real currency and zero gives `Some(0)`; with an
  empty currency gives `None`; absent gives `None`; with a currency
  differing from the trade's, quarantines;
- **the recorded response's own rows give `None`** — the companion to the
  synthetic known-zero case above, without which neither proves anything
  about this gateway;
- a two-fill order with a non-zero accrued interest on one date splits it
  by quantity and the parts sum to the aggregate; the same order with its
  fills on two dates is quarantined with both dates named;
- a two-fill order **without** accrued interest whose fills fall on two
  dates produces two facts with two different trade dates, and no
  quarantine — the parent design's §14 case 5, which the dates-alone rule
  would have refused;
- an order whose fills carry a non-UTC offset is judged single-day or
  multi-day on the same UTC dates the facts are recorded with.

## 8. Gates

`cargo check` and the two crates' own tests, as in T1 and T2. The
workspace gates run once at the end of the epic.
