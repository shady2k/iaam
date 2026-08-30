//! Независимые эталоны метрик без реинвестирования (§15.4).
//!
//! График переводится из замороженного ответа MOEX в минимальные доменные
//! типы прямо в тесте. Ожидаемые суммы ниже вычислены вручную из строк
//! fixtures, а не вызовом проверяемых функций.

use iaam_core::bond::offer::{OfferChoice, OfferRight, ScheduleCompleteness};
use iaam_core::bond::{AccrualPeriod, BondSchedule, DefaultFlags, PrincipalReturn};
use iaam_core::ids::InstrumentId;
use iaam_core::instrument::CurrencyRoles;
use iaam_core::money::{CalcMoney, CurrencyCode, PerUnitAmount, Quantity};
use iaam_core::numeric::decimal::Dec;
use iaam_core::numeric::xirr::RateOutcome;
use iaam_core::returns::zero_reinvestment::{IrrLabel, prospective_metric};
use iaam_core::returns::{Computed, NotComputable};
use iaam_core::rules::{
    CashflowError, CashflowInput, CashflowProjection, CashflowProjectionV1, ExpectedPosting,
    PostingKind,
};
use rust_decimal::Decimal;
use serde_json::Value;
use time::macros::{date, format_description};
use time::{Date, Duration};

const FIXED_COUPON: &str =
    include_str!("../../../tests/fixtures/market/moex-iss-bondization-fixed-coupon.json");
const AMORTISED: &str =
    include_str!("../../../tests/fixtures/market/moex-iss-bondization-amortised.json");
const FLOATER: &str =
    include_str!("../../../tests/fixtures/market/moex-iss-bondization-floater.json");
const OFFERS: &str =
    include_str!("../../../tests/fixtures/market/moex-iss-bondization-offers.json");

fn dec(text: &str) -> Dec {
    Dec::new(text.parse::<Decimal>().expect("десятичный эталон"))
}

fn calc(text: &str) -> CalcMoney {
    CalcMoney::new(dec(text), CurrencyCode::Rub)
}

fn date_of(text: &str) -> Date {
    Date::parse(text, format_description!("[year]-[month]-[day]")).expect("дата fixture")
}

fn block<'a>(root: &'a Value, name: &str) -> (Vec<&'a str>, &'a [Value]) {
    let block = root.get(name).expect("секция fixture");
    let columns = block
        .get("columns")
        .and_then(Value::as_array)
        .expect("columns fixture")
        .iter()
        .map(|value| value.as_str().expect("имя колонки fixture"))
        .collect();
    let rows = block
        .get("data")
        .and_then(Value::as_array)
        .expect("data fixture");
    (columns, rows)
}

fn field<'a>(columns: &[&str], row: &'a Value, name: &str) -> &'a Value {
    let index = columns
        .iter()
        .position(|column| *column == name)
        .expect("колонка fixture");
    row.get(index).expect("ячейка fixture")
}

fn required_date(columns: &[&str], row: &Value, name: &str) -> Date {
    date_of(field(columns, row, name).as_str().expect("дата строкой"))
}

fn optional_dec(columns: &[&str], row: &Value, name: &str) -> Option<Dec> {
    let value = field(columns, row, name);
    if value.is_null() {
        None
    } else {
        let text = value
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| value.to_string());
        Some(dec(&text))
    }
}

