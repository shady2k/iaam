//! Ожидания от ног события (§15.2).
//!
//! Существующие помощники `expect_single_cash` и `validate_trade` сверяют
//! вид ноги, сумму и знак — но не счёт, не бумагу и не место хранения.
//! Заслон, пропускающий ногу по чужой бумаге, декоративен: событие
//! с посторонним движением не является тем событием, которым назвалось.

use super::leg::{Leg, LegKind};
use super::{Event, EventValidationError};
use crate::ids::{AccountId, CustodyId, InstrumentId};
use crate::money::{Money, Quantity};

/// Ожидание от одной ноги.
///
/// Незаполненное поле не проверяется — заполненное обязано совпасть.
/// Вид и счёт обязательны: нога без них не описана вовсе.
#[derive(Debug, Clone, PartialEq)]
pub struct LegExpectation {
    pub kind: LegKind,
    pub account: AccountId,
    pub instrument: Option<InstrumentId>,
    pub custody: Option<CustodyId>,
    pub money: Option<Money>,
    pub quantity: Option<Quantity>,
}

impl Event {
    /// **Ровно** перечисленные ноги, в любом порядке.
    ///
    /// Лишняя нога — такая же ошибка, как недостающая: событие
    /// с посторонним движением не является тем событием, которым
    /// назвалось. Порядок ног не проверяется: источник волен записать
    /// их как угодно.
    pub fn expect_legs(
        &self,
        name: &'static str,
        expected: &[LegExpectation],
    ) -> Result<(), EventValidationError> {
        let mut taken = vec![false; self.legs.len()];
        if self.legs.len() == expected.len() && assign(&self.legs, expected, &mut taken) {
            return Ok(());
        }
        Err(self.diagnose(name, expected))
    }

    /// Почему раскладка не сошлась. Точность диагноза важна: «ног не
    /// столько» ничего не говорит тому, кто читает отказ импорта.
    fn diagnose(&self, name: &'static str, expected: &[LegExpectation]) -> EventValidationError {
        let found = self.legs.len();
        let want = expected.len();
        // Ожидание, к которому не подходит ни одна нога, — самая точная
        // жалоба: она называет поле, а не число.
        for expectation in expected {
            if self.legs.iter().any(|leg| matches(leg, expectation)) {
                continue;
            }
            // Ни одна нога не подошла, поэтому у любой ноги того же вида
            // найдётся несовпавшее поле — а его имя и есть диагноз.
            let mismatch = self
                .legs
                .iter()
                .filter(|leg| leg.kind == expectation.kind)
                .find_map(|leg| first_difference(leg, expectation));
            return match mismatch {
                Some(field) => EventValidationError::LegMismatch {
                    event: name,
                    kind: expectation.kind,
                    field,
                },
                None => EventValidationError::MissingLeg {
                    event: name,
                    kind: expectation.kind,
                    expected: want,
                    found,
                },
            };
        }
        // Каждое ожидание выполнимо по отдельности — значит, дело
        // в числе ног: либо нога осталась лишней, либо два ожидания
        // претендуют на одну и ту же.
        if found < want {
            return EventValidationError::MissingLeg {
                event: name,
                kind: expected[want - 1].kind,
                expected: want,
                found,
            };
        }
        EventValidationError::UnexpectedLeg {
            event: name,
            expected: want,
            found,
        }
    }
}

/// Полный перебор с возвратом, а не жадная раскладка.
///
/// Жадная сопоставила бы незаполненное ожидание первой подошедшей ноге
/// и объявила бы событие неправильным, хотя раскладка существует.
/// Ног у события единицы, поэтому цена перебора неощутима.
fn assign(legs: &[Leg], expected: &[LegExpectation], taken: &mut [bool]) -> bool {
    let Some((first, rest)) = expected.split_first() else {
        return true;
    };
    for (index, leg) in legs.iter().enumerate() {
        if taken[index] || !matches(leg, first) {
            continue;
        }
        taken[index] = true;
        if assign(legs, rest, taken) {
            return true;
        }
        taken[index] = false;
    }
    false
}

fn matches(leg: &Leg, expectation: &LegExpectation) -> bool {
    first_difference(leg, expectation).is_none()
}

