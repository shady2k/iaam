//! Reading back what ingest recorded.
//!
//! Every other read this API serves is an aggregate computed over the journal:
//! balances, money flow, returns, reconciliation. None of them can answer "is
//! this the row I submitted, and did it land on the account I meant". This
//! scenario answers exactly that, and nothing else — it computes no total and
//! derives no figure, so an agent forbidden its own arithmetic has something to
//! quote (§13).
//!
//! What it serves are **journal events**, not the operations that were posted.
//! Ingest normalises an operation into an event and keeps the event; the
//! operation as submitted is not stored, so it cannot be handed back. The two
//! differ in ways a caller notices — a deposit becomes a `cash_in` event with
//! one cash leg — and the route says so in its own description rather than
//! leaving an agent to discover it from the field names.

use iaam_core::dates::EventDates;
use iaam_core::event::leg::Leg;
use iaam_core::event::provenance::RuleSettlement;
use iaam_core::event::{Confidence, Relation};
use iaam_core::ids::{
    AccountId, ClassificationRuleId, EventId, ImportId, ImportSessionId, OwnerId, SourceId,
};
use time::{Date, Time};

use crate::error::AppError;
use crate::ports::{JournalCursor, JournalQuery, Store};

/// Rows returned when the caller names no size.
pub const DEFAULT_PAGE_SIZE: u32 = 50;

/// The largest page the route will assemble.
///
/// A ceiling rather than a silent clamp: a caller that asked for a thousand
/// rows and received two hundred cannot tell a truncated page from the end of
/// the journal, and would stop reading one page early.
pub const MAX_PAGE_SIZE: u32 = 200;

/// The source a caller declared when it submitted, named the same way.
///
/// A source identity is derived from the owner, the account and the channel;
/// the derived identifier itself is never handed out, so the only way a caller
/// can ask "what did that import put in" is to repeat what it declared.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeclaredSource {
    pub account: AccountId,
    pub channel: String,
}

/// One read of the journal. Every filter is optional and they combine.
#[derive(Debug, Clone, Default)]
pub struct JournalReadQuery {
    /// The client key supplied at ingest. It addresses at most one event.
    pub idempotency_key: Option<String>,
    pub account: Option<AccountId>,
    pub source: Option<DeclaredSource>,
    /// The import session whose commit wrote these rows.
    ///
    /// The finest handle this route offers, and the only one that names an
    /// **act**. The declared source names a channel and answers «everything
    /// that ever arrived this way»; this answers «what that one import put in»,
    /// which is the question an owner asks when a figure surprises him — and
    /// the session identifier is one he already holds, because
    /// `POST /v1/import-sessions` handed it to him and every row returned here
    /// carries it back.
    pub import_session: Option<ImportSessionId>,
    /// The standing classification rule that filed the rows.
    ///
    /// The handle the owner's own review needs. He makes one decision, a rule is
    /// written from it, and the rule then files a group of rows automatically;
    /// when one of them turns out wrong, finding it means seeing the group — and
    /// the group is defined by the rule. Every other filter here narrows by
    /// where a row came from or when it happened, and none of them can assemble
    /// that group.
    ///
    /// It composes with the rest rather than replacing them: «what this rule did
    /// in March, on that account» is one query.
    pub settled_by_rule: Option<ClassificationRuleId>,
    /// One version of that rule, where the caller wants only its rows.
    ///
    /// A rule can be edited, so «the rows rule R filed» and «the rows version 3
    /// of R filed» are different questions, and after an edit the second is the
    /// one asked. Supplied together with the rule; a version on its own names
    /// nothing and is refused rather than ignored.
    pub settled_by_rule_version: Option<u32>,
    /// Inclusive lower bound on the effective date.
    pub from: Option<Date>,
    /// Inclusive upper bound on the effective date.
    pub to: Option<Date>,
    /// Opaque position from a previous page's `next`.
    pub after: Option<String>,
    /// Rows per page. Absent means [`DEFAULT_PAGE_SIZE`].
    pub limit: Option<u32>,
}

/// One page of the journal together with where to resume.
#[derive(Debug, Clone, PartialEq)]
pub struct JournalPage {
    pub rows: Vec<JournalEventView>,
    /// Position to pass back as `after`. `None` means this page is the last
    /// one; an empty string would be indistinguishable from "resume at the
    /// beginning", which would loop forever.
    pub next: Option<String>,
}

