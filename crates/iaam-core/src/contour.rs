//! Контуры (§4.10).
//!
//! Брокер считает перевод со вклада пополнением, потому что его контур —
//! только его собственный счёт. Владелец видит всю картину, поэтому
//! границу проводит он.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::event::Event;
use crate::event::kind::FlowEndpoints;
use crate::ids::AccountId;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ContourId(pub Uuid);

impl ContourId {
    #[must_use]
    pub fn new_random() -> Self {
        Self(Uuid::new_v4())
    }
}

/// Версия определения контура.
///
/// Расчёт доходности ссылается на версию: без этого изменение состава
/// контура задним числом молча меняет исторические цифры.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ContourVersion(pub u32);

/// Состав контура на конкретной версии.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContourDefinition {
    id: ContourId,
    version: ContourVersion,
    accounts: BTreeSet<AccountId>,
}

impl ContourDefinition {
    /// Тело вынесено в `from_parts`: `cargo-mutants` молча пропускает
    /// любую функцию с именем `new`, и сборка состава внутри `new`
    /// осталась бы вне мутационного заслона (§15.7).
    #[must_use]
    pub fn new(
        id: ContourId,
        version: ContourVersion,
        accounts: impl IntoIterator<Item = AccountId>,
    ) -> Self {
        Self::from_parts(id, version, accounts)
    }

    fn from_parts(
        id: ContourId,
        version: ContourVersion,
        accounts: impl IntoIterator<Item = AccountId>,
    ) -> Self {
        Self {
            id,
            version,
            accounts: accounts.into_iter().collect(),
        }
    }

    #[must_use]
    pub const fn id(&self) -> ContourId {
        self.id
    }

    #[must_use]
    pub const fn version(&self) -> ContourVersion {
        self.version
    }

    #[must_use]
    pub fn contains(&self, account: AccountId) -> bool {
        self.accounts.contains(&account)
    }
}

/// Отношение события к границе контура.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlowClass {
    /// Деньги вошли в контур извне. Входит в XIRR со знаком плюс.
    ExternalIn {
        contour: ContourId,
        version: ContourVersion,
    },
    /// Деньги вышли из контура. Входит в XIRR со знаком минус.
    ExternalOut {
        contour: ContourId,
        version: ContourVersion,
    },
    /// Внутри контура: меняет аллокацию, но не доходность.
    Internal,
    /// Событие к этому контуру не относится.
    Irrelevant,
}