/// Имя первого несовпавшего поля, если оно есть. Порядок полей
/// фиксирован: диагноз обязан быть воспроизводимым, иначе один и тот же
/// брак объясняется каждый раз по-разному.
fn first_difference(leg: &Leg, expectation: &LegExpectation) -> Option<&'static str> {
    if leg.kind != expectation.kind {
        return Some("kind");
    }
    if leg.account != expectation.account {
        return Some("account");
    }
    if differs(leg.instrument, expectation.instrument) {
        return Some("instrument");
    }
    if differs(leg.custody, expectation.custody) {
        return Some("custody");
    }
    if differs(leg.money, expectation.money) {
        return Some("money");
    }
    if differs(leg.quantity, expectation.quantity) {
        return Some("quantity");
    }
    None
}

/// Незаполненное ожидание не проверяется; заполненное обязано совпасть,
/// в том числе с пустым полем ноги.
fn differs<T: PartialEq>(actual: Option<T>, wanted: Option<T>) -> bool {
    wanted.is_some_and(|wanted| actual != Some(wanted))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::Leg;
    use crate::event::kind::EventKind;
    use crate::event::test_support::event_with;
    use crate::money::{CurrencyCode, PostedMinor};
    use crate::numeric::decimal::Dec;
    use rust_decimal::Decimal;
    use time::macros::date;

    fn rub(minor: i64) -> Money {
        Money::new(PostedMinor::new(minor), CurrencyCode::Rub)
    }

    fn qty(text: &str) -> Quantity {
        Quantity(Dec::new(Decimal::from_str_exact(text).unwrap()))
    }

    fn with_legs(account: AccountId, legs: Vec<Leg>) -> Event {
        event_with(
            account,
            date!(2026 - 03 - 01),
            0,
            EventKind::CashIn { amount: rub(1) },
            legs,
        )
    }

    fn principal_expectation(
        account: AccountId,
        instrument: InstrumentId,
        money: Money,
    ) -> LegExpectation {
        LegExpectation {
            kind: LegKind::Principal,
            account,
            instrument: Some(instrument),
            custody: None,
            money: Some(money),
            quantity: None,
        }
    }

    #[test]
    fn a_leg_naming_another_instrument_is_refused() {
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let other = InstrumentId::new_random();
        let event = with_legs(account, vec![Leg::principal(account, other, rub(100_000))]);

        assert_eq!(
            event.expect_legs(
                "x",
                &[principal_expectation(account, instrument, rub(100_000))]
            ),
            Err(EventValidationError::LegMismatch {
                event: "x",
                kind: LegKind::Principal,
                field: "instrument",
            })
        );
    }

    #[test]
    fn an_extra_leg_is_refused_like_a_missing_one() {
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let event = with_legs(
            account,
            vec![
                Leg::principal(account, instrument, rub(100_000)),
                Leg::cash(account, rub(1)),
            ],
        );

        assert_eq!(
            event.expect_legs(
                "x",
                &[principal_expectation(account, instrument, rub(100_000))]
            ),
            Err(EventValidationError::UnexpectedLeg {
                event: "x",
                expected: 1,
                found: 2,
            })
        );
    }

    #[test]
    fn a_leg_of_a_kind_that_is_not_there_at_all_is_reported_as_missing() {
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let event = with_legs(account, vec![Leg::cash(account, rub(100_000))]);

        assert_eq!(
            event.expect_legs(
                "x",
                &[principal_expectation(account, instrument, rub(100_000))]
            ),
            Err(EventValidationError::MissingLeg {
                event: "x",
                kind: LegKind::Principal,
                expected: 1,
                found: 1,
            })
        );
    }

    #[test]
    fn a_leg_held_in_another_custody_is_refused() {
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let custody = CustodyId::new_random();
        let other = CustodyId::new_random();
        let event = with_legs(
            account,
            vec![Leg::security(account, other, instrument, qty("10"))],
        );

        assert_eq!(
            event.expect_legs(
                "x",
                &[LegExpectation {
                    kind: LegKind::SecurityQuantity,
                    account,
                    instrument: Some(instrument),
                    custody: Some(custody),
                    money: None,
                    quantity: Some(qty("10")),
                }]
            ),
            Err(EventValidationError::LegMismatch {
                event: "x",
                kind: LegKind::SecurityQuantity,
                field: "custody",
            })
        );
    }

    #[test]
    fn a_quantity_of_the_wrong_sign_is_refused() {
        // Выбытие записывается отрицательным количеством: та же величина
        // с обратным знаком — противоположное движение, а не описка.
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let custody = CustodyId::new_random();
        let event = with_legs(
            account,
            vec![Leg::security(account, custody, instrument, qty("10"))],
        );

        assert_eq!(
            event.expect_legs(
                "x",
                &[LegExpectation {
                    kind: LegKind::SecurityQuantity,
                    account,
                    instrument: Some(instrument),
                    custody: Some(custody),
                    money: None,
                    quantity: Some(qty("-10")),
                }]
            ),
            Err(EventValidationError::LegMismatch {
                event: "x",
                kind: LegKind::SecurityQuantity,
                field: "quantity",
            })
        );
    }

    #[test]
    fn a_leg_booked_to_another_account_is_refused() {
        let account = AccountId::new_random();
        let other = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let event = with_legs(
            account,
            vec![Leg::principal(other, instrument, rub(100_000))],
        );

        assert_eq!(
            event.expect_legs(
                "x",
                &[principal_expectation(account, instrument, rub(100_000))]
            ),
            Err(EventValidationError::LegMismatch {
                event: "x",
                kind: LegKind::Principal,
                field: "account",
            })
        );
    }

    #[test]
    fn a_leg_carrying_another_amount_is_refused() {
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let event = with_legs(
            account,
            vec![Leg::principal(account, instrument, rub(99_999))],
        );

        assert_eq!(
            event.expect_legs(
                "x",
                &[principal_expectation(account, instrument, rub(100_000))]
            ),
            Err(EventValidationError::LegMismatch {
                event: "x",
                kind: LegKind::Principal,
                field: "money",
            })
        );
    }

    #[test]
    fn matching_legs_pass_regardless_of_the_order_they_were_written_in() {
        // Порядок ног в событии не несёт смысла: источник волен записать
        // их как угодно, и заслон, зависящий от порядка, ловил бы порядок.
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let event = with_legs(
            account,
            vec![
                Leg::cash(account, rub(1_000)),
                Leg::principal(account, instrument, rub(100_000)),
            ],
        );

        let expectations = [
            principal_expectation(account, instrument, rub(100_000)),
            LegExpectation {
                kind: LegKind::Cash,
                account,
                instrument: None,
                custody: None,
                money: Some(rub(1_000)),
                quantity: None,
            },
        ];

        assert_eq!(event.expect_legs("x", &expectations), Ok(()));
    }

    #[test]
    fn two_expectations_never_settle_on_the_same_leg() {
        // Жадная раскладка сопоставила бы обе ноги одному ожиданию
        // и объявила бы событие правильным, потеряв вторую ногу.
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let event = with_legs(
            account,
            vec![
                Leg::principal(account, instrument, rub(100_000)),
                Leg::principal(account, instrument, rub(200_000)),
            ],
        );

        let loose = LegExpectation {
            kind: LegKind::Principal,
            account,
            instrument: Some(instrument),
            custody: None,
            money: None,
            quantity: None,
        };

        // Незаполненное ожидание подходит к обеим ногам; заполненное —
        // только к одной. Раскладка обязана найти её, а не сдаться.
        assert_eq!(
            event.expect_legs(
                "x",
                &[
                    loose.clone(),
                    principal_expectation(account, instrument, rub(200_000))
                ]
            ),
            Ok(())
        );
    }

    #[test]
    fn an_unfilled_expectation_field_is_not_checked() {
        // Незаполненное поле — «не проверяется», а не «обязано быть пустым».
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let event = with_legs(
            account,
            vec![Leg::principal(account, instrument, rub(100_000))],
        );

        assert_eq!(
            event.expect_legs(
                "x",
                &[LegExpectation {
                    kind: LegKind::Principal,
                    account,
                    instrument: None,
                    custody: None,
                    money: None,
                    quantity: None,
                }]
            ),
            Ok(())
        );
    }

    #[test]
    fn an_event_with_no_legs_meets_an_empty_expectation() {
        // Поданная заявка по оферте ног не имеет: ни денег, ни бумаг она
        // не двигает. Заслон обязан это подтверждать, а не отказывать.
        let account = AccountId::new_random();
        let event = with_legs(account, Vec::new());
        assert_eq!(event.expect_legs("x", &[]), Ok(()));
    }

    #[test]
    fn a_leg_where_none_was_expected_is_refused() {
        let account = AccountId::new_random();
        let event = with_legs(account, vec![Leg::cash(account, rub(1))]);
        assert_eq!(
            event.expect_legs("x", &[]),
            Err(EventValidationError::UnexpectedLeg {
                event: "x",
                expected: 0,
                found: 1,
            })
        );
    }
}