/// One recorded event, as much of it as answers who, when, what and where from.
///
/// Deliberately absent: the raw-row hash, the parser version and the row
/// locator. They answer "which line of which document produced this", which is
/// a different question from the one this route exists for, and they are the
/// part of provenance a caller cannot act on.
#[derive(Debug, Clone, PartialEq)]
pub struct JournalEventView {
    pub event: EventId,
    pub account: AccountId,
    /// The date the journal orders this event by.
    pub effective_date: Date,
    /// Order within the day. With `effective_date` it names the row uniquely.
    pub sequence: u32,
    /// Time of day the source stated, when it stated one.
    pub source_time: Option<Time>,
    /// Event family: `cash_in`, `trade`, `income` and so on.
    pub kind: &'static str,
    /// The semantic dates the fact carries. They are not the ordering date and
    /// a caller comparing against a statement wants these.
    pub dates: EventDates,
    /// The movement, leg by leg, exactly as recorded. No leg is added up here:
    /// a total is a computed number and this route computes none.
    pub legs: Vec<Leg>,
    /// Whether this event reverses or replaces another. A reader who cannot see
    /// that an event was reversed reads a retracted fact as a live one.
    pub relation: Relation,
    pub confidence: Confidence,
    pub idempotency_key: Option<String>,
    pub source: SourceId,
    /// The import this fact arrived in, when the submission named one.
    ///
    /// Published beside the source because it is the narrower of the two and
    /// the one a retraction actually takes: `POST /v1/corrections/imports`
    /// decides on the import when a label was declared and on the source only
    /// when none was. A caller shown the source alone can see which channel of
    /// which account a fact came through and still cannot tell whether taking
    /// it back would take one statement or every statement ever imported that
    /// way.
    ///
    /// `None` means the submission named no import — rows from a channel that
    /// declares no source at all, and rows recorded before imports could be
    /// named. They are retracted as the one unnamed group they are, and a
    /// reader must not read the absence as «this import has no name yet».
    pub import: Option<ImportId>,
    pub source_operation_id: Option<String>,
    /// The category the source itself put on the row. Evidence, never a verdict.
    pub source_category: Option<String>,
    /// The source's own word for what the operation was. Evidence, never a
    /// verdict, and a different fact from the category beside it: this says
    /// what happened, that says what the money was for.
    ///
    /// Published because it is what a classification rule matches, and a field
    /// a rule fires on that no response ever shows is a rule the owner cannot
    /// check. `None` for a fact recorded before schema version 14 — including
    /// one written through the observation path, whose operation word is in
    /// `source_category`. Nothing rewrites those.
    pub source_kind: Option<String>,
    /// The description or counterparty the source printed on the row.
    pub description: Option<String>,
    /// The import session this fact was committed out of, when one is recorded.
    ///
    /// Published where the raw hash and the parser version are not, and the
    /// difference is what a caller can do with it: those name a line of a
    /// document nobody here can open, while this is an identifier that
    /// addresses `GET /v1/import-sessions/{session}` and its assessment — the
    /// rows that were held, the questions that were answered, the control
    /// figures the source printed. It is the step from «this is the fact» to
    /// «this is the act that admitted it».
    ///
    /// `None` covers both a fact that came through no session and one committed
    /// before the field existed, and a reader must not resolve it either way.
    pub import_session: Option<ImportSessionId>,
    /// What the owner's standing rules made of this row, when a reading said.
    ///
    /// Three states and not two, and the third is the one he must be able to
    /// see. A rule names itself and its version — that is the group one decision
    /// of his reached. `no_rule` says a reading ran and none of his rules
    /// matched, so the row was settled some other way: he answered it, his
    /// account directory recognised the far side, the source asserted it, or the
    /// caller submitted a finished operation. Absence says nothing was recorded
    /// at all — every fact written before this field existed, and every route
    /// that writes without reading a row against the rules.
    ///
    /// Absence is therefore never «no rule filed this». Reading it that way
    /// tells him a row one of his rules did file was decided by hand.
    pub rule_settlement: Option<RuleSettlement>,
}

/// Read one page of the owner's journal.
///
/// An idempotency key is an address, not a filter: it is unique per owner by
/// database index, so a key that matches nothing is a missing resource rather
/// than an empty answer, and is reported as one. Every other filter narrows a
/// listing, and a listing that matches nothing is legitimately empty.
pub async fn read_journal(
    store: &dyn Store,
    owner: OwnerId,
    query: JournalReadQuery,
) -> Result<JournalPage, AppError> {
    let limit = page_size(query.limit)?;
    let range = date_range(query.from, query.to)?;
    let after = query.after.as_deref().map(parse_cursor).transpose()?;
    let source = query
        .source
        .as_ref()
        .map(|declared| declared_source(owner, declared))
        .transpose()?;
    let (settled_by_rule, settled_by_rule_version) =
        rule_filter(query.settled_by_rule, query.settled_by_rule_version)?;

    // One row beyond the page: the difference between "there is more" and "that
    // was everything" cannot be inferred from a full page, and a caller that
    // guesses wrong either stops early or asks for a page that is never there.
    let events = store
        .list_journal_events(
            owner,
            JournalQuery {
                event: None,
                idempotency_key: query.idempotency_key.clone(),
                account: query.account,
                source,
                import_session: query.import_session,
                settled_by_rule,
                settled_by_rule_version,
                from: range.0,
                to: range.1,
                after,
                limit: limit.saturating_add(1),
            },
        )
        .await?;

    if events.is_empty() {
        if let Some(key) = query.idempotency_key {
            return Err(AppError::NotFound {
                what: "journal event by idempotency key",
                id: key,
            });
        }
    }

    let has_more = events.len() > limit as usize;
    let rows: Vec<JournalEventView> = events
        .iter()
        .take(limit as usize)
        .map(journal_event_view)
        .collect();
    let next = has_more
        .then(|| {
            rows.last()
                .map(|row| format_cursor(row.effective_date, row.sequence))
        })
        .flatten();
    Ok(JournalPage { rows, next })
}

