//! Корпоративные действия — типизированное семейство (§4.7).
//!
//! Один универсальный `corporate_action` с мешком необязательных полей
//! превратился бы в невалидируемый JSON: инварианта, который отличает
//! амортизацию от замещения, у такого мешка нет. Здесь у каждого члена
//! свои поля, и `match` по семейству исчерпывающий — новый член обязан
//! сломать сборку везде, где его не обработали (§15.1).

use serde::{Deserialize, Serialize};
use time::Date;

use crate::ids::{CustodyId, InstrumentId};
use crate::money::{Money, PerUnitAmount, Quantity};
use crate::numeric::decimal::Dec;

/// Корпоративное действие по бумаге.
///
/// Исчерпаемый `enum` без `#[non_exhaustive]` — по той же причине, что
/// и [`crate::event::kind::EventKind`]: добавление члена обязано
/// сломать сборку везде, где разбор не полон (§15.1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum CorporateAction {
    /// Амортизация: непогашенный номинал уменьшается, деньги приходят,
    /// **количество бумаг не меняется** (§6.5).
    PartialRedemption {
        instrument: InstrumentId,
        /// Место хранения — факт о выплате, а **не** ключ выборки лотов:
        /// `LotKey` намеренно не различает депозитарии
        /// (`projection/lots.rs`), и перенос бумаги между ними партии
        /// не создаёт.
        custody: CustodyId,
        /// Количество, которого касается выплата. Проекция его сверяет,
        /// а не масштабирует по нему номинал: расхождение с позицией —
        /// брак источника, а не повод пересчитать.
        quantity: Quantity,
        principal_returned_per_unit: PerUnitAmount,
        /// Денежная компенсация, фактически поступившая владельцу.
        /// Она может отличаться от возвращённого номинала — на удержанный
        /// налог, например, — и потому записывается отдельно.
        compensation: Money,
        effective_date: Date,
        record_date: Option<Date>,
        grounds: Option<String>,
    },
    /// Окончательное погашение: номинал возвращён целиком и бумага
    /// выбывает из позиции.
    ///
    /// Отдельный член, а не амортизация до нуля: обнулить остаток и
    /// оставить количество значило бы держать позицию из погашенных
    /// бумаг, которой не существует.
    Redemption {
        instrument: InstrumentId,
        custody: CustodyId,
        quantity: Quantity,
        principal_returned_per_unit: PerUnitAmount,
        compensation: Money,
        effective_date: Date,
        record_date: Option<Date>,
        grounds: Option<String>,
    },
    /// Замещение: бумага предшественника меняется на бумагу преемника.
    ///
    /// Поля подобраны так, чтобы E5 посчитал перенос налоговой стоимости
    /// и срока владения, ничего не угадывая (§16.1). Правило переноса
    /// хранится в самом факте: вывести его позже будет нечем — условия
    /// замещения живут в решении эмитента, а не в справочнике.
    Conversion {
        predecessor: InstrumentId,
        successor: InstrumentId,
        custody: CustodyId,
        /// Сколько бумаг преемника приходится на одну бумагу
        /// предшественника.
        ratio: Dec,
        quantity_in: Quantity,
        quantity_out: Quantity,
        fractional: FractionalTreatment,
        /// Компенсация дробей. Как она влияет на налоговую базу —
        /// правило E5; часть 1 её только сохраняет.
        compensation: Option<Money>,
        effective_date: Date,
        record_date: Option<Date>,
        grounds: Option<String>,
        basis_transfer: BasisTransferRule,
    },
}

impl CorporateAction {
    /// Имя члена для диагностики и заслонов. Тот же приём, что
    /// у [`crate::event::kind::EventKind::discriminant`].
    #[must_use]
    pub const fn discriminant(&self) -> &'static str {
        match self {
            Self::PartialRedemption { .. } => "partial_redemption",
            Self::Redemption { .. } => "redemption",
            Self::Conversion { .. } => "conversion",
        }
    }

    /// Дата вступления в силу — идентичность факта, поэтому обязательна
    /// у каждого члена и достаётся без разбора семейства.
    #[must_use]
    pub const fn effective_date(&self) -> Date {
        match self {
            Self::PartialRedemption { effective_date, .. }
            | Self::Redemption { effective_date, .. }
            | Self::Conversion { effective_date, .. } => *effective_date,
        }
    }
}

/// Что сделали с дробной частью при замещении.
///
/// Отдельный член на «дроби не возникло»: `None` означал бы «неизвестно»,
/// а это разные вещи (§4.9).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FractionalTreatment {
    /// Дробь выкуплена деньгами.
    CashCompensated,
    /// Дробь отброшена вниз без компенсации.
    RoundedDown,
    /// Дроби не возникло.
    NotApplicable,
}

