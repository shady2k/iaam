//! Golden-приёмка E2: отчётные строки, сверка, периметр и качество данных.
//!
//! Ожидаемые суммы ниже посчитаны вручную из строк сценария. Этот тест
//! намеренно остаётся в `iaam-core`: ingest-слой не является зависимостью
//! core, поэтому граница разбора реального XLSX проверяется в iaam-app.

use iaam_core::contour::{ContourDefinition, ContourId, ContourVersion};
use iaam_core::event::Event;
use iaam_core::event::kind::{EventKind, FeeOrigin};
use iaam_core::event::leg::Leg;
use iaam_core::ids::{AccountId, OwnerId};
use iaam_core::money::{CurrencyCode, Money, PostedMinor};
use iaam_core::numeric::approx::SolverPolicy;
use iaam_core::perimeter::{
    NegativeCashClassification, PerimeterExceptions, PerimeterPolicy, assess,
};
use iaam_core::projection::{ProjectionContext, project};
use iaam_core::reconciliation::check::ClaimOutcome;
use iaam_core::reconciliation::claim::{AssertionPeriod, BalancePoint, ControlClaim};
use iaam_core::reconciliation::{Dimension, DimensionStatus, ReconciliationLedger};
use iaam_core::returns::{DataQualityStatus, ReturnsRequest, returns_report};
use iaam_core::rules::{LotRuleVersion, RuleRegistry};
use iaam_core::valuation::{FxSource, FxTable};
use time::{Date, macros::date};

mod support;
use support::{Posting, TestChannel, event_on};

const REPORT_DAY: Date = date!(2026 - 03 - 31);
const MARCH_CLOSING: i64 = 203_850;
const MARCH_DEBIT: i64 = 504_000;
const MARCH_CREDIT: i64 = 300_150;

fn rub(minor: i64) -> Money {
    Money::new(PostedMinor::new(minor), CurrencyCode::Rub)
}

