//! Восстановленное начало как набор утверждений (§10.7).
//!
//! Журнал append-only: события, записанные до появления поля
//! `assertions`, обязаны читаться без миграции. Проверяется это на
//! JSON, **выписанном в тесте руками**, а не сгенерированном текущим
//! кодом: сгенерированный текущим кодом образец проверял бы сам себя
//! и прошёл бы даже после несовместимого изменения схемы (§15.7).

use iaam_core::event::Event;
use iaam_core::event::kind::{
    BasisCertainty, Certainty, DateCertainty, EventKind, Knowledge, OpeningAssertions, Tristate,
};

/// Событие версии 2 в том виде, в каком оно лежит в уже созданных
/// базах: поля `assertions` в нём нет.
///
/// Формат полей (дата как пара «год и день года», количество строкой)
/// взят из действующей сериализации — это форма, а не ожидаемое
/// значение. Проверяемое утверждение выписано руками: раздел `kind`
/// **не содержит** `assertions`, и именно это делает образец записью
/// прежней версии, а не свежим выводом текущего кода.
const RECORDED_AT_VERSION_TWO: &str = r#"{
  "id": "6f1a2b3c-4d5e-4f60-8112-233445566778",
  "schema_version": 2,
  "owner": "11111111-2222-4333-8444-555555555555",
  "account": "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee",
  "kind": {
    "OpeningPosition": {
      "instrument": "99999999-8888-4777-8666-555555555555",
      "quantity": "10",
      "cost_basis": null
    }
  },
  "dates": {
    "trade": null,
    "settled": null,
    "cash_posted": [2024, 61],
    "entitlement": null,
    "paid": null,
    "tax_period_override": null
  },
  "order": { "date": [2024, 61], "sequence": 1 },
  "legs": [],
  "provenance": {
    "source": "12121212-3434-4565-8787-989898989898",
    "raw_hash": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    "parser_version": "manual/1",
    "source_operation_id": null,
    "row": null
  },
  "relation": "None",
  "confidence": "Estimated",
  "idempotency_key": null
}"#;

#[test]
fn an_event_recorded_before_the_field_existed_still_reads() {
    // Умолчание — «неизвестно» по каждому пункту. Это не заглушка:
    // событие действительно ничего из перечисленного не утверждало,
    // и приписать ему Known значило бы задним числом объявить
    // документированным то, чего никто не видел.
    let event: Event = serde_json::from_str(RECORDED_AT_VERSION_TWO)
        .expect("событие версии 2 обязано читаться без миграции");
    assert_eq!(event.schema_version, 2);

    let EventKind::OpeningPosition { assertions, .. } = event.kind else {
        panic!("ожидалось восстановленное начало");
    };
    assert_eq!(assertions, OpeningAssertions::default());
    assert_eq!(assertions.quantity, Certainty::Estimated);
    assert_eq!(assertions.acquisition_date, None);
    assert_eq!(
        assertions.acquisition_date_certainty,
        DateCertainty::Unknown
    );
    assert_eq!(assertions.tax_basis, BasisCertainty::Unknown);
    assert_eq!(assertions.basis_currency, None);
    assert_eq!(assertions.basis_rate, None);
    assert_eq!(assertions.fees_included, Tristate::Unknown);
    assert_eq!(assertions.ldv_eligibility, Knowledge::Unknown);
    assert_eq!(assertions.prior_corporate_actions, Knowledge::Unknown);
    assert!(
        !assertions.basis_is_documented(),
        "налоговая стоимость не становится документированной оттого, \
         что событие старое"
    );
}

#[test]
fn a_documented_basis_is_the_only_thing_that_reports_as_documented() {
    // §10.7: если налоговая стоимость неизвестна, налоговый отчёт
    // возвращает диапазон или not_computable, но не точную цифру.
    // Признак обязан различать три состояния, а не два.
    for (certainty, documented) in [
        (BasisCertainty::Documented, true),
        (BasisCertainty::Estimated, false),
        (BasisCertainty::Unknown, false),
    ] {
        let assertions = OpeningAssertions {
            tax_basis: certainty,
            ..OpeningAssertions::default()
        };
        assert_eq!(
            assertions.basis_is_documented(),
            documented,
            "стоимость {certainty:?} оценена неверно"
        );
    }
}

#[test]
fn assertions_survive_a_round_trip() {
    // Утверждения уходят в журнал и возвращаются оттуда: вариант,
    // не переживающий сериализацию, обнаружится при чтении истории.
    let assertions = OpeningAssertions {
        quantity: Certainty::Known,
        acquisition_date: Some(time::macros::date!(2021 - 06 - 15)),
        acquisition_date_certainty: DateCertainty::Known,
        tax_basis: BasisCertainty::Documented,
        basis_currency: Some(iaam_core::money::CurrencyCode::Usd),
        basis_rate: Some(iaam_core::numeric::decimal::Dec::one()),
        fees_included: Tristate::Yes,
        ldv_eligibility: Knowledge::Known,
        prior_corporate_actions: Knowledge::Known,
    };
    let json = serde_json::to_string(&assertions).expect("сериализация");
    let back: OpeningAssertions = serde_json::from_str(&json).expect("разбор");
    assert_eq!(back, assertions);
}
