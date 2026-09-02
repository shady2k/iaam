//! Fixed vocabularies published in the contract (§13, §17.1).
//!
//! A code that arrives as a bare string tells the reader nothing: to learn what
//! `possible_duplicate` or `missing_fx_rate` means, a client has to find a
//! document that lists them, and a hand-written list drifts away from the code
//! that produces it. So the vocabularies here are **expanded from the domain**:
//! `iaam_app::ingest::verdict_vocabulary!`,
//! `iaam_core::not_computable_vocabulary!`,
//! `iaam_core::negative_cash_classification_vocabulary!` and
//! `iaam_core::data_quality_status_vocabulary!` each call a macro in this
//! module with every variant, its wire code and the sentence explaining it, and
//! that one list produces the Rust type, the conversion from the domain value
//! and the OpenAPI schema alike.
//!
//! Two consequences are the point of the arrangement. A variant added to the
//! domain enum without an entry in the vocabulary fails to compile, because the
//! generated conversion matches the domain type exhaustively. And a code cannot
//! reach the wire without its meaning reaching the contract, because neither is
//! written twice.
//!
//! The wire form is unchanged: every code serialises to exactly the string it
//! did when the field was typed as `String`.

use serde::{Deserialize, Serialize};
use utoipa::PartialSchema;
use utoipa::ToSchema;
use utoipa::openapi::RefOr;
use utoipa::openapi::schema::{ObjectBuilder, OneOfBuilder, Schema, SchemaType, Type};

use iaam_app::ingest::Verdict;
use iaam_core::perimeter::NegativeCashClassification;
use iaam_core::returns::{DataQualityStatus, NotComputable};

/// A schema that both enumerates a vocabulary and explains it.
///
/// `oneOf` over single-valued `enum`s rather than one `enum` with every code:
/// a plain enumeration has room for one description for the whole field, and
/// the meaning of an individual code has nowhere to go. Here each code carries
/// its own sentence, and validation is unchanged — the value is still one of
/// the listed strings.
///
/// Visible to the crate because two vocabularies are not expanded from a domain
/// macro: `AliasNamespaceDto` and `BalancePointDto` are converted in both
/// directions and are written out in `dto.rs`, and they publish themselves
/// through this function rather than through a second mechanism of their own.
pub(crate) fn described_vocabulary(description: &str, codes: &[(&str, &str)]) -> RefOr<Schema> {
    codes
        .iter()
        .fold(
            OneOfBuilder::new().description(Some(description)),
            |schema, (code, meaning)| {
                schema.item(
                    ObjectBuilder::new()
                        .schema_type(SchemaType::Type(Type::String))
                        .enum_values(Some([*code]))
                        .description(Some(*meaning)),
                )
            },
        )
        .into()
}

