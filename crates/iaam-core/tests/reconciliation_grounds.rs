//! Восемь оснований автоматического повышения статуса (§10.3, таблица).
//!
//! Тест перечисляет основания по спеке построчно. Ожидаемые уровни взяты
//! из таблицы §10.3, а не из вывода программы (§15.5).

use std::collections::BTreeSet;

use iaam_core::event::provenance::{ParserVersion, RawHash};
use iaam_core::ids::SourceId;
use iaam_core::reconciliation::evidence::{Evidence, Ground, SourceChannel};
use iaam_core::reconciliation::{ConfidenceLevel, Dimension};

/// Хеш документа из читаемого имени.
///
/// Имя кодируется шестнадцатерично и дополняется до шестидесяти четырёх
/// знаков: `RawHash` принимает только корректный SHA-256, а тесту нужны
/// различимые и узнаваемые в отладке документы, а не настоящие хеши.
fn hash(seed: &str) -> RawHash {
    let mut hex: String = seed.bytes().map(|byte| format!("{byte:02x}")).collect();
    assert!(hex.len() <= 64, "имя документа {seed} слишком длинное");
    while hex.len() < 64 {
        hex.push('0');
    }
    RawHash::parse(&hex).unwrap()
}

fn report_channel(parser: &str, document: &str) -> SourceChannel {
    SourceChannel {
        source: SourceId::new_random(),
        parser_version: ParserVersion(parser.to_owned()),
        document: Some(hash(document)),
    }
}

fn api_channel(parser: &str) -> SourceChannel {
    // У ответа API документа нет: это поток, а не файл.
    SourceChannel {
        source: SourceId::new_random(),
        parser_version: ParserVersion(parser.to_owned()),
        document: None,
    }
}

fn dims(list: &[Dimension]) -> BTreeSet<Dimension> {
    list.iter().copied().collect()
}

#[test]
fn ground_one_opening_matches_prior_closing_is_internal_only() {
    // Тот же брокер и тот же парсер: общая ошибка разбора исказит обе
    // стороны одинаково, и сверка её не заметит. Это непрерывность,
    // а не независимость.
    let evidence = Evidence::from_match(
        Ground::OpeningMatchesPriorClosing,
        report_channel("tinkoff-xlsx/1", "b"),
        report_channel("tinkoff-xlsx/1", "a"),
        dims(&[Dimension::Cash, Dimension::Positions]),
    )
    .unwrap();
    assert_eq!(evidence.level(), ConfidenceLevel::AcceptedInternal);
}

#[test]
fn ground_two_continuity_is_internal_only() {
    let evidence = Evidence::from_match(
        Ground::ContinuityBetweenStatements,
        report_channel("finam-xls/1", "b"),
        report_channel("finam-xls/1", "a"),
        dims(&[Dimension::Cash]),
    )
    .unwrap();
    assert_eq!(evidence.level(), ConfidenceLevel::AcceptedInternal);
}

#[test]
fn ground_three_broker_api_against_a_parsed_report_is_independent() {
    // Другой канал получения и другой код разбора — условие §10.3
    // выполнено, и только здесь появляется independent.
    let evidence = Evidence::from_match(
        Ground::BrokerApiAgreesWithStatement,
        api_channel("tinkoff-api/1"),
        report_channel("tinkoff-xlsx/1", "a"),
        dims(&[Dimension::Cash, Dimension::Positions]),
    )
    .unwrap();
    assert_eq!(evidence.level(), ConfidenceLevel::AcceptedIndependent);
}

#[test]
fn ground_three_degrades_to_internal_when_the_channel_is_not_independent() {
    // Ключевая проверка §10.3: уровень определяется независимостью
    // канала, а не типом основания. Если «API» разобран тем же кодом
    // и тем же документом, никакой независимости нет.
    let evidence = Evidence::from_match(
        Ground::BrokerApiAgreesWithStatement,
        report_channel("tinkoff-xlsx/1", "a"),
        report_channel("tinkoff-xlsx/1", "a"),
        dims(&[Dimension::Cash]),
    )
    .unwrap();
    assert_eq!(
        evidence.level(),
        ConfidenceLevel::AcceptedInternal,
        "тот же парсер и тот же документ не дают независимости"
    );
}

