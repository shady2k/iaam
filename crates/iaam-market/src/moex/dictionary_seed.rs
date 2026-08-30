//! Initial dictionary of MOEX ISS codes (§2.5).
//!
//! This is **our** knowledge, not the exchange’s: the source lists codes but does not
//! say that `SUR` and `RUB` are one rouble for us, or that `maturity` and
//! `amortization` both return principal. Therefore the table lives in code and
//! is inserted into the database once.
//!
//! It is **not** the source of truth afterwards: the dictionary is edited in the database, and
//! seeding from here does not touch existing rows.
//!
//! Offer-right kinds are listed using the wording observed
//! in the live check on 2026-08-27. Wording is not a code, and the exchange may
//! change it; an unlisted wording yields a refusal, not a silent skip.

/// Triples of “area → source code → domain meaning”.
pub const MOEX_SOURCE_CODES: &[(&str, &str, &str)] = &[
    // One source, two codes for one currency.
    ("currency", "SUR", "RUB"),
    ("currency", "RUB", "RUB"),
    ("currency", "USD", "USD"),
    ("currency", "EUR", "EUR"),
    // Finality is NOT interpreted here: it is derived
    // from the accumulated share total because the source does not always provide
    // 'maturity'—six of 50 checked securities lacked it entirely.
    (
        "principal_repayment_kind",
        "amortization",
        "principal_return",
    ),
    ("principal_repayment_kind", "maturity", "principal_return"),
    ("offer_kind", "Оферта", "put_option"),
    ("offer_kind", "Оферта (состоялось)", "put_option_settled"),
];
