//! Stage 1 acceptance (§16.3): for a single account with manual input, the system
//! reports how much was deposited, how much was withdrawn, and what the return is
//! before tax.
//!
//! The expected rate was obtained from an independent Python reference
//! (`scripts/gen-xirr-fixtures.py`, `decimal` arithmetic, 50 digits),
//! rather than from the output of the program under test (§15.5).

use iaam_core::contour::{ContourDefinition, ContourId, ContourVersion};
use iaam_core::dates::{CashPostedDate, EffectiveOrder, EventDates, TradeDate};
use iaam_core::event::kind::{EventKind, TradeSide};
use iaam_core::event::leg::Leg;
use iaam_core::event::provenance::{ParserVersion, Provenance, RawHash};
use iaam_core::event::{Confidence, Event, Relation, SCHEMA_VERSION};
use iaam_core::ids::{AccountId, CustodyId, EventId, InstrumentId, OwnerId, SourceId};
use iaam_core::money::{CurrencyCode, Money, PostedMinor, Quantity};
use iaam_core::numeric::approx::SolverPolicy;
use iaam_core::numeric::decimal::Dec;
use iaam_core::projection::{ProjectionContext, project};
use iaam_core::returns::{ReturnsRequest, returns_report};
use iaam_core::rules::{LotRuleVersion, RuleRegistry};
use iaam_core::valuation::{FxSource, FxTable, PriceQuality};
use rust_decimal::Decimal;
use time::Date;
use time::macros::date;

struct Fixture {
    owner: OwnerId,
    account: AccountId,
    custody: CustodyId,
    instrument: InstrumentId,
    source: SourceId,
    sequence: u32,
}

impl Fixture {
    fn new() -> Self {
        Self {
            owner: OwnerId::new_random(),
            account: AccountId::new_random(),
            custody: CustodyId::new_random(),
            instrument: InstrumentId::new_random(),
            source: SourceId::new_random(),
            sequence: 0,
        }
    }

    fn provenance(&self) -> Provenance {
        Provenance::new(
            self.source,
            RawHash::parse(&"a".repeat(64)).expect("fixture hash"),
            ParserVersion("manual/1".into()),
        )
    }

    fn event(&mut self, day: Date, kind: EventKind, dates: EventDates, legs: Vec<Leg>) -> Event {
        self.sequence += 1;
        Event {
            id: EventId::new_random(),
            schema_version: SCHEMA_VERSION,
            owner: self.owner,
            account: self.account,
            kind,
            dates,
            order: EffectiveOrder::new(day, self.sequence),
            legs,
            provenance: self.provenance(),
            relation: Relation::None,
            confidence: Confidence::Known,
            idempotency_key: None,
        }
    }
}

fn rub(minor: i64) -> Money {
    Money::new(PostedMinor::new(minor), CurrencyCode::Rub)
}

fn qty(units: i64) -> Quantity {
    Quantity(Dec::new(Decimal::from(units)))
}

/// Manual input: deposit, purchase, dividend, withdrawal, valuation.
fn journal(fixture: &mut Fixture) -> Vec<Event> {
    let deposit = rub(10_000_000);
    let gross = rub(9_000_000);
    // The trade fee is specified as a POSITIVE amount: `trade_settlement`
    // adds it to the trade principal and only then changes the sign for a purchase
    // (for a sale, it subtracts it from the proceeds). A negative fee here
    // would reduce the purchase cost — verified: `AmountMismatch`.
    let fee = rub(10_000);
    let dividend = rub(300_000);
    let withdrawal = rub(-1_000_000);

    vec![
        fixture.event(
            date!(2025 - 01 - 01),
            EventKind::CashIn { amount: deposit },
            EventDates::for_cash(CashPostedDate(date!(2025 - 01 - 01))),
            vec![Leg::cash(fixture.account, deposit)],
        ),
        {
            let settlement = rub(-(9_000_000 + 10_000));
            let account = fixture.account;
            let custody = fixture.custody;
            let instrument = fixture.instrument;
            fixture.event(
                date!(2025 - 01 - 15),
                EventKind::Trade {
                    side: TradeSide::Buy,
                    instrument,
                    quantity: qty(100),
                    gross,
                    fee: Some(fee),
                    basis_fee: None,
                    basis_fee_exact: None,
                    accrued_interest: None,
                },
                EventDates::for_trade(TradeDate(date!(2025 - 01 - 15)), None),
                vec![
                    Leg::cash(account, settlement),
                    Leg::security(account, custody, instrument, qty(100)),
                ],
            )
        },
        {
            let account = fixture.account;
            let instrument = fixture.instrument;
            fixture.event(
                date!(2025 - 07 - 01),
                EventKind::Income {
                    instrument: Some(instrument),
                    gross: dividend,
                    kind: None,
                },
                EventDates::for_cash(CashPostedDate(date!(2025 - 07 - 01))),
                vec![Leg::cash(account, dividend)],
            )
        },
        {
            let account = fixture.account;
            fixture.event(
                date!(2025 - 09 - 01),
                EventKind::CashOut { amount: withdrawal },
                EventDates::for_cash(CashPostedDate(date!(2025 - 09 - 01))),
                vec![Leg::cash(account, withdrawal)],
            )
        },
        {
            let instrument = fixture.instrument;
            fixture.event(
                date!(2026 - 01 - 01),
                EventKind::Valuation {
                    instrument,
                    price: Dec::new(Decimal::from(1_000)),
                    currency: CurrencyCode::Rub,
                    quality: PriceQuality::PreviousClose,
                },
                EventDates::for_cash(CashPostedDate(date!(2026 - 01 - 01))),
                vec![],
            )
        },
    ]
}

