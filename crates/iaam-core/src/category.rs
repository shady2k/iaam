//! Owner categories: what the money went to (spec §3).
//!
//! **A category is not a field on an event and never becomes one.** It is
//! derived here from versioned rules over the row's attributes and its date, so
//! renaming, splitting, merging and retiring categories touch reference data
//! only. Had the category been written onto the event, every reorganisation
//! would demand a journal migration and the owner would stop reorganising —
//! which is how a category list ossifies into one nobody opens.
//!
//! This is a different question from `iaam_ingest::classification`, which
//! answers "what kind of operation this is". A rule reading "Corner Shop →
//! Продукты" says nothing about whether a row is a fee or a withdrawal, and the
//! two must not share a type.
//!
//! **Every rule is valid over an interval.** A merchant that sold pies in 2024
//! and umbrellas in 2026 is one string with two meanings; a rule claiming to
//! hold forever misclassifies half of history. Instrument aliases already solve
//! exactly this, with the reasoning at `crates/iaam-ingest/src/csv_source.rs:47`.

use time::Date;

use crate::ids::{CategoryId, CategoryRuleId};

/// The inclusive validity interval of a category rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CategoryInterval {
    pub from: Option<Date>,
    pub to: Option<Date>,
}

impl CategoryInterval {
    /// Whether this interval covers the row date.
    pub fn covers(&self, on: Date) -> bool {
        self.from.is_none_or(|from| from <= on) && self.to.is_none_or(|to| on <= to)
    }
}

/// Row attributes used to derive an owner's category.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CategorySubject<'a> {
    pub row_key: Option<&'a str>,
    pub source_category: Option<&'a str>,
    pub counterparty: Option<&'a str>,
    pub description: Option<&'a str>,
    pub on: Date,
}

/// Why a rule assigned the category.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CategoryBasis {
    Row { rule: CategoryRuleId },
    SourceCategory { rule: CategoryRuleId },
    Description { rule: CategoryRuleId },
}

/// A category rule matcher.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CategoryMatcher {
    /// One specific row, by its stable key. The owner's hand-made decision.
    Row { key: String },
    /// The source's own category value, matched exactly.
    SourceCategory { value: String },
    /// A case-insensitive substring of the counterparty or description.
    DescriptionContains { text: String },
}

impl CategoryMatcher {
    fn matches(&self, subject: &CategorySubject<'_>) -> bool {
        match self {
            Self::Row { key } => !key.is_empty() && subject.row_key == Some(key.as_str()),
            Self::SourceCategory { value } => {
                !value.is_empty() && subject.source_category == Some(value.as_str())
            }
            Self::DescriptionContains { text } => {
                if text.is_empty() {
                    return false;
                }
                let wanted = text.to_lowercase();
                subject
                    .counterparty
                    .into_iter()
                    .chain(subject.description)
                    .any(|candidate| candidate.to_lowercase().contains(&wanted))
            }
        }
    }
}

/// A versioned rule assigning one owner category.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CategoryRule {
    pub id: CategoryRuleId,
    pub version: u32,
    pub interval: CategoryInterval,
    pub matcher: CategoryMatcher,
    pub category: CategoryId,
}

/// The result of deriving an owner's category for a row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CategoryAssignment {
    Assigned {
        category: CategoryId,
        basis: CategoryBasis,
    },
    /// No rule covers this row on this date. Never a silent bucket.
    NotDecomposed,
}

/// Assign a category using explicit precedence and the latest matching version.
pub fn assign(subject: &CategorySubject<'_>, rules: &[CategoryRule]) -> CategoryAssignment {
    let row_rule = rules
        .iter()
        .filter(|rule| rule.interval.covers(subject.on))
        .filter(|rule| matches!(rule.matcher, CategoryMatcher::Row { .. }))
        .filter(|rule| rule.matcher.matches(subject))
        .max_by_key(|rule| rule.version);
    if let Some(rule) = row_rule {
        return CategoryAssignment::Assigned {
            category: rule.category,
            basis: CategoryBasis::Row { rule: rule.id },
        };
    }

    let source_rule = rules
        .iter()
        .filter(|rule| rule.interval.covers(subject.on))
        .filter(|rule| matches!(rule.matcher, CategoryMatcher::SourceCategory { .. }))
        .filter(|rule| rule.matcher.matches(subject))
        .max_by_key(|rule| rule.version);
    if let Some(rule) = source_rule {
        return CategoryAssignment::Assigned {
            category: rule.category,
            basis: CategoryBasis::SourceCategory { rule: rule.id },
        };
    }

    let description_rule = rules
        .iter()
        .filter(|rule| rule.interval.covers(subject.on))
        .filter(|rule| matches!(rule.matcher, CategoryMatcher::DescriptionContains { .. }))
        .filter(|rule| rule.matcher.matches(subject))
        .max_by_key(|rule| rule.version);
    if let Some(rule) = description_rule {
        return CategoryAssignment::Assigned {
            category: rule.category,
            basis: CategoryBasis::Description { rule: rule.id },
        };
    }

    CategoryAssignment::NotDecomposed
}

#[cfg(test)]
mod tests {
    use time::{Date, macros::date};

    use crate::ids::{CategoryId, CategoryRuleId};

    use super::{
        CategoryAssignment, CategoryBasis, CategoryInterval, CategoryMatcher, CategoryRule,
        CategorySubject, assign,
    };

    fn rule(
        id: u128,
        version: u32,
        matcher: CategoryMatcher,
        from: Option<Date>,
        to: Option<Date>,
        category: u128,
    ) -> CategoryRule {
        CategoryRule {
            id: CategoryRuleId(uuid::Uuid::from_u128(id)),
            version,
            interval: CategoryInterval { from, to },
            matcher,
            category: CategoryId(uuid::Uuid::from_u128(category)),
        }
    }