/// Классификация события относительно контура.
///
/// Ключевое место всей системы: именно из-за путаницы здесь сервисы
/// показывают доходность, в которой собственные пополнения выглядят
/// заработком. Для перевода классификация определяется **парой**
/// принадлежностей, поэтому оба счёта обязаны храниться в событии.
#[must_use]
pub fn classify(def: &ContourDefinition, event: &Event) -> FlowClass {
    let inbound = FlowClass::ExternalIn {
        contour: def.id(),
        version: def.version(),
    };
    let outbound = FlowClass::ExternalOut {
        contour: def.id(),
        version: def.version(),
    };

    match event.kind.flow_endpoints() {
        FlowEndpoints::InboundFromOutside => {
            if def.contains(event.account) {
                inbound
            } else {
                FlowClass::Irrelevant
            }
        }
        FlowEndpoints::OutboundToOutside => {
            if def.contains(event.account) {
                outbound
            } else {
                FlowClass::Irrelevant
            }
        }
        FlowEndpoints::BetweenAccounts { from, to } => {
            match (def.contains(from), def.contains(to)) {
                (true, true) => FlowClass::Internal,
                (false, true) => inbound,
                (true, false) => outbound,
                (false, false) => FlowClass::Irrelevant,
            }
        }
        FlowEndpoints::WithinAccount => {
            if def.contains(event.account) {
                FlowClass::Internal
            } else {
                FlowClass::Irrelevant
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::Event;
    use crate::event::kind::{EventKind, TradeSide};
    use crate::event::leg::Leg;
    use crate::event::test_support::sample_event;
    use crate::ids::{AccountId, CustodyId, InstrumentId, TransferId};
    use crate::money::{CurrencyCode, Money, PostedMinor, Quantity};

    // Суммы записываются в минимальных единицах одним числом: группировка
    // вида `100_000_00` не компилируется (clippy::inconsistent_digit_grouping
    // входит в `all`, а `all = deny`).
    fn rub(minor: i64) -> Money {
        Money::new(PostedMinor::new(minor), CurrencyCode::Rub)
    }

    /// Перевод между двумя счетами.
    ///
    /// Ноги переписываются целиком, а не только `kind`: перевод требует
    /// **двух встречных денежных ног на объявленных счетах**
    /// (`validate_structure`, задача 10), а `sample_event` даёт одну ногу
    /// прихода. Событие, не проходящее структурную проверку, не может
    /// служить основанием для утверждений о классификации.
    fn transfer(from: AccountId, to: AccountId) -> Event {
        let amount = rub(10_000_000);
        let mut event = sample_event(0);
        event.account = from;
        event.kind = EventKind::CashTransfer {
            transfer_id: TransferId::new_random(),
            from,
            to,
            amount,
        };
        event.legs = vec![Leg::cash(from, rub(-10_000_000)), Leg::cash(to, amount)];
        event
    }

    /// Приход денег извне на счёт.
    fn cash_in(account: AccountId) -> Event {
        let amount = rub(1_000_000);
        let mut event = sample_event(0);
        event.account = account;
        event.kind = EventKind::CashIn { amount };
        event.legs = vec![Leg::cash(account, amount)];
        event
    }

    /// Уход денег со счёта наружу.
    fn cash_out(account: AccountId) -> Event {
        let amount = rub(-1_000_000);
        let mut event = sample_event(0);
        event.account = account;
        event.kind = EventKind::CashOut { amount };
        event.legs = vec![Leg::cash(account, amount)];
        event
    }

    /// Покупка бумаги: движение внутри одного счёта.
    fn purchase(account: AccountId) -> Event {
        let gross = rub(5_000_000);
        let instrument = InstrumentId::new_random();
        let mut event = sample_event(0);
        event.account = account;
        event.kind = EventKind::Trade {
            side: TradeSide::Buy,
            instrument,
            quantity: Quantity::zero(),
            gross,
            fee: None,
            accrued_interest: None,
        };
        event.legs = vec![
            Leg::cash(account, rub(-5_000_000)),
            Leg::security(
                account,
                CustodyId::new_random(),
                instrument,
                Quantity::zero(),
            ),
        ];
        event
    }

    fn contour(accounts: Vec<AccountId>) -> ContourDefinition {
        ContourDefinition::new(ContourId::new_random(), ContourVersion(1), accounts)
    }

    #[test]
    fn every_event_used_as_evidence_is_structurally_valid() {
        // Расхождение с планом. Тесты плана строили перевод подменой одного
        // лишь `kind` у `sample_event`, оставляя единственную ногу прихода:
        // такое событие отклоняется `validate_structure`. Классификация
        // события, которое журнал не принял бы, ничего не доказывает.
        let deposit = AccountId::new_random();
        let broker = AccountId::new_random();
        for event in [
            cash_in(broker),
            cash_out(broker),
            transfer(deposit, broker),
            purchase(broker),
        ] {
            let verdict = event.validate_structure();
            assert!(
                verdict.is_ok(),
                "{} не проходит структурную проверку: {verdict:?}",
                event.kind.discriminant()
            );
        }
    }

    // --- Критерии приёмки ---

    #[test]
    fn transfer_between_two_inside_accounts_is_internal() {
        // Вклад -> брокерский счёт, оба внутри контура «весь капитал».
        // Это не пополнение: доходность не меняется, меняется аллокация.
        let deposit = AccountId::new_random();
        let broker = AccountId::new_random();
        let def = contour(vec![deposit, broker]);
        assert_eq!(
            classify(&def, &transfer(deposit, broker)),
            FlowClass::Internal
        );
    }

    #[test]
    fn the_same_event_is_external_for_a_narrower_contour() {
        // Событие ОДНО И ТО ЖЕ, меняется только определение контура.
        // Прежняя редакция плана подменяла здесь CashTransfer на CashIn
        // и потому перевод вообще не тестировала.
        let deposit = AccountId::new_random();
        let broker = AccountId::new_random();
        let event = transfer(deposit, broker);

        let wide = contour(vec![deposit, broker]);
        let narrow = contour(vec![broker]);

        assert_eq!(classify(&wide, &event), FlowClass::Internal);
        assert!(
            matches!(classify(&narrow, &event), FlowClass::ExternalIn { .. }),
            "для узкого контура тот же перевод — приход извне"
        );
    }

    #[test]
    fn transfer_out_of_the_contour_is_external_out() {
        let broker = AccountId::new_random();
        let outside = AccountId::new_random();
        let def = contour(vec![broker]);
        assert!(matches!(
            classify(&def, &transfer(broker, outside)),
            FlowClass::ExternalOut { .. }
        ));
    }

    #[test]
    fn transfer_between_two_outside_accounts_is_irrelevant() {
        let a = AccountId::new_random();
        let b = AccountId::new_random();
        let def = contour(vec![AccountId::new_random()]);
        assert_eq!(classify(&def, &transfer(a, b)), FlowClass::Irrelevant);
    }

    #[test]
    fn the_direction_of_a_transfer_decides_the_sign_of_the_external_flow() {
        // Тот же контур и та же пара счетов: меняется только направление.
        // Перепутанные стороны дали бы XIRR приток вместо оттока.
        let broker = AccountId::new_random();
        let outside = AccountId::new_random();
        let def = contour(vec![broker]);
        assert!(matches!(
            classify(&def, &transfer(broker, outside)),
            FlowClass::ExternalOut { .. }
        ));
        assert!(matches!(
            classify(&def, &transfer(outside, broker)),
            FlowClass::ExternalIn { .. }
        ));
    }

    #[test]
    fn buying_a_security_is_internal_not_a_contribution() {
        let broker = AccountId::new_random();
        let def = contour(vec![broker]);
        assert_eq!(classify(&def, &purchase(broker)), FlowClass::Internal);
    }

    #[test]
    fn cash_in_on_an_outside_account_is_irrelevant() {
        let inside = AccountId::new_random();
        let outside = AccountId::new_random();
        let def = contour(vec![inside]);
        assert_eq!(classify(&def, &cash_in(outside)), FlowClass::Irrelevant);
    }

    // --- Полная таблица решений ---

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Expected {
        In,
        Out,
        Internal,
        Irrelevant,
    }

    fn observed(class: FlowClass) -> Expected {
        match class {
            FlowClass::ExternalIn { .. } => Expected::In,
            FlowClass::ExternalOut { .. } => Expected::Out,
            FlowClass::Internal => Expected::Internal,
            FlowClass::Irrelevant => Expected::Irrelevant,
        }
    }

    #[test]
    fn every_combination_of_movement_and_membership_is_classified() {
        // Четыре формы движения на четырёх составах контура. Для перевода
        // значима вся пара принадлежностей; для остальных форм второй счёт
        // не должен влиять ни на что — это и проверяют столбцы «только
        // второй счёт» и «оба счёта».
        use Expected::{In, Internal, Irrelevant, Out};

        let first = AccountId::new_random();
        let second = AccountId::new_random();
        let unrelated = AccountId::new_random();

        let contours = [
            ("ни одного из счетов события", contour(vec![unrelated])),
            ("только первый счёт", contour(vec![first])),
            ("только второй счёт", contour(vec![second])),
            ("оба счёта", contour(vec![first, second])),
        ];

        let rows: [(&str, Event, [Expected; 4]); 4] = [
            (
                "приход извне на первый счёт",
                cash_in(first),
                [Irrelevant, In, Irrelevant, In],
            ),
            (
                "уход наружу с первого счёта",
                cash_out(first),
                [Irrelevant, Out, Irrelevant, Out],
            ),
            (
                "перевод с первого счёта на второй",
                transfer(first, second),
                [Irrelevant, Out, In, Internal],
            ),
            (
                "покупка на первом счёте",
                purchase(first),
                [Irrelevant, Internal, Irrelevant, Internal],
            ),
        ];

        for (movement, event, expectations) in &rows {
            for ((shape, def), expected) in contours.iter().zip(expectations) {
                assert_eq!(
                    observed(classify(def, event)),
                    *expected,
                    "{movement} при контуре «{shape}»"
                );
            }
        }
    }

    // --- Определение контура ---

    #[test]
    fn contour_version_is_carried_into_the_classification() {
        let broker = AccountId::new_random();
        let id = ContourId::new_random();
        let def = ContourDefinition::new(id, ContourVersion(7), vec![broker]);
        assert_eq!(def.id(), id);
        assert_eq!(def.version(), ContourVersion(7));
        match classify(&def, &cash_in(broker)) {
            FlowClass::ExternalIn { contour, version } => {
                assert_eq!(contour, id);
                assert_eq!(version, ContourVersion(7));
            }
            other => panic!("ожидался ExternalIn, получено {other:?}"),
        }
    }

    #[test]
    fn an_outbound_flow_carries_the_same_definition() {
        // Без версии в исходящем потоке пересчёт задним числом молча
        // изменил бы исторические цифры только по одной стороне.
        let broker = AccountId::new_random();
        let id = ContourId::new_random();
        let def = ContourDefinition::new(id, ContourVersion(3), vec![broker]);
        match classify(&def, &cash_out(broker)) {
            FlowClass::ExternalOut { contour, version } => {
                assert_eq!(contour, id);
                assert_eq!(version, ContourVersion(3));
            }
            other => panic!("ожидался ExternalOut, получено {other:?}"),
        }
    }

    #[test]
    fn contains_answers_only_for_the_accounts_of_the_definition() {
        let inside = AccountId::new_random();
        let outside = AccountId::new_random();
        let def = contour(vec![inside]);
        assert!(def.contains(inside));
        assert!(!def.contains(outside));
    }

    #[test]
    fn a_repeated_account_does_not_make_a_different_definition() {
        // Состав — множество: счёт, названный дважды, не даёт второго
        // членства, иначе сравнение определений зависело бы от порядка
        // и повторов во входных данных.
        let id = ContourId::new_random();
        let account = AccountId::new_random();
        let twice = ContourDefinition::new(id, ContourVersion(1), vec![account, account]);
        let once = ContourDefinition::new(id, ContourVersion(1), vec![account]);
        assert_eq!(twice, once);
    }

    #[test]
    fn the_order_of_accounts_does_not_change_the_definition() {
        let id = ContourId::new_random();
        let first = AccountId::new_random();
        let second = AccountId::new_random();
        let forward = ContourDefinition::new(id, ContourVersion(1), vec![first, second]);
        let backward = ContourDefinition::new(id, ContourVersion(1), vec![second, first]);
        assert_eq!(forward, backward);
    }

    #[test]
    fn a_definition_keeps_every_account_it_was_given() {
        // Отбрасывание состава сделало бы весь капитал внешним.
        let first = AccountId::new_random();
        let second = AccountId::new_random();
        let def = contour(vec![first, second]);
        assert!(def.contains(first));
        assert!(def.contains(second));
    }

    #[test]
    fn two_random_contour_ids_are_distinct() {
        assert_ne!(ContourId::new_random(), ContourId::new_random());
    }
}