#[test]
fn single_account_answers_the_three_questions_of_stage_one() {
    let mut fixture = Fixture::new();
    let events = journal(&mut fixture);
    let contour = ContourDefinition::new(
        ContourId::new_random(),
        ContourVersion(1),
        [fixture.account],
    );
    let rules = RuleRegistry::with_defaults();
    let ctx = ProjectionContext {
        contour: &contour,
        rules: &rules,
        lot_rule: LotRuleVersion(1),
    };

    let projection = project(&events, &ctx).expect("projection builds successfully");
    let state = projection.state();

    // Cash: 100 000 − 90 100 + 3 000 − 10 000 = 2 900 rubles.
    assert_eq!(
        state.balances().cash(fixture.account, CurrencyCode::Rub),
        Some(rub(290_000))
    );
    // Position: 100 securities, acquisition cost 90 100 rubles.
    assert_eq!(
        state
            .balances()
            .quantity_of(fixture.account, fixture.instrument)
            .expect("quantity"),
        qty(100)
    );

    // The invariants were checked, not merely «it did not crash»: the report lists
    // exactly what was checked.
    assert!(!projection.invariants().checked().is_empty());

    let fx = FxTable::new(FxSource::OwnerSupplied);
    // Reconciliation and the perimeter are not involved in this test: it checks the calculation,
    // not data confirmation. An empty registry and valuation mean
    // «nothing has been confirmed», which is neutral for the calculation.
    let ledger = iaam_core::reconciliation::ReconciliationLedger::default();
    let perimeter = iaam_core::perimeter::PerimeterAssessment::empty(
        iaam_core::perimeter::PerimeterPolicy::default(),
    );
    let request = ReturnsRequest {
        contour: &contour,
        coordinate: iaam_core::returns::KnowledgeCoordinate::default(),
        as_of: date!(2026 - 01 - 01),
        report_currency: CurrencyCode::Rub,
        fx: &fx,
        solver_policy: SolverPolicy::returns_default(),
        ledger: &ledger,
        perimeter: &perimeter,
        market_prices: &[],
        bond_schedules: &std::collections::BTreeMap::new(),
        accrued_observations: &std::collections::BTreeMap::new(),
    };
    let report = returns_report(state, &request);

    assert_eq!(
        report.contributed.value(),
        Some(&Dec::new(Decimal::from(100_000)))
    );
    assert_eq!(
        report.withdrawn.value(),
        Some(&Dec::new(Decimal::from(10_000)))
    );
    // 2 900 in cash + 100 securities at 1 000 = 102 900.
    assert_eq!(
        report.terminal_value.value(),
        Some(&Dec::new(Decimal::from(102_900)))
    );

    let outcome = report.xirr.value().expect("rate computed");
    let expected = 0.133_270_341_032_f64;
    assert!(
        (outcome.rate().value() - expected).abs() < 1e-7,
        "rate {} versus reference {expected}",
        outcome.rate().value()
    );
    // The dividend does not cross the perimeter boundary: it is not an investment.
    assert_eq!(state.flows().external().len(), 2);
}