fn schedule_from_fixture(raw: &str, instrument: InstrumentId) -> BondSchedule {
    let root: Value = serde_json::from_str(raw).expect("JSON fixture");
    let (coupon_columns, coupon_rows) = block(&root, "coupons");
    let periods = coupon_rows
        .iter()
        .map(|row| AccrualPeriod {
            period_start: required_date(&coupon_columns, row, "startdate"),
            accrual_end: required_date(&coupon_columns, row, "coupondate"),
            payment_date: required_date(&coupon_columns, row, "coupondate"),
            record_date: None,
            coupon_per_unit: optional_dec(&coupon_columns, row, "value")
                .map(|value| PerUnitAmount::new(value, CurrencyCode::Rub)),
        })
        .collect();

    let (amort_columns, amort_rows) = block(&root, "amortizations");
    let principal_returns = amort_rows
        .iter()
        .map(|row| PrincipalReturn {
            repayment_date: required_date(&amort_columns, row, "amortdate"),
            share_percent: optional_dec(&amort_columns, row, "valueprc")
                .expect("доля возврата fixture"),
        })
        .collect();

    let (offer_columns, offer_rows) = block(&root, "offers");
    let offer_windows = offer_rows
        .iter()
        .map(|row| {
            let execution_date = required_date(&offer_columns, row, "offerdate");
            let source_kind = field(&offer_columns, row, "offertype")
                .as_str()
                .expect("вид оферты fixture");
            let right = match source_kind {
                "Оферта" => OfferRight::HolderPut,
                "Оферта (состоялось)" => OfferRight::HolderPutSettled,
                other => panic!("неожиданный вид оферты fixture: {other}"),
            };
            iaam_core::bond::offer::OfferWindowTerms {
                window: iaam_core::bond::offer::OfferWindowId::derive(instrument, execution_date),
                right,
                execution_date,
                submission_start: None,
                submission_end: None,
                price_percent: optional_dec(&offer_columns, row, "price"),
            }
        })
        .collect();

    BondSchedule {
        periods,
        principal_returns,
        initial_principal: None,
        offer_windows,
        completeness: ScheduleCompleteness::Validated,
        default_flags: Some(DefaultFlags {
            declared: false,
            technical: false,
        }),
        currency_roles: Some(CurrencyRoles::uniform(CurrencyCode::Rub)),
    }
}

fn principal() -> iaam_core::rules::lot_disposal::PrincipalState {
    iaam_core::rules::lot_disposal::PrincipalState::known(
        PerUnitAmount::new(dec("1000"), CurrencyCode::Rub),
        PerUnitAmount::new(dec("1000"), CurrencyCode::Rub),
    )
    .expect("известный номинал")
}

fn project(
    raw: &str,
    instrument: InstrumentId,
    as_of: Date,
    choice: &OfferChoice,
) -> iaam_core::rules::CashflowPlan {
    let schedule = schedule_from_fixture(raw, instrument);
    CashflowProjectionV1
        .future_postings(&CashflowInput {
            schedule: &schedule,
            principal: principal(),
            quantity: Quantity(dec("1")),
            choice,
            as_of,
            report_currency: CurrencyCode::Rub,
        })
        .expect("график проецируется")
}

fn assert_rate(outcome: &RateOutcome, expected: f64, label: &str) {
    let approx = outcome.rate();
    let delta = (approx.value() - expected).abs();
    assert!(
        delta <= approx.error_bound() + 1e-12,
        "{label}: {} против независимого эталона {expected}, ошибка {delta}, граница {}",
        approx.value(),
        approx.error_bound()
    );
}

#[test]
fn fixed_coupon_fixture_matches_independent_schedule_ytm_and_cagr() {
    let instrument = InstrumentId::new_random();
    let as_of = date!(2040 - 11 - 14);
    let terminal = date!(2041 - 05 - 15);
    let choice = OfferChoice::HoldToMaturity;
    let plan = project(FIXED_COUPON, instrument, as_of, &choice);

    // Единственная будущая дата 2041-05-15: купон 35.40 ₽ и возврат 1,000 ₽;
    // C0 = 1,000 ₽. Поэтому W_T = 1,035.40 ₽, surplus = 35.40 ₽,
    // HPR = 1,035.40 / 1,000 - 1 = 0.0354, T = 182 дня.
    assert_eq!(
        plan.postings,
        vec![
            ExpectedPosting {
                date: terminal,
                amount: calc("35.4"),
                kind: PostingKind::Coupon,
            },
            ExpectedPosting {
                date: terminal,
                amount: calc("1000"),
                kind: PostingKind::PrincipalReturn,
            },
        ]
    );
    assert_eq!(plan.terminal_date, terminal);

    let metric = prospective_metric(as_of, &plan, Computed::Value(calc("1000")), &choice);
    let metrics = metric
        .metrics
        .value()
        .expect("метрики фиксированного купона");
    assert_eq!(metrics.terminal_wealth, calc("1035.4"));
    assert_eq!(metrics.surplus, calc("35.4"));
    assert_eq!(metrics.hpr, Computed::Value(dec("0.0354")));
    assert_eq!(metric.irr_label, IrrLabel::YieldToMaturity);

    // Независимо: (1,035.40 / 1,000)^(365 / 182) - 1.
    // При одном терминальном дне эта же арифметика задаёт YTM через NPV.
    let expected_rate = 0.072_258_093_861_151_67_f64;
    assert_rate(
        metric.irr.value().expect("YTM фиксированного купона"),
        expected_rate,
        "YTM fixed",
    );
    assert_rate(
        metrics
            .cagr_0r
            .value()
            .expect("CAGR_0R фиксированного купона"),
        expected_rate,
        "CAGR fixed",
    );
}

