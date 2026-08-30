//! Eight grounds for automatic status promotion (§10.3, table).
//!
//! The test lists the grounds from the spec line by line. The expected levels are taken
//! from the table in §10.3, not from the program output (§15.5).

use std::collections::BTreeSet;

use iaam_core::event::provenance::{ParserVersion, RawHash};
use iaam_core::ids::SourceId;
use iaam_core::reconciliation::evidence::{Evidence, Ground, SourceChannel};
use iaam_core::reconciliation::{ConfidenceLevel, Dimension};

/// Document hash from a human-readable name.
///
/// The name is hex-encoded and padded to sixty-four
/// characters: `RawHash` accepts only a valid SHA-256, while the test needs
/// documents that are distinct and recognizable in debug output, not real hashes.
fn hash(seed: &str) -> RawHash {
    let mut hex: String = seed.bytes().map(|byte| format!("{byte:02x}")).collect();
    assert!(hex.len() <= 64, "document name {seed} is too long");
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
    // An API response has no document: it is a stream, not a file.
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
    // The same broker and the same parser: a shared parsing error will distort both
    // sides identically, and reconciliation will not detect it. This is continuity,
    // not independence.
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
    // A different retrieval channel and different parsing code—the condition in §10.3
    // is met, and independent appears only here.
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
    // The key check from §10.3: the level is determined by channel independence,
    // not by the type of ground. If «API» is parsed using the same code
    // and the same document, there is no independence.
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
        "the same parser and the same document do not provide independence"
    );
}

#[test]
fn a_later_statement_of_the_same_broker_never_reaches_independent() {
    // Exact wording from the spec: «The next report from the same broker,
    // parsed by the same parser, is continuity, not
    // independence». The documents are different, but the parser is the same.
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
                "ground {ground:?} yielded independent with a single parser"
            );
        }
    }
}

#[test]
fn a_reparse_of_the_same_document_by_a_new_parser_is_not_independent() {
    // A new parser version for the same document is a corrected
    // parse, not a second source. There is only one document, and an error in it
    // will go unnoticed by both sides.
    let confirmed = report_channel("tinkoff-xlsx/1", "march");
    let confirming = report_channel("tinkoff-xlsx/2", "march");
    assert!(!confirming.is_independent_of(&confirmed));
}

#[test]
fn independence_ignores_the_source_identifier() {
    // Two sources may share parsing code. Different identifiers
    // with the same parser and the same document do not create independence.
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
    // The depository confirms quantities and the custody location. As for money,
    // it says nothing, and using this to promote the monetary dimension would
    // amount to issuing confirmation that was never provided.
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
    // Independent equations, but one document and one parser.
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
    // A bank statement checked against our projection is independent. The same statement
    // that provided the contract terms is not.
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
    // A separate document and a separate parser are independent, but only for
    // aggregates: the certificate confirms neither the balance nor the quantities.
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
    // §10.4: the balance stated by the owner confirms the snapshot and does not
    // affect the tax basis or income. The level is internal: the owner
    // could have read the same figure in the same report, so independence
    // has not been proven here, while §10.3 requires evidence.
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
    // A basis that confirms nothing is no basis at all. An empty
    // set of dimensions is more dangerous than an error here: it silently adds
    // a row to the list of evidence.
    let channel = report_channel("tinkoff-xlsx/1", "a");
    assert!(
        Evidence::from_match(
            Ground::DepositaryReportConfirms,
            channel.clone(),
            channel,
            dims(&[Dimension::Cash]),
        )
        .is_none(),
        "the depository does not confirm money — no basis"
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
    assert_eq!(
        count, 9,
        "eight bases under §10.3 plus the owner-reported balance"
    );
}

#[test]
fn no_ground_raises_a_dimension_it_cannot_speak_about() {
    // Check all bases: the confirmed dimensions must be
    // a subset of those the basis is entitled to cover. This is a property,
    // not an example, and it catches any retroactive expansion of a basis.
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
        .expect("each basis must confirm at least one dimension");
        assert!(
            evidence.dimensions().is_subset(&ground.dimensions()),
            "basis {ground:?} promoted a dimension it does not cover"
        );
    }
}
