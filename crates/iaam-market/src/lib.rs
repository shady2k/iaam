//! Market data: MOEX ISS and CBR (§12).
//!
//! This crate **describes requests and parses responses**. It knows no HTTP—
//! transport lives in `iaam-http`, enforced by rule 11 of
//! `scripts/check-architecture.sh`. The key property follows: parsing is
//! checked against frozen references **without network access or HTTP mocks**.
//!
//! The crate does not decide which price to apply: it returns every
//! observation supplied by the source. Choosing among them is valuation policy
//! (E3.3).

pub mod cbr;
pub mod error;
pub mod moex;
pub mod observation;
pub mod schedule;

pub use error::MarketError;
pub use observation::{
    AccruedInterestObservation, Executability, FxObservation, KeyRateObservation, ObservedAt,
    PriceKind, PriceObservation, TradeDate, Venue,
};
pub use schedule::completeness::{Completeness, validate_moex_profile};
pub use schedule::terms::{DefaultFlags, IssueTerms};
pub use schedule::{
    CouponAmount, CouponPeriod, Knowledge, OfferWindow, PrincipalRepayment, ScheduleSnapshot,
};