#[test]
fn amortised_fixture_matches_independent_schedule_ytm_and_cagr() {
    let instrument = InstrumentId::new_random();
    let as_of = date!(2034 - 08 - 07);
    let terminal = date!(2036 - 02 - 06);
    let choice = OfferChoice::HoldToMaturity;
    let plan = project(AMORTISED, instrument, as_of, &choice);

    // Будущие купоны: 34.41 ₽ (09.08.2034), 25.80 ₽ (07.02.2035),
    // 17.20 ₽ (08.08.2035), 8.60 ₽ (06.02.2036). Возвраты номинала:
    // по 250 ₽ на 09.08.2034, 07.02.2035, 08.08.2035 и 06.02.2036.
    // C0 = 1,000 ₽; W_T = 34.41 + 25.80 + 17.20 + 8.60 + 4*250
    // = 1,086.01 ₽; surplus = 86.01 ₽; HPR = 1,086.01/1,000 - 1
    // = 0.08601; T = 548 дней.
    assert_eq!(
        plan.postings,
        vec![
            ExpectedPosting {
                date: date!(2034 - 08 - 09),
                amount: calc("34.41"),
                kind: PostingKind::Coupon
            },
            ExpectedPosting {
                date: date!(2034 - 08 - 09),
                amount: calc("250"),
                kind: PostingKind::PrincipalReturn
            },
            ExpectedPosting {
                date: date!(2035 - 02 - 07),
                amount: calc("25.8"),
                kind: PostingKind::Coupon
            },
            ExpectedPosting {
                date: date!(2035 - 02 - 07),
                amount: calc("250"),
                kind: PostingKind::PrincipalReturn
            },
            ExpectedPosting {
                date: date!(2035 - 08 - 08),
                amount: calc("17.2"),
                kind: PostingKind::Coupon
            },
            ExpectedPosting {
                date: date!(2035 - 08 - 08),
                amount: calc("250"),
                kind: PostingKind::PrincipalReturn
            },
            ExpectedPosting {
                date: terminal,
                amount: calc("8.6"),
                kind: PostingKind::Coupon
            },
            ExpectedPosting {
                date: terminal,
                amount: calc("250"),
                kind: PostingKind::PrincipalReturn
            },
        ]
    );
    assert_eq!(plan.terminal_date, terminal);

    let metric = prospective_metric(as_of, &plan, Computed::Value(calc("1000")), &choice);
    let metrics = metric
        .metrics
        .value()
        .expect("метрики амортизируемого выпуска");
    assert_eq!(metrics.terminal_wealth, calc("1086.01"));
    assert_eq!(metrics.surplus, calc("86.01"));
    assert_eq!(metrics.hpr, Computed::Value(dec("0.08601")));
    assert_eq!(metric.irr_label, IrrLabel::YieldToMaturity);

    // Независимо: CAGR_0R = (1,086.01/1,000)^(365/548) - 1.
    // Независимый YTM — корень NPV = 0 для потоков:
    // -1,000 в день 0; 284.41 в день 2; 275.80 в день 184;
    // 267.20 в день 366; 258.60 в день 548.
    let expected_cagr = 0.056_494_935_308_105_676_f64;
    let expected_ytm = 0.122_174_516_159_886_06_f64;
    assert_rate(
        metric.irr.value().expect("YTM амортизируемого выпуска"),
        expected_ytm,
        "YTM amortised",
    );
    assert_rate(
        metrics
            .cagr_0r
            .value()
            .expect("CAGR_0R амортизируемого выпуска"),
        expected_cagr,
        "CAGR amortised",
    );
}