fn march() -> AssertionPeriod {
    match AssertionPeriod::between(date!(2026 - 03 - 01), REPORT_DAY) {
        Some(period) => period,
        None => panic!("the golden period must be well formed"),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RowVerdict {
    Accepted,
    Rejected { field: &'static str },
    Unsupported { code: &'static str },
}

#[derive(Debug)]
struct ImportedRows {
    events: Vec<Event>,
    verdicts: Vec<RowVerdict>,
}

/// Представляет итог разбора строк до передачи фактов в ядро.
///
/// В production этот boundary принадлежит `iaam-ingest`; здесь он записан
/// явно, чтобы golden-сценарий не мог потерять rejected/unsupported строки.
fn imported_report(owner: OwnerId, account: AccountId, channel: &TestChannel) -> ImportedRows {
    let mut events = Vec::new();
    let mut verdicts = Vec::new();
    let mut add = |day, sequence, kind, legs| {
        events.push(event_on(
            channel,
            Posting {
                owner,
                account,
                day,
                sequence,
            },
            kind,
            legs,
        ));
        verdicts.push(RowVerdict::Accepted);
    };

    add(
        date!(2026 - 03 - 02),
        1,
        EventKind::CashIn {
            amount: rub(500_000),
        },
        vec![Leg::cash(account, rub(500_000))],
    );
    add(
        date!(2026 - 03 - 05),
        1,
        EventKind::CashOut {
            amount: rub(-300_000),
        },
        vec![Leg::cash(account, rub(-300_000))],
    );
    add(
        date!(2026 - 03 - 05),
        2,
        EventKind::Fee {
            amount: rub(-150),
            origin: FeeOrigin::Brokerage,
        },
        vec![Leg::fee(account, rub(-150))],
    );
    add(
        date!(2026 - 03 - 12),
        1,
        EventKind::Income {
            instrument: None,
            gross: rub(4_000),
            kind: None,
        },
        vec![Leg::cash(account, rub(4_000))],
    );
    verdicts.push(RowVerdict::Rejected { field: "payment" });
    verdicts.push(RowVerdict::Unsupported {
        code: "unsupported_repo_encumbrance",
    });

    verdicts.extend([
        RowVerdict::Accepted,
        RowVerdict::Accepted,
        RowVerdict::Accepted,
    ]);
    ImportedRows { events, verdicts }
}

fn append_claim(
    events: &mut Vec<Event>,
    owner: OwnerId,
    account: AccountId,
    channel: &TestChannel,
    claim: ControlClaim,
    sequence: u32,
) {
    events.push(event_on(
        channel,
        Posting {
            owner,
            account,
            day: REPORT_DAY,
            sequence,
        },
        EventKind::ControlAssertion {
            period: march(),
            claim,
        },
        Vec::new(),
    ));
}

fn append_full_claims(
    events: &mut Vec<Event>,
    owner: OwnerId,
    account: AccountId,
    channel: &TestChannel,
    closing: i64,
) {
    append_claim(
        events,
        owner,
        account,
        channel,
        ControlClaim::CashBalance {
            currency: CurrencyCode::Rub,
            amount: PostedMinor::new(0),
            at: BalancePoint::Opening,
        },
        10,
    );
    append_claim(
        events,
        owner,
        account,
        channel,
        ControlClaim::CashBalance {
            currency: CurrencyCode::Rub,
            amount: PostedMinor::new(closing),
            at: BalancePoint::Closing,
        },
        11,
    );
    append_claim(
        events,
        owner,
        account,
        channel,
        ControlClaim::CashTurnover {
            currency: CurrencyCode::Rub,
            debit: PostedMinor::new(MARCH_DEBIT),
            credit: PostedMinor::new(MARCH_CREDIT),
        },
        12,
    );
}

fn report(
    events: &[Event],
    contour: &ContourDefinition,
    as_of: Date,
    perimeter: &iaam_core::perimeter::PerimeterAssessment,
) -> iaam_core::returns::ReturnsReport {
    let rules = RuleRegistry::with_defaults();
    let context = ProjectionContext {
        contour,
        rules: &rules,
        lot_rule: LotRuleVersion(1),
    };
    let projection = match project(events, &context) {
        Ok(projection) => projection,
        Err(error) => panic!("golden projection must build: {error}"),
    };
    let ledger = match ReconciliationLedger::build(events) {
        Ok(ledger) => ledger,
        Err(error) => panic!("golden reconciliation must build: {error}"),
    };
    let fx = FxTable::new(FxSource::OwnerSupplied);
    returns_report(
        projection.state(),
        &ReturnsRequest {
            contour,
            coordinate: iaam_core::returns::KnowledgeCoordinate::default(),
            as_of,
            report_currency: CurrencyCode::Rub,
            fx: &fx,
            solver_policy: SolverPolicy::returns_default(),
            ledger: &ledger,
            perimeter,
            market_prices: &[],
            bond_schedules: &std::collections::BTreeMap::new(),
            accrued_observations: &std::collections::BTreeMap::new(),
        },
    )
}

#[test]
fn golden_report_rows_keep_rejections_and_manual_totals() {
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();
    let channel = TestChannel::new("tinkoff-api/1", "march");
    let imported = imported_report(owner, account, &channel);

    assert_eq!(imported.events.len(), 4);
    assert_eq!(
        imported.verdicts,
        vec![
            RowVerdict::Accepted,
            RowVerdict::Accepted,
            RowVerdict::Accepted,
            RowVerdict::Accepted,
            RowVerdict::Rejected { field: "payment" },
            RowVerdict::Unsupported {
                code: "unsupported_repo_encumbrance",
            },
            RowVerdict::Accepted,
            RowVerdict::Accepted,
            RowVerdict::Accepted,
        ]
    );

    let mut events = imported.events;
    append_full_claims(&mut events, owner, account, &channel, MARCH_CLOSING);
    let contour = ContourDefinition::new(ContourId::new_random(), ContourVersion(1), [account]);
    let perimeter = match assess(&events, PerimeterPolicy::default()) {
        Ok(perimeter) => perimeter,
        Err(error) => panic!("golden perimeter must build: {error}"),
    };
    let rules = RuleRegistry::with_defaults();
    let projection = match project(
        &events,
        &ProjectionContext {
            contour: &contour,
            rules: &rules,
            lot_rule: LotRuleVersion(1),
        },
    ) {
        Ok(projection) => projection,
        Err(error) => panic!("golden projection must build: {error}"),
    };
    assert_eq!(
        projection
            .state()
            .balances()
            .cash(account, CurrencyCode::Rub),
        Some(rub(MARCH_CLOSING))
    );

    let ledger = match ReconciliationLedger::build(&events) {
        Ok(ledger) => ledger,
        Err(error) => panic!("golden reconciliation must build: {error}"),
    };
    let status = match ledger.statuses().find(|status| status.account() == account) {
        Some(status) => status,
        None => panic!("the golden account must have a reconciliation status"),
    };
    assert_eq!(
        status.dimension(Dimension::Cash),
        DimensionStatus::AcceptedInternal
    );
    assert!(
        status
            .outcomes()
            .iter()
            .all(|check| matches!(check.outcome, ClaimOutcome::Matched))
    );

    let result = report(&events, &contour, REPORT_DAY, &perimeter);
    assert_eq!(result.data_quality.status, DataQualityStatus::Clean);
    assert_eq!(
        result.data_quality.nav_coverage.accepted_internal,
        iaam_core::numeric::decimal::Dec::one()
    );
    assert!(
        result
            .data_quality
            .nav_coverage
            .accepted_independent
            .is_zero()
    );
    assert!(result.data_quality.nav_coverage.discrepant.is_zero());
}

#[test]
fn golden_channels_and_perimeter_are_account_scoped() {
    let owner = OwnerId::new_random();
    let healthy = AccountId::new_random();
    let margin = AccountId::new_random();
    let unclassified = AccountId::new_random();
    let repo = AccountId::new_random();
    let first = TestChannel::new("tinkoff-api/1", "march");
    let second = TestChannel::new("custody-export/1", "custody-march");
    let corrupted = TestChannel::new("tinkoff-api/2", "march-corrupt");

    let mut events = imported_report(owner, healthy, &first).events;
    append_full_claims(&mut events, owner, healthy, &first, MARCH_CLOSING);
    let one_channel = match ReconciliationLedger::build(&events) {
        Ok(ledger) => ledger,
        Err(error) => panic!("one-channel reconciliation must build: {error}"),
    };
    let one_channel_status = match one_channel
        .statuses()
        .find(|status| status.account() == healthy)
    {
        Some(status) => status,
        None => panic!("the healthy account must have a one-channel status"),
    };
    assert_eq!(
        one_channel_status.dimension(Dimension::Cash),
        DimensionStatus::AcceptedInternal
    );

    append_full_claims(&mut events, owner, healthy, &second, MARCH_CLOSING);
    let independent = match ReconciliationLedger::build(&events) {
        Ok(ledger) => ledger,
        Err(error) => panic!("two-channel reconciliation must build: {error}"),
    };
    let independent_status = match independent
        .statuses()
        .find(|status| status.account() == healthy)
    {
        Some(status) => status,
        None => panic!("the healthy account must have an independent status"),
    };
    assert_eq!(
        independent_status.dimension(Dimension::Cash),
        DimensionStatus::AcceptedIndependent
    );
    let two_channel_events = events.clone();

    events.push(event_on(
        &first,
        Posting {
            owner,
            account: unclassified,
            day: date!(2026 - 03 - 20),
            sequence: 1,
        },
        EventKind::CashOut {
            amount: rub(-1_000),
        },
        vec![Leg::cash(unclassified, rub(-1_000))],
    ));
    append_full_claims(&mut events, owner, healthy, &corrupted, MARCH_CLOSING + 1);
    let discrepant = match ReconciliationLedger::build(&events) {
        Ok(ledger) => ledger,
        Err(error) => panic!("discrepant reconciliation must build: {error}"),
    };
    let discrepant_status = match discrepant
        .statuses()
        .find(|status| status.account() == healthy)
    {
        Some(status) => status,
        None => panic!("the healthy account must have a discrepant status"),
    };
    assert_eq!(
        discrepant_status.dimension(Dimension::Cash),
        DimensionStatus::Discrepant
    );
    let two_channel_contour =
        ContourDefinition::new(ContourId::new_random(), ContourVersion(1), [healthy]);
    let two_channel_perimeter = match assess(&two_channel_events, PerimeterPolicy::default()) {
        Ok(perimeter) => perimeter,
        Err(error) => panic!("two-channel perimeter must build: {error}"),
    };
    let two_channel_result = report(
        &two_channel_events,
        &two_channel_contour,
        REPORT_DAY,
        &two_channel_perimeter,
    );
    assert_eq!(
        two_channel_result.data_quality.status,
        DataQualityStatus::Clean
    );
    assert_eq!(
        two_channel_result
            .data_quality
            .nav_coverage
            .accepted_independent,
        iaam_core::numeric::decimal::Dec::one()
    );
    assert!(
        two_channel_result
            .data_quality
            .nav_coverage
            .accepted_internal
            .is_zero()
    );
    assert!(
        two_channel_result
            .data_quality
            .nav_coverage
            .discrepant
            .is_zero()
    );

    events.push(event_on(
        &first,
        Posting {
            owner,
            account: margin,
            day: date!(2026 - 03 - 20),
            sequence: 1,
        },
        EventKind::CashOut {
            amount: rub(-10_000),
        },
        vec![Leg::cash(margin, rub(-10_000))],
    ));
    events.push(event_on(
        &first,
        Posting {
            owner,
            account: margin,
            day: date!(2026 - 03 - 21),
            sequence: 1,
        },
        EventKind::Fee {
            amount: rub(-50),
            origin: FeeOrigin::MarginInterest,
        },
        vec![Leg::fee(margin, rub(-50))],
    ));
    events.push(event_on(
        &first,
        Posting {
            owner,
            account: repo,
            day: date!(2026 - 03 - 10),
            sequence: 1,
        },
        EventKind::CashIn {
            amount: rub(20_000),
        },
        vec![Leg::cash(repo, rub(20_000))],
    ));

    let perimeter = match assess(&events, PerimeterPolicy::default()) {
        Ok(perimeter) => perimeter,
        Err(error) => panic!("golden perimeter must build: {error}"),
    };
    let margin_span = perimeter.spans().iter().find(|span| span.account == margin);
    assert_eq!(
        margin_span.map(|span| span.classification),
        Some(NegativeCashClassification::UnsupportedMarginLiability)
    );
    let unclassified_span = perimeter
        .spans()
        .iter()
        .find(|span| span.account == unclassified);
    assert_eq!(
        unclassified_span.map(|span| span.classification),
        Some(NegativeCashClassification::UnclassifiedNegativeCash)
    );
    assert!(perimeter.blocks_period_reports(unclassified));
    assert!(perimeter.blocks_period_reports(margin));
    assert!(!perimeter.blocks_period_reports(healthy));
    assert!(!perimeter.blocks_period_reports(repo));

    let mut exceptions = PerimeterExceptions::default();
    exceptions.add(
        repo,
        Dimension::Positions,
        iaam_core::reconciliation::check::ReconciliationException::UnsupportedRepoEncumbrance,
    );
    let repo_claim = ControlClaim::PositionQuantity {
        instrument: iaam_core::ids::InstrumentId::new_random(),
        custody: iaam_core::ids::CustodyId::new_random(),
        quantity: iaam_core::money::Quantity(iaam_core::numeric::decimal::Dec::one()),
        at: BalancePoint::Closing,
    };
    let repo_claim_channel = TestChannel::new("tinkoff-api/1", "repo");
    append_claim(&mut events, owner, repo, &repo_claim_channel, repo_claim, 2);
    let repo_ledger = match ReconciliationLedger::build_with(&events, &exceptions) {
        Ok(ledger) => ledger,
        Err(error) => panic!("golden reconciliation with perimeter exception must build: {error}"),
    };
    let repo_status = match repo_ledger
        .statuses()
        .find(|status| status.account() == repo)
    {
        Some(status) => status,
        None => panic!("the repo account must have a reconciliation status"),
    };
    assert!(repo_status.outcomes().iter().any(|check| {
        matches!(
            check.outcome,
            ClaimOutcome::Excepted {
                exception: iaam_core::reconciliation::check::ReconciliationException::UnsupportedRepoEncumbrance
            }
        )
    }));
    assert_eq!(
        repo_status.dimension(Dimension::Positions),
        DimensionStatus::Provisional
    );

    let healthy_contour =
        ContourDefinition::new(ContourId::new_random(), ContourVersion(1), [healthy]);
    let healthy_perimeter = match assess(&events, PerimeterPolicy::default()) {
        Ok(perimeter) => perimeter,
        Err(error) => panic!("healthy perimeter must build: {error}"),
    };
    let healthy_result = report(&events, &healthy_contour, REPORT_DAY, &healthy_perimeter);
    assert_eq!(
        healthy_result.data_quality.status,
        DataQualityStatus::Incomplete
    );
    assert!(healthy_result
        .data_quality
        .material_issues
        .iter()
        .any(|issue| matches!(issue, iaam_core::returns::MaterialIssue::Discrepancy { account, .. } if *account == healthy)));
}
#[test]
fn golden_compensating_parser_error_stays_internal() {
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();
    let channel = TestChannel::new("tinkoff-xlsx/1", "compensated-march");
    let shifted = 7;
    let amount = 100_000 + shifted;
    let mut events = vec![event_on(
        &channel,
        Posting {
            owner,
            account,
            day: date!(2026 - 03 - 10),
            sequence: 1,
        },
        EventKind::CashIn {
            amount: rub(amount),
        },
        vec![Leg::cash(account, rub(amount))],
    )];
    append_claim(
        &mut events,
        owner,
        account,
        &channel,
        ControlClaim::CashBalance {
            currency: CurrencyCode::Rub,
            amount: PostedMinor::new(0),
            at: BalancePoint::Opening,
        },
        10,
    );
    append_claim(
        &mut events,
        owner,
        account,
        &channel,
        ControlClaim::CashBalance {
            currency: CurrencyCode::Rub,
            amount: PostedMinor::new(amount),
            at: BalancePoint::Closing,
        },
        11,
    );
    append_claim(
        &mut events,
        owner,
        account,
        &channel,
        ControlClaim::CashTurnover {
            currency: CurrencyCode::Rub,
            debit: PostedMinor::new(amount),
            credit: PostedMinor::new(0),
        },
        12,
    );
    let ledger = match ReconciliationLedger::build(&events) {
        Ok(ledger) => ledger,
        Err(error) => panic!("compensated report must reconcile: {error}"),
    };
    assert_eq!(
        ledger.status_for(account, date!(2026 - 03 - 15), Dimension::Cash),
        DimensionStatus::AcceptedInternal
    );
    assert_ne!(
        ledger.status_for(account, date!(2026 - 03 - 15), Dimension::Cash),
        DimensionStatus::AcceptedIndependent
    );
}

#[test]
fn golden_full_recompute_after_import_uses_corrected_history() {
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();
    let channel = TestChannel::new("tinkoff-api/1", "corrected-march");
    let contour = ContourDefinition::new(ContourId::new_random(), ContourVersion(1), [account]);
    let rules = RuleRegistry::with_defaults();
    let project_cash = |events: &[Event]| {
        let projection = match project(
            events,
            &ProjectionContext {
                contour: &contour,
                rules: &rules,
                lot_rule: LotRuleVersion(1),
            },
        ) {
            Ok(projection) => projection,
            Err(error) => panic!("full recompute must build: {error}"),
        };
        match projection
            .state()
            .balances()
            .cash(account, CurrencyCode::Rub)
        {
            Some(balance) => balance,
            None => panic!("recomputed account must have a cash balance"),
        }
    };
    let make_events = |cash_out: i64| {
        vec![
            event_on(
                &channel,
                Posting {
                    owner,
                    account,
                    day: date!(2026 - 03 - 02),
                    sequence: 1,
                },
                EventKind::CashIn {
                    amount: rub(100_000),
                },
                vec![Leg::cash(account, rub(100_000))],
            ),
            event_on(
                &channel,
                Posting {
                    owner,
                    account,
                    day: date!(2026 - 03 - 05),
                    sequence: 1,
                },
                EventKind::CashOut {
                    amount: rub(-cash_out),
                },
                vec![Leg::cash(account, rub(-cash_out))],
            ),
        ]
    };

    assert_eq!(project_cash(&make_events(40_000)), rub(60_000));
    assert_eq!(project_cash(&make_events(30_000)), rub(70_000));
}

#[test]
fn golden_policy_change_recomputes_imported_history() {
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();
    let channel = TestChannel::new("tinkoff-xlsx/1", "recomputed-march");
    let events = vec![
        event_on(
            &channel,
            Posting {
                owner,
                account,
                day: date!(2026 - 03 - 10),
                sequence: 1,
            },
            EventKind::CashOut {
                amount: rub(-10_000),
            },
            vec![Leg::cash(account, rub(-10_000))],
        ),
        event_on(
            &channel,
            Posting {
                owner,
                account,
                day: date!(2026 - 03 - 13),
                sequence: 1,
            },
            EventKind::CashIn {
                amount: rub(10_000),
            },
            vec![Leg::cash(account, rub(10_000))],
        ),
    ];
    let contour = ContourDefinition::new(ContourId::new_random(), ContourVersion(1), [account]);
    let rules = RuleRegistry::with_defaults();
    let project_cash = |imported: &[Event]| {
        let projection = match project(
            imported,
            &ProjectionContext {
                contour: &contour,
                rules: &rules,
                lot_rule: LotRuleVersion(1),
            },
        ) {
            Ok(projection) => projection,
            Err(error) => panic!("full recompute must build: {error}"),
        };
        match projection
            .state()
            .balances()
            .cash(account, CurrencyCode::Rub)
        {
            Some(balance) => balance,
            None => panic!("recomputed account must have a cash balance"),
        }
    };

    let permissive = match assess(
        &events,
        PerimeterPolicy {
            settlement_window_days: 5,
        },
    ) {
        Ok(perimeter) => perimeter,
        Err(error) => panic!("permissive policy must assess imported history: {error}"),
    };
    let strict = match assess(
        &events,
        PerimeterPolicy {
            settlement_window_days: 2,
        },
    ) {
        Ok(perimeter) => perimeter,
        Err(error) => panic!("strict policy must assess imported history: {error}"),
    };
    assert_eq!(
        permissive.spans()[0].classification,
        NegativeCashClassification::TemporarySettlementDeficit
    );
    assert_eq!(
        strict.spans()[0].classification,
        NegativeCashClassification::UnclassifiedNegativeCash
    );
    assert!(!permissive.blocks_period_reports(account));
    assert!(strict.blocks_period_reports(account));
    assert_eq!(project_cash(&events), rub(0));
}