fn journal_event_view(event: &iaam_core::event::Event) -> JournalEventView {
    JournalEventView {
        event: event.id,
        account: event.account,
        effective_date: event.order.date(),
        sequence: event.order.sequence(),
        source_time: event.order.source_time(),
        kind: event.kind.discriminant(),
        dates: event.dates,
        legs: event.legs.clone(),
        relation: event.relation,
        confidence: event.confidence,
        idempotency_key: event.idempotency_key.clone(),
        source: event.provenance.source(),
        import: event.provenance.import(),
        source_operation_id: event.provenance.source_operation_id().map(str::to_owned),
        source_category: event.provenance.source_category().map(str::to_owned),
        source_kind: event.provenance.source_kind().map(str::to_owned),
        description: event.provenance.description().map(str::to_owned),
        import_session: event.provenance.import_session(),
        rule_settlement: event.provenance.rule_settlement().copied(),
    }
}

/// The rule narrowing, with the pair checked before either half is used.
///
/// A version numbers one rule's own revisions, so a version with no rule beside
/// it names nothing at all. Accepting it and ignoring it would answer a question
/// nobody asked — every version's rows under a request for one — and the caller
/// would have no way to tell that from a rule genuinely edited only once.
fn rule_filter(
    rule: Option<ClassificationRuleId>,
    version: Option<u32>,
) -> Result<(Option<ClassificationRuleId>, Option<u32>), AppError> {
    if rule.is_none() {
        if let Some(version) = version {
            return Err(AppError::Invalid {
                field: "settled_by_rule_version".to_owned(),
                expected: "a rule named beside the version, because a version numbers one \
                           rule's own revisions"
                    .to_owned(),
                actual: version.to_string(),
            });
        }
    }
    Ok((rule, version))
}

fn page_size(limit: Option<u32>) -> Result<u32, AppError> {
    let Some(limit) = limit else {
        return Ok(DEFAULT_PAGE_SIZE);
    };
    if limit == 0 || limit > MAX_PAGE_SIZE {
        return Err(AppError::Invalid {
            field: "limit".to_owned(),
            expected: format!("a page size between 1 and {MAX_PAGE_SIZE}"),
            actual: limit.to_string(),
        });
    }
    Ok(limit)
}

fn date_range(
    from: Option<Date>,
    to: Option<Date>,
) -> Result<(Option<Date>, Option<Date>), AppError> {
    if let (Some(from), Some(to)) = (from, to) {
        if from > to {
            return Err(AppError::Invalid {
                field: "to".to_owned(),
                expected: format!("a date on or after from ({from})"),
                actual: to.to_string(),
            });
        }
    }
    Ok((from, to))
}

/// The channel bound matches the one ingest applies when the same source is
/// declared: a channel this route would accept but ingest would refuse could
/// never name rows that exist.
fn declared_source(owner: OwnerId, declared: &DeclaredSource) -> Result<SourceId, AppError> {
    let channel = declared.channel.trim();
    if channel.is_empty() || channel.len() > 32 {
        return Err(AppError::Invalid {
            field: "source_channel".to_owned(),
            expected: "a short channel name of 1 to 32 characters, such as file, paste or manual"
                .to_owned(),
            actual: declared.channel.clone(),
        });
    }
    Ok(SourceId::declared(owner, declared.account, channel))
}

/// The wire form of a position: the date and the order within it, the two
/// values the journal's own unique index is built on.
fn format_cursor(effective_date: Date, sequence: u32) -> String {
    format!("{effective_date}:{sequence}")
}