#[test]
fn floater_fixture_has_reproducible_coupon_undetermined_refusal() {
    let instrument = InstrumentId::new_random();
    let schedule = schedule_from_fixture(FLOATER, instrument);
    let choice = OfferChoice::HoldToMaturity;
    let error = CashflowProjectionV1
        .future_postings(&CashflowInput {
            schedule: &schedule,
            principal: principal(),
            quantity: Quantity(dec("1")),
            choice: &choice,
            as_of: date!(2020 - 05 - 12),
            report_currency: CurrencyCode::Rub,
        })
        .expect_err("неопределённый будущий купон должен отказать");
    assert_eq!(
        error,
        CashflowError::CouponUndetermined {
            period_start: date!(2020 - 05 - 13),
        }
    );

    // На границе returns этот отказ становится not_computable с той же
    // причиной, а не нулевым купоном и не правдоподобным YTM.
    let reason = match error {
        CashflowError::CouponUndetermined { .. } => {
            NotComputable::CouponUndetermined { instrument }
        }
        other => panic!("неожиданная причина отказа: {other}"),
    };
    let metric = prospective_metric(
        date!(2020 - 05 - 12),
        &iaam_core::rules::CashflowPlan {
            postings: Vec::new(),
            terminal_date: date!(2026 - 03 - 25),
            past: Vec::new(),
        },
        Computed::NotComputable { reason },
        &choice,
    );
    assert_eq!(
        metric.metrics.reason().expect("причина отказа").code(),
        "coupon_undetermined"
    );
    assert_eq!(
        metric.irr.reason().expect("причина отказа").code(),
        "coupon_undetermined"
    );
}

#[test]
fn offers_fixture_lists_only_future_priced_holder_windows() {
    let instrument = InstrumentId::new_random();
    let schedule = schedule_from_fixture(OFFERS, instrument);
    let choices = iaam_core::bond::offer::available_choices(&schedule, date!(2012 - 03 - 13));

    let windows: Vec<_> = choices
        .iter()
        .filter_map(|choice| match choice {
            OfferChoice::ExerciseAtOffer { window } => Some(*window),
            OfferChoice::HoldToMaturity => None,
        })
        .collect();
    assert_eq!(choices.len(), 6);
    assert_eq!(windows.len(), 5);

    for choice in choices {
        let scenario_as_of = match &choice {
            // Все купоны до погашения уже прошли, поэтому неизвестные
            // купоны после горизонта не участвуют в сценарии удержания.
            OfferChoice::HoldToMaturity => date!(2032 - 02 - 17),
            OfferChoice::ExerciseAtOffer { window } => {
                schedule
                    .offer_windows
                    .iter()
                    .find(|terms| terms.window == *window)
                    .expect("окно оферты")
                    .execution_date
                    - Duration::days(1)
            }
        };
        let plan = project(OFFERS, instrument, scenario_as_of, &choice);
        match choice {
            OfferChoice::HoldToMaturity => {
                assert_eq!(plan.terminal_date, date!(2032 - 02 - 17));
            }
            OfferChoice::ExerciseAtOffer { window } => {
                let terms = schedule
                    .offer_windows
                    .iter()
                    .find(|terms| terms.window == window)
                    .expect("окно оферты");
                assert_eq!(terms.right, OfferRight::HolderPut);
                assert!(terms.price_percent.is_some());
                assert_eq!(plan.terminal_date, terms.execution_date);
                let metric = prospective_metric(
                    scenario_as_of,
                    &plan,
                    Computed::Value(calc("1000")),
                    &choice,
                );
                assert_eq!(metric.irr_label, IrrLabel::YieldToOffer);
            }
        }
    }
}