#[test]
fn a_later_statement_of_the_same_broker_never_reaches_independent() {
    // Прямая формулировка спеки: «Следующий отчёт того же брокера,
    // разобранный тем же парсером, — это непрерывность, а не
    // независимость». Документы разные, парсер один.
    let confirmed = report_channel("tinkoff-xlsx/3", "march");
    let confirming = report_channel("tinkoff-xlsx/3", "april");
    assert!(!confirming.is_independent_of(&confirmed));

    for ground in Ground::all() {
        let evidence = Evidence::from_match(
            ground,
            confirming.clone(),
            confirmed.clone(),
            dims(&[
                Dimension::Cash,
                Dimension::Positions,
                Dimension::Income,
                Dimension::TaxBasis,
            ]),
        );
        if let Some(evidence) = evidence {
            assert!(
                evidence.level() <= ConfidenceLevel::AcceptedInternal,
                "основание {ground:?} выдало independent на одном парсере"
            );
        }
    }
}

#[test]
fn a_reparse_of_the_same_document_by_a_new_parser_is_not_independent() {
    // Новая версия парсера по тому же документу — это исправленный
    // разбор, а не второй источник. Документ один, и ошибка в нём самом
    // останется незамеченной обеими сторонами.
    let confirmed = report_channel("tinkoff-xlsx/1", "march");
    let confirming = report_channel("tinkoff-xlsx/2", "march");
    assert!(!confirming.is_independent_of(&confirmed));
}

#[test]
fn independence_ignores_the_source_identifier() {
    // Два источника могут делить код разбора. Разные идентификаторы
    // при одном парсере и одном документе независимости не создают.
    let left = SourceChannel {
        source: SourceId::new_random(),
        parser_version: ParserVersion("shared/1".to_owned()),
        document: Some(hash("a")),
    };
    let right = SourceChannel {
        source: SourceId::new_random(),
        parser_version: ParserVersion("shared/1".to_owned()),
        document: Some(hash("a")),
    };
    assert_ne!(left.source, right.source);
    assert!(!left.is_independent_of(&right));
}

#[test]
fn ground_four_depositary_report_raises_positions_only() {
    // Депозитарий подтверждает количества и место хранения. О деньгах
    // он не говорит ничего, и повысить ими денежное измерение значило
    // бы выдать подтверждение, которого не было.
    let evidence = Evidence::from_match(
        Ground::DepositaryReportConfirms,
        report_channel("depositary-pdf/1", "b"),
        report_channel("tinkoff-xlsx/1", "a"),
        dims(&[Dimension::Cash, Dimension::Positions]),
    )
    .unwrap();
    assert_eq!(evidence.level(), ConfidenceLevel::AcceptedIndependent);
    assert_eq!(evidence.dimensions(), dims(&[Dimension::Positions]));
}

#[test]
fn ground_five_separate_sections_agree_is_internal_across_dimensions() {
    // Независимые уравнения, но один документ и один парсер.
    let channel = report_channel("tinkoff-xlsx/1", "march");
    let evidence = Evidence::from_match(
        Ground::SeparateSectionsAgree,
        channel.clone(),
        channel,
        dims(&[Dimension::Cash, Dimension::Positions, Dimension::Income]),
    )
    .unwrap();
    assert_eq!(evidence.level(), ConfidenceLevel::AcceptedInternal);
    assert_eq!(
        evidence.dimensions(),
        dims(&[Dimension::Cash, Dimension::Positions, Dimension::Income])
    );
}

#[test]
fn ground_six_payout_is_independent_only_through_another_channel() {
    // Выписка банка против нашей проекции — независимо. Та же выписка,
    // что дала условия договора, — нет.
    let terms = report_channel("bank-statement/1", "contract");
    let independent = Evidence::from_match(
        Ground::PayoutConfirmsSchedule,
        report_channel("bank-api/1", "statement"),
        terms.clone(),
        dims(&[Dimension::Income]),
    )
    .unwrap();
    assert_eq!(independent.level(), ConfidenceLevel::AcceptedIndependent);

    let same = Evidence::from_match(
        Ground::PayoutConfirmsSchedule,
        terms.clone(),
        terms,
        dims(&[Dimension::Income]),
    )
    .unwrap();
    assert_eq!(same.level(), ConfidenceLevel::AcceptedInternal);
}