/// Правило переноса налоговой стоимости и срока владения при замещении.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BasisTransferRule {
    /// Стоимость и срок владения переходят на преемника целиком.
    CarryOver,
    /// Замещение приравнено к продаже и покупке: срок начинается заново.
    Restart,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::money::{CurrencyCode, PostedMinor};
    use crate::numeric::decimal::Dec;
    use rust_decimal::Decimal;
    use time::macros::date;

    fn rub(minor: i64) -> Money {
        Money::new(PostedMinor::new(minor), CurrencyCode::Rub)
    }

    fn dec(text: &str) -> Dec {
        Dec::new(Decimal::from_str_exact(text).unwrap())
    }

    fn per_unit(text: &str) -> PerUnitAmount {
        PerUnitAmount::new(dec(text), CurrencyCode::Rub)
    }

    fn sample_partial_redemption() -> CorporateAction {
        CorporateAction::PartialRedemption {
            instrument: InstrumentId::new_random(),
            custody: CustodyId::new_random(),
            quantity: Quantity(dec("100")),
            principal_returned_per_unit: per_unit("200.0000"),
            compensation: rub(2_000_000),
            effective_date: date!(2026 - 06 - 15),
            record_date: Some(date!(2026 - 06 - 13)),
            grounds: Some("решение эмитента №4".to_owned()),
        }
    }

    fn sample_redemption() -> CorporateAction {
        CorporateAction::Redemption {
            instrument: InstrumentId::new_random(),
            custody: CustodyId::new_random(),
            quantity: Quantity(dec("100")),
            principal_returned_per_unit: per_unit("800.0000"),
            compensation: rub(8_000_000),
            effective_date: date!(2026 - 12 - 15),
            record_date: None,
            grounds: None,
        }
    }

    fn sample_conversion() -> CorporateAction {
        CorporateAction::Conversion {
            predecessor: InstrumentId::new_random(),
            successor: InstrumentId::new_random(),
            custody: CustodyId::new_random(),
            ratio: dec("1.5"),
            quantity_in: Quantity(dec("100")),
            quantity_out: Quantity(dec("150")),
            fractional: FractionalTreatment::NotApplicable,
            compensation: None,
            effective_date: date!(2026 - 09 - 01),
            record_date: Some(date!(2026 - 08 - 30)),
            grounds: None,
            basis_transfer: BasisTransferRule::CarryOver,
        }
    }

    #[test]
    fn every_corporate_action_survives_a_json_round_trip() {
        for action in [
            sample_partial_redemption(),
            sample_redemption(),
            sample_conversion(),
        ] {
            let text = serde_json::to_string(&action).unwrap();
            assert_eq!(
                serde_json::from_str::<CorporateAction>(&text).unwrap(),
                action
            );
        }
    }

    #[test]
    fn every_corporate_action_names_itself() {
        assert_eq!(
            sample_partial_redemption().discriminant(),
            "partial_redemption"
        );
        assert_eq!(sample_redemption().discriminant(), "redemption");
        assert_eq!(sample_conversion().discriminant(), "conversion");
    }

    #[test]
    fn the_effective_date_of_every_action_is_reachable_without_a_match() {
        // Дата вступления в силу — идентичность факта, и она есть
        // у каждого члена: проекция обязана её получать, не разбирая
        // семейство заново на каждом вызове.
        assert_eq!(
            sample_partial_redemption().effective_date(),
            date!(2026 - 06 - 15)
        );
        assert_eq!(sample_redemption().effective_date(), date!(2026 - 12 - 15));
        assert_eq!(sample_conversion().effective_date(), date!(2026 - 09 - 01));
    }

    #[test]
    fn a_fractional_treatment_survives_a_json_round_trip() {
        for treatment in [
            FractionalTreatment::CashCompensated,
            FractionalTreatment::RoundedDown,
            FractionalTreatment::NotApplicable,
        ] {
            let text = serde_json::to_string(&treatment).unwrap();
            assert_eq!(
                serde_json::from_str::<FractionalTreatment>(&text).unwrap(),
                treatment
            );
        }
    }

    #[test]
    fn a_basis_transfer_rule_survives_a_json_round_trip() {
        for rule in [BasisTransferRule::CarryOver, BasisTransferRule::Restart] {
            let text = serde_json::to_string(&rule).unwrap();
            assert_eq!(
                serde_json::from_str::<BasisTransferRule>(&text).unwrap(),
                rule
            );
        }
    }
}