    #[test]
    fn a_merchant_that_changed_its_trade_is_not_miscategorised_backwards() {
        // The shop sold pies until 2025 and umbrellas after. One string, two
        // meanings — the same problem instrument aliases solve with an interval
        // (crates/iaam-ingest/src/csv_source.rs:47).
        let pies = 10;
        let umbrellas = 20;
        let rules = vec![
            rule(
                1,
                1,
                CategoryMatcher::DescriptionContains {
                    text: "ЛАВКА".into(),
                },
                None,
                Some(date!(2025 - 12 - 31)),
                pies,
            ),
            rule(
                2,
                2,
                CategoryMatcher::DescriptionContains {
                    text: "ЛАВКА".into(),
                },
                Some(date!(2026 - 01 - 01)),
                None,
                umbrellas,
            ),
        ];
        let subject = |on| CategorySubject {
            row_key: None,
            source_category: None,
            counterparty: None,
            description: Some("Лавка на углу"),
            on,
        };

        assert!(matches!(
            assign(&subject(date!(2024 - 06 - 01)), &rules),
            CategoryAssignment::Assigned { category, .. }
                if category == CategoryId(uuid::Uuid::from_u128(pies))
        ));
        assert!(matches!(
            assign(&subject(date!(2026 - 08 - 01)), &rules),
            CategoryAssignment::Assigned { category, .. }
                if category == CategoryId(uuid::Uuid::from_u128(umbrellas))
        ));
    }

    #[test]
    fn a_hand_made_row_decision_outranks_a_later_blanket_rule() {
        // R12: the owner said what this particular purchase was. A rule written
        // afterwards about the same merchant must not overwrite that.
        let by_hand = 10;
        let blanket = 20;
        let rules = vec![
            rule(
                1,
                1,
                CategoryMatcher::Row {
                    key: "row-7".into(),
                },
                None,
                None,
                by_hand,
            ),
            rule(
                2,
                9,
                CategoryMatcher::DescriptionContains {
                    text: "ЛАВКА".into(),
                },
                None,
                None,
                blanket,
            ),
        ];
        let subject = CategorySubject {
            row_key: Some("row-7"),
            source_category: None,
            counterparty: None,
            description: Some("Лавка на углу"),
            on: date!(2026 - 08 - 01),
        };
        assert!(matches!(
            assign(&subject, &rules),
            CategoryAssignment::Assigned {
                basis: CategoryBasis::Row { .. },
                category,
            } if category == CategoryId(uuid::Uuid::from_u128(by_hand))
        ));
    }

    #[test]
    fn the_sources_category_outranks_a_description_rule() {
        let from_source = 10;
        let from_text = 20;
        let rules = vec![
            rule(
                1,
                1,
                CategoryMatcher::SourceCategory {
                    value: "Супермаркеты".into(),
                },
                None,
                None,
                from_source,
            ),
            rule(
                2,
                5,
                CategoryMatcher::DescriptionContains {
                    text: "ЛАВКА".into(),
                },
                None,
                None,
                from_text,
            ),
        ];
        let subject = CategorySubject {
            row_key: None,
            source_category: Some("Супермаркеты"),
            counterparty: None,
            description: Some("Лавка на углу"),
            on: date!(2026 - 08 - 01),
        };
        assert!(matches!(
            assign(&subject, &rules),
            CategoryAssignment::Assigned {
                basis: CategoryBasis::SourceCategory { .. },
                category,
            } if category == CategoryId(uuid::Uuid::from_u128(from_source))
        ));
    }

    #[test]
    fn a_row_no_rule_covers_is_not_decomposed_rather_than_bucketed() {
        let rules = vec![];
        let subject = CategorySubject {
            row_key: None,
            source_category: Some("Супермаркеты"),
            counterparty: None,
            description: Some("Лавка на углу"),
            on: date!(2026 - 08 - 01),
        };
        // No "Other". A silent catch-all is how a decomposition stops meaning
        // anything, which is exactly the state Actual Budget's categories are in.
        assert!(matches!(
            assign(&subject, &rules),
            CategoryAssignment::NotDecomposed
        ));
    }

    #[test]
    fn an_empty_matcher_matches_nothing() {
        let rules = vec![rule(
            1,
            1,
            CategoryMatcher::DescriptionContains {
                text: String::new(),
            },
            None,
            None,
            10,
        )];
        let subject = CategorySubject {
            row_key: None,
            source_category: None,
            counterparty: None,
            description: Some("Лавка на углу"),
            on: date!(2026 - 08 - 01),
        };
        assert!(matches!(
            assign(&subject, &rules),
            CategoryAssignment::NotDecomposed
        ));
    }

    #[test]
    fn among_rules_of_one_kind_the_highest_version_wins() {
        let old = 10;
        let new = 20;
        let rules = vec![
            rule(
                1,
                1,
                CategoryMatcher::SourceCategory {
                    value: "Супермаркеты".into(),
                },
                None,
                None,
                old,
            ),
            rule(
                2,
                2,
                CategoryMatcher::SourceCategory {
                    value: "Супермаркеты".into(),
                },
                None,
                None,
                new,
            ),
        ];
        let subject = CategorySubject {
            row_key: None,
            source_category: Some("Супермаркеты"),
            counterparty: None,
            description: Some(""),
            on: date!(2026 - 08 - 01),
        };
        assert!(matches!(
            assign(&subject, &rules),
            CategoryAssignment::Assigned { category, .. }
                if category == CategoryId(uuid::Uuid::from_u128(new))
        ));
    }
}