#[test]
fn ground_seven_corporate_action_terms_raise_positions() {
    let evidence = Evidence::from_match(
        Ground::CorporateActionMatchesIssueTerms,
        api_channel("moex-iss/1"),
        report_channel("tinkoff-xlsx/1", "a"),
        dims(&[Dimension::Positions]),
    )
    .unwrap();
    assert_eq!(evidence.level(), ConfidenceLevel::AcceptedIndependent);
    assert_eq!(evidence.dimensions(), dims(&[Dimension::Positions]));
}

#[test]
fn ground_eight_tax_certificate_raises_income_and_tax_basis_only() {
    // Отдельный документ, отдельный парсер — independent, но только по
    // агрегатам: справка не подтверждает ни остаток, ни количества.
    let evidence = Evidence::from_match(
        Ground::TaxAgentCertificate,
        report_channel("tax-certificate/1", "b"),
        report_channel("tinkoff-xlsx/1", "a"),
        dims(&[Dimension::Cash, Dimension::Income, Dimension::TaxBasis]),
    )
    .unwrap();
    assert_eq!(evidence.level(), ConfidenceLevel::AcceptedIndependent);
    assert_eq!(
        evidence.dimensions(),
        dims(&[Dimension::Income, Dimension::TaxBasis])
    );
}

#[test]
fn an_owner_stated_balance_is_internal_and_touches_cash_and_positions_only() {
    // §10.4: названный владельцем остаток подтверждает снимок и не
    // трогает налоговую стоимость и доходы. Уровень — internal: владелец
    // мог прочитать ту же цифру в том же отчёте, и независимость здесь
    // не доказана, а §10.3 требует доказательства.
    let owner = SourceChannel {
        source: SourceId::new_random(),
        parser_version: ParserVersion("owner/1".to_owned()),
        document: None,
    };
    let evidence = Evidence::from_match(
        Ground::OwnerStatedBalance,
        owner,
        report_channel("tinkoff-xlsx/1", "a"),
        dims(&[
            Dimension::Cash,
            Dimension::Positions,
            Dimension::TaxBasis,
            Dimension::Income,
        ]),
    )
    .unwrap();
    assert_eq!(evidence.level(), ConfidenceLevel::AcceptedInternal);
    assert_eq!(
        evidence.dimensions(),
        dims(&[Dimension::Cash, Dimension::Positions])
    );
}

#[test]
fn evidence_without_any_confirmed_dimension_does_not_exist() {
    // Основание, ничего не подтверждающее, — это не основание. Пустое
    // множество измерений здесь опаснее ошибки: оно молча добавляет
    // строку в список доказательств.
    let channel = report_channel("tinkoff-xlsx/1", "a");
    assert!(
        Evidence::from_match(
            Ground::DepositaryReportConfirms,
            channel.clone(),
            channel,
            dims(&[Dimension::Cash]),
        )
        .is_none(),
        "депозитарий не подтверждает деньги — основания нет"
    );
}

#[test]
fn every_ground_has_a_distinct_machine_readable_code() {
    let grounds = Ground::all();
    let mut codes: Vec<&str> = grounds.iter().map(|g| g.code()).collect();
    let count = codes.len();
    codes.sort_unstable();
    codes.dedup();
    assert_eq!(codes.len(), count);
    assert_eq!(count, 9, "восемь оснований §10.3 плюс остаток от владельца");
}

#[test]
fn no_ground_raises_a_dimension_it_cannot_speak_about() {
    // Обход по всем основаниям: подтверждённые измерения обязаны быть
    // подмножеством того, о чём основание вправе говорить. Это свойство,
    // а не пример, и оно ловит расширение любого основания задним числом.
    let confirming = api_channel("other/1");
    let confirmed = report_channel("report/1", "a");
    let everything = dims(&[
        Dimension::Cash,
        Dimension::Positions,
        Dimension::Income,
        Dimension::TaxBasis,
    ]);
    for ground in Ground::all() {
        let evidence = Evidence::from_match(
            ground,
            confirming.clone(),
            confirmed.clone(),
            everything.clone(),
        )
        .expect("хотя бы одно измерение подтверждается каждым основанием");
        assert!(
            evidence.dimensions().is_subset(&ground.dimensions()),
            "основание {ground:?} повысило измерение, о котором не говорит"
        );
    }
}