/// Expands one vocabulary into a wire type: the enum, the conversion from the
/// domain value, and the schema that lists and explains every code.
macro_rules! vocabulary_enum {
    (
        $(#[$attribute:meta])*
        $name:ident from $domain:ident { $description:literal }
        $($variant:ident => $code:literal : $meaning:literal),+ $(,)?
    ) => {
        $(#[$attribute])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
        pub enum $name {
            $(
                #[serde(rename = $code)]
                $variant,
            )+
        }

        impl $name {
            /// The code as it appears on the wire.
            #[must_use]
            pub const fn code(self) -> &'static str {
                match self {
                    $(Self::$variant => $code,)+
                }
            }

            /// Every code with the sentence that explains it, in declaration order.
            const VOCABULARY: &'static [(&'static str, &'static str)] =
                &[$(($code, $meaning)),+];

            /// Exhaustive over the domain enum: a new variant there stops the
            /// build until it is given a code and a meaning.
            #[must_use]
            pub const fn from_domain(value: &$domain) -> Self {
                match value {
                    $($domain::$variant { .. } => Self::$variant,)+
                }
            }
        }

        impl PartialSchema for $name {
            fn schema() -> RefOr<Schema> {
                described_vocabulary($description, Self::VOCABULARY)
            }
        }

        impl ToSchema for $name {}
    };
}

macro_rules! define_verdict_code_dto {
    ($($variant:ident => $code:literal : $meaning:literal),+ $(,)?) => {
        vocabulary_enum! {
            /// The verdict on one submitted row (§10.4).
            ///
            /// Every code says first whether the fact reached the journal, and
            /// only then why: that distinction matters more than the code
            /// itself, and the reasoning behind it is on `iaam_ingest::Verdict`.
            VerdictCodeDto from Verdict {
                "The verdict on one submitted row. Each code states whether the fact was recorded in the journal and, if it was not, why. A code that is unfamiliar is to be read here, not guessed at."
            }
            $($variant => $code : $meaning),+
        }
    };
}

iaam_app::ingest::verdict_vocabulary!(define_verdict_code_dto);

macro_rules! define_not_computable_code_dto {
    ($($variant:ident => $code:literal : $meaning:literal),+ $(,)?) => {
        vocabulary_enum! {
            /// Why a value was not computed (§13).
            ///
            /// A refusal is an answer, and it is never recomputed by the
            /// caller. Two of the codes — `state_newer_than_report` and
            /// `numeric` — are defect reports rather than gaps in the owner's
            /// data; see `iaam_core::returns::NotComputable`.
            NotComputableCodeDto from NotComputable {
                "Why the value was not computed. A refusal is an answer: pass it on, do not recompute it. The code names the reason; the neighbouring `detail` field names the instrument, currency or date it happened to. Everything here except `state_newer_than_report` and `numeric` is something the owner can act on; those two are defects in the system and are reported as such."
            }
            $($variant => $code : $meaning),+
        }
    };
}

iaam_core::not_computable_vocabulary!(define_not_computable_code_dto);

macro_rules! define_negative_cash_classification_dto {
    ($($variant:ident => $code:literal : $meaning:literal),+ $(,)?) => {
        vocabulary_enum! {
            /// Why a cash balance is negative, and therefore whether §11 lets
            /// the period's reports be calculated for the account (§11).
            NegativeCashClassificationDto from NegativeCashClassification {
                "How the negative cash balance is classified under §11. Only `temporary_settlement_deficit` leaves the period's tax and financial reports calculable for the account; the other two refuse them for that account and for no other. None of the three is a reason to hide the figure: a negative balance is stated by the answer either way."
            }
            $($variant => $code : $meaning),+
        }
    };
}

iaam_core::negative_cash_classification_vocabulary!(define_negative_cash_classification_dto);

macro_rules! define_data_quality_status_dto {
    ($($variant:ident => $code:literal : $meaning:literal),+ $(,)?) => {
        vocabulary_enum! {
            /// How well confirmed the answer is (§10.5).
            DataQualityStatusDto from DataQualityStatus {
                "How well confirmed the answer is. The status is not a share and not a defect count: it is replaced neither by a coverage figure of one's own nor by calling the provisional share an error."
            }
            $($variant => $code : $meaning),+
        }
    };
}

iaam_core::data_quality_status_vocabulary!(define_data_quality_status_dto);

#[cfg(test)]
mod tests {
    use super::*;

    use iaam_core::reconciliation::claim::BalancePoint;

    #[test]
    fn a_code_serialises_to_the_string_it_replaced() {
        // The field used to be a `String` holding `Verdict::code()`. Typing it
        // was not permission to change what a client reads.
        for verdict in [
            VerdictCodeDto::PossibleDuplicate,
            VerdictCodeDto::NeedsReconciliation,
            VerdictCodeDto::Quarantined,
        ] {
            assert_eq!(
                serde_json::to_value(verdict).expect("serialisation"),
                serde_json::Value::String(verdict.code().to_owned())
            );
        }
        assert_eq!(
            serde_json::to_value(NotComputableCodeDto::MissingFxRate).expect("serialisation"),
            serde_json::json!("missing_fx_rate")
        );
        assert_eq!(
            serde_json::to_value(DataQualityStatusDto::Incomplete).expect("serialisation"),
            serde_json::json!("incomplete")
        );
    }

    #[test]
    fn a_namespace_code_serialises_as_it_did_before_it_was_explained() {
        // `AliasNamespaceDto` gained a `oneOf` schema so that each register
        // could carry its meaning. The schema is what a client reads; the five
        // strings are what it sends, and those had to stay exactly as they were.
        for namespace in iaam_core::instrument::AliasNamespace::ALL {
            let dto = crate::dto::AliasNamespaceDto::from_domain(namespace);
            assert_eq!(
                serde_json::to_value(dto).expect("serialisation"),
                serde_json::Value::String(namespace.code().to_owned())
            );
            assert_eq!(dto.code(), namespace.code());
            assert_eq!(
                serde_json::from_value::<crate::dto::AliasNamespaceDto>(serde_json::Value::String(
                    namespace.code().to_owned()
                ))
                .expect("deserialisation"),
                dto,
                "a code the route accepted is no longer read back"
            );
        }
    }

    #[test]
    fn a_balance_point_serialises_as_it_did_when_it_was_a_bare_string() {
        // The field arrived as `pub at: String` and the handler compared it
        // against two literals. Typing it enumerates those two literals in the
        // contract; it does not license changing either of them, and the action
        // queue presets the field with `BalancePoint::code()` at both points.
        for point in [BalancePoint::Opening, BalancePoint::Closing] {
            let dto = crate::dto::BalancePointDto::from_domain(point);
            assert_eq!(
                serde_json::to_value(dto).expect("serialisation"),
                serde_json::Value::String(point.code().to_owned())
            );
            assert_eq!(dto.code(), point.code());
            assert_eq!(dto.to_domain(), point);
            assert_eq!(
                serde_json::from_value::<crate::dto::BalancePointDto>(serde_json::Value::String(
                    point.code().to_owned()
                ))
                .expect("deserialisation"),
                dto,
                "a code the action queue presets is no longer read back"
            );
        }
    }

    #[test]
    fn the_wire_code_is_the_domain_code() {
        // Two lists would be one list too many: the domain decides the code,
        // and the transport must not be able to disagree with it.
        let quarantined = Verdict::Quarantined {
            reason: "unreadable row".to_owned(),
        };
        assert_eq!(
            VerdictCodeDto::from_domain(&quarantined).code(),
            quarantined.code()
        );

        let refusal = NotComputable::NoExternalFlows;
        assert_eq!(
            NotComputableCodeDto::from_domain(&refusal).code(),
            refusal.code()
        );

        let status = DataQualityStatus::Mixed;
        assert_eq!(
            DataQualityStatusDto::from_domain(&status).code(),
            status.code()
        );

        for classification in [
            NegativeCashClassification::TemporarySettlementDeficit,
            NegativeCashClassification::UnsupportedMarginLiability,
            NegativeCashClassification::UnclassifiedNegativeCash,
        ] {
            assert_eq!(
                NegativeCashClassificationDto::from_domain(&classification).code(),
                classification.code()
            );
            assert_eq!(
                serde_json::to_value(NegativeCashClassificationDto::from_domain(&classification))
                    .expect("serialisation"),
                serde_json::Value::String(classification.code().to_owned())
            );
        }
    }
}