fn parse_cursor(value: &str) -> Result<JournalCursor, AppError> {
    let invalid = || AppError::Invalid {
        field: "after".to_owned(),
        expected: "a position returned as next by an earlier page".to_owned(),
        actual: value.to_owned(),
    };
    let (date, sequence) = value.split_once(':').ok_or_else(invalid)?;
    let effective_date = Date::parse(date, &time::format_description::well_known::Iso8601::DATE)
        .map_err(|_| invalid())?;
    let sequence = sequence.parse::<u32>().map_err(|_| invalid())?;
    Ok(JournalCursor {
        effective_date,
        sequence,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::date;

    #[test]
    fn an_absent_page_size_is_the_default_and_zero_is_refused() {
        // Zero is not "no limit": a query with LIMIT 0 returns nothing, and a
        // caller would read an empty journal instead of a refusal.
        assert_eq!(page_size(None).expect("default"), DEFAULT_PAGE_SIZE);
        assert_eq!(page_size(Some(1)).expect("one row"), 1);
        assert!(page_size(Some(0)).is_err());
        assert!(page_size(Some(MAX_PAGE_SIZE)).is_ok());
        assert!(page_size(Some(MAX_PAGE_SIZE + 1)).is_err());
    }

    #[test]
    fn a_page_size_refusal_names_the_field() {
        let error = page_size(Some(0)).expect_err("zero refused");
        let AppError::Invalid { field, actual, .. } = error else {
            panic!("a page size is refused as an invalid field");
        };
        assert_eq!(field, "limit");
        assert_eq!(actual, "0");
    }

    #[test]
    fn an_inverted_range_is_refused_and_names_the_later_bound() {
        let error = date_range(Some(date!(2026 - 03 - 02)), Some(date!(2026 - 03 - 01)))
            .expect_err("inverted range");
        let AppError::Invalid { field, .. } = error else {
            panic!("an inverted range is refused as an invalid field");
        };
        assert_eq!(field, "to");
    }

    #[test]
    fn a_half_open_or_absent_range_is_accepted() {
        // Narrowing is optional, so one bound alone must be usable.
        assert!(date_range(None, None).is_ok());
        assert!(date_range(Some(date!(2026 - 03 - 01)), None).is_ok());
        assert!(date_range(None, Some(date!(2026 - 03 - 01))).is_ok());
        assert!(date_range(Some(date!(2026 - 03 - 01)), Some(date!(2026 - 03 - 01))).is_ok());
    }

    #[test]
    fn a_cursor_survives_the_round_trip() {
        let cursor = parse_cursor(&format_cursor(date!(2026 - 03 - 01), 7)).expect("cursor");
        assert_eq!(cursor.effective_date, date!(2026 - 03 - 01));
        assert_eq!(cursor.sequence, 7);
    }

    #[test]
    fn a_malformed_cursor_is_refused_rather_than_read_as_the_beginning() {
        // Silently starting from the top would hand the caller page one again
        // and page two would never arrive.
        for value in [
            "",
            "2026-03-01",
            "2026-03-01:",
            ":7",
            "not-a-date:7",
            "2026-03-01:-1",
        ] {
            let error = parse_cursor(value).expect_err("malformed cursor");
            let AppError::Invalid { field, .. } = error else {
                panic!("a malformed cursor is refused as an invalid field: {value}");
            };
            assert_eq!(field, "after");
        }
    }

    #[test]
    fn a_rule_narrows_the_journal_and_a_version_narrows_it_further() {
        let rule = ClassificationRuleId::new_random();
        assert_eq!(
            rule_filter(None, None).expect("no rule named"),
            (None, None)
        );
        assert_eq!(
            rule_filter(Some(rule), None).expect("the rule alone"),
            (Some(rule), None)
        );
        assert_eq!(
            rule_filter(Some(rule), Some(3)).expect("one version of it"),
            (Some(rule), Some(3))
        );
    }

    #[test]
    fn a_rule_version_with_no_rule_beside_it_is_refused() {
        // A version numbers one rule's own revisions, so «version 3» on its own
        // names nothing. Accepting it and ignoring it would hand back every
        // version's rows under a question that asked for one.
        let error = rule_filter(None, Some(3)).expect_err("a version alone is refused");
        let AppError::Invalid { field, actual, .. } = error else {
            panic!("a lone version is refused as an invalid field");
        };
        assert_eq!(field, "settled_by_rule_version");
        assert_eq!(actual, "3");
    }

    #[test]
    fn a_declared_source_resolves_to_the_identity_ingest_derived() {
        let owner = OwnerId::new_random();
        let account = AccountId::new_random();
        let declared = DeclaredSource {
            account,
            channel: " file ".to_owned(),
        };
        assert_eq!(
            declared_source(owner, &declared).expect("declared source"),
            SourceId::declared(owner, account, "file"),
            "the read must derive the same identity ingest wrote under"
        );
    }

    #[test]
    fn an_empty_channel_is_refused() {
        let declared = DeclaredSource {
            account: AccountId::new_random(),
            channel: "   ".to_owned(),
        };
        let error =
            declared_source(OwnerId::new_random(), &declared).expect_err("empty channel refused");
        let AppError::Invalid { field, .. } = error else {
            panic!("an empty channel is refused as an invalid field");
        };
        assert_eq!(field, "source_channel");
    }
}
