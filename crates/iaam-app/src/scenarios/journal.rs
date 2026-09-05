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
use iaam_core::event::leg::{Leg, LegKind};
use iaam_core::event::provenance::RuleSettlement;
use iaam_core::event::{Confidence, Relation};
use iaam_core::ids::{
    AccountId, ClassificationRuleId, CustodyId, EventId, ImportId, ImportSessionId, InstrumentId,
    OwnerId, SourceId,
};
use iaam_core::money::{Money, Quantity};
use time::{Date, Time};

use crate::error::AppError;
use crate::ports::{JournalCursor, JournalQuery, RecordedEvent, Store};

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

/// One thing the owner did to an operation, as opposed to one fact the journal
/// holds.
///
/// A correction is two facts — a reversal of the target and a replacement of
/// it — and publishing them raw would show him two entries for one thing he
/// did and leave him to work out that they are one act. The pairing is
/// unambiguous, because both name the same target and `resolve` refuses a
/// second replacement of an event, so it is done once, here, and every reader
/// downstream is handed acts.
///
/// Three of them, and no more.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryAct {
    /// The fact entered the journal. The first step of every history, and the
    /// only step of most of them.
    ///
    /// It is also where a history begins when the head of the chain names a
    /// target this owner's journal does not hold — see
    /// [`read_operation_history`] for why that ends the walk rather than
    /// failing it.
    Arrived,
    /// A reversal and a replacement of the same target: the fact stopped
    /// counting and another took its place. The state after the act is the
    /// replacement.
    Corrected,
    /// A reversal with no replacement: the fact stopped counting and nothing
    /// took its place, so there is no state after it.
    ///
    /// A whole import taken back writes exactly the reversal one corrected row
    /// does, and this says the same word for both. What the owner is looking at
    /// is his operation, and «this was taken back» is the same fact about it
    /// either way; which act it belonged to he reads off the state before the
    /// retraction, which names the import the fact arrived in.
    Retracted,
}

/// Which aspect of the fact an act made different.
///
/// A closed vocabulary computed from the two published states, and **not a
/// rendered before-and-after**. Both states are published in full, so rendering
/// the difference as well would be a second answer to the same question, and
/// the two would come to disagree in front of the owner. These name where the
/// difference is; what it is, he reads off the states themselves.
///
/// **A category is deliberately not among them.** A category is not recorded on
/// the fact at all — it is decided by the owner's category rules when a report
/// is computed — so «I filed this under the wrong category» is not a correction
/// of an operation, and this history will not show it. Saying so is the point:
/// an empty `changed` that quietly meant «we do not record that» would tell him
/// nothing had happened.
///
/// Provenance is not among them either, for the opposite reason. A replacement
/// is a new fact and necessarily carries its own source and its own idempotency
/// key, so calling those changed would mark every correction as having changed
/// them and would say nothing at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangedAspect {
    /// The event family — `cash_in`, `trade`, `income` and so on.
    Kind,
    /// What moved: the money and the quantity the legs carry, and the
    /// instrument they name.
    Amount,
    /// Where it moved: the account the fact is filed under, and the account and
    /// custody each leg posts to.
    Account,
    /// When it happened: the date the journal orders the fact by, the time of
    /// day the source stated, and the semantic dates the fact carries.
    Dates,
    /// Who the far side was, as the source printed it on the row.
    Counterparty,
    /// How sure the fact is.
    Confidence,
}

/// One act, with the state it left behind.
#[derive(Debug, Clone, PartialEq)]
pub struct HistoryStep {
    /// Which of the three things happened.
    pub act: HistoryAct,
    /// When the act was recorded: this instance's own clock at the moment it
    /// wrote the fact down, RFC 3339.
    ///
    /// **Not the effective date, and never published as one.** A correction
    /// written in March to a fact effective in January is a March act about a
    /// January fact, and a reader that confused the two would date the owner's
    /// change of mind to the day of the operation he changed his mind about.
    /// The effective date is on the state, where it belongs.
    ///
    /// An act can be two facts and each carries a stamp. The earlier of the two
    /// is when the act reached the journal; taking the replacement's would date
    /// a correction by whichever half happened to be written second, and a
    /// caller is free to send the two in either order.
    pub at: String,
    /// The fact as it stands after this act.
    ///
    /// `None` after a retraction, because nothing stands after one. It is the
    /// same view the journal listing returns, so that the two routes can never
    /// describe one fact differently.
    pub state: Option<JournalEventView>,
    /// Which aspects this act made different from the previous state.
    ///
    /// Empty on an arrival, which changed nothing because there was nothing
    /// before it, and after a retraction, which left no state to compare.
    pub changed: Vec<ChangedAspect>,
    /// The reversal this act wrote, where it wrote one.
    ///
    /// Published so that every entry of the history is addressable by name: a
    /// correction of a correction can then be asked about on its own.
    pub reversal: Option<EventId>,
    /// The replacement this act wrote, where it wrote one.
    ///
    /// `None` on an arrival and on a retraction, and that absence is the whole
    /// difference between a retraction and a correction.
    pub replacement: Option<EventId>,
}

/// One operation's life: what it was when it arrived, and every act since.
#[derive(Debug, Clone, PartialEq)]
pub struct OperationHistory {
    /// The acts, oldest first, because a history is read forwards: he wants to
    /// see what the operation was before he sees what it became.
    pub steps: Vec<HistoryStep>,
    /// The fact that counts now.
    ///
    /// `None` where the operation ends in a retraction. It is the identifier of
    /// the last step's state, published beside the steps so that a caller about
    /// to act on the operation — correcting it once more — need not work out
    /// which step that is.
    pub current: Option<EventId>,
}

/// One operation's history, entered by any identifier in it.
///
/// The original, the replacement standing now and every reversal written along
/// the way all name the same operation, so all of them answer with the same
/// history: which handle he happens to hold must not decide which question he
/// may ask. [`Store::event_chain`] does the walking; this turns the facts it
/// returns into the acts that wrote them.
///
/// An identifier that addresses no event of his is an [`AppError::NotFound`] —
/// the same refusal [`read_journal`] gives an idempotency key that addresses
/// nothing, and the same refusal for an event of somebody else's, deliberately,
/// since telling those two apart would confirm that a stranger's event exists.
///
/// **A head that names a target outside his journal.** The backward walk stops
/// at a `relation.target` this owner's journal does not hold rather than
/// failing over it, so such a fact becomes the head of the chain and is
/// published here as an arrival. The history says nothing further about it, on
/// purpose: from here a target that was never written, one that belongs to
/// another owner and one lost to a corrupt database are indistinguishable, so a
/// step phrased as «there was more before this» would be a claim nothing
/// supports, and one phrased as «there was not» would be false. What the reader
/// gets instead is the head's own `relation`, published verbatim on the state,
/// which says exactly what the fact says and no more.
pub async fn read_operation_history(
    store: &dyn Store,
    owner: OwnerId,
    event: EventId,
) -> Result<OperationHistory, AppError> {
    let chain = store.event_chain(owner, event).await?;
    let Some(head) = chain.first() else {
        return Err(AppError::NotFound {
            what: "journal event",
            id: event.inner().to_string(),
        });
    };
    Ok(acts_of(head, &chain))
}

/// Fold a chain of facts into the acts that wrote it.
///
/// One pass forward from the head, pairing at each step the reversal and the
/// replacement that name the fact standing there.
fn acts_of(head: &RecordedEvent, chain: &[RecordedEvent]) -> OperationHistory {
    let mut standing = journal_event_view(&head.event);
    let mut steps = vec![HistoryStep {
        act: HistoryAct::Arrived,
        at: head.recorded_at.clone(),
        state: Some(standing.clone()),
        changed: Vec::new(),
        reversal: None,
        replacement: None,
    }];
    let mut current = Some(head.event.id);

    // Every act consumes at least one fact of the chain, so the chain's length
    // bounds the number of acts. The bound is what stops a database the core
    // could not have written from being walked forever.
    for _ in 0..chain.len() {
        let Some(target) = current else { break };
        let reversal = reversal_of(chain, target);
        let Some(replacement) = replacement_of(chain, target) else {
            let Some(reversal) = reversal else { break };
            steps.push(HistoryStep {
                act: HistoryAct::Retracted,
                at: reversal.recorded_at.clone(),
                state: None,
                changed: Vec::new(),
                reversal: Some(reversal.event.id),
                replacement: None,
            });
            current = None;
            break;
        };
        let state = journal_event_view(&replacement.event);
        steps.push(HistoryStep {
            act: HistoryAct::Corrected,
            at: recorded_first(reversal, replacement),
            changed: changed_aspects(&standing, &state),
            state: Some(state.clone()),
            reversal: reversal.map(|reversal| reversal.event.id),
            replacement: Some(replacement.event.id),
        });
        current = Some(replacement.event.id);
        standing = state;
    }

    OperationHistory { steps, current }
}

/// The fact that reverses the named one, where the chain holds it.
///
/// The first such fact, not every one of them: a second reversal of one target
/// is a second fact but not a second retraction, and a database that holds one
/// must not turn into two acts about the same target.
fn reversal_of(chain: &[RecordedEvent], target: EventId) -> Option<&RecordedEvent> {
    chain.iter().find(
        |recorded| matches!(recorded.event.relation, Relation::Reversal { target: named } if named == target),
    )
}

/// The fact that replaces the named one, where the chain holds it.
///
/// The first, for the reason [`reversal_of`] takes the first: `resolve` refuses
/// to admit a second replacement of one event, so a chain holding two is a
/// database nothing here can have written, and following both would publish two
/// futures for one fact.
fn replacement_of(chain: &[RecordedEvent], target: EventId) -> Option<&RecordedEvent> {
    chain.iter().find(
        |recorded| matches!(recorded.event.relation, Relation::Replacement { target: named } if named == target),
    )
}

/// When an act of two facts reached the journal: the earlier of the two stamps.
///
/// Compared as text, which is what they are: every stamp is written by this
/// instance in the one RFC 3339 form, at UTC, so ordering them as strings
/// orders them in time.
fn recorded_first(reversal: Option<&RecordedEvent>, replacement: &RecordedEvent) -> String {
    reversal.map_or_else(
        || replacement.recorded_at.clone(),
        |reversal| {
            reversal
                .recorded_at
                .clone()
                .min(replacement.recorded_at.clone())
        },
    )
}

/// The aspects two published states differ in, in the order they are declared.
///
/// Computed from the views the caller is handed and from nothing else. Read off
/// the underlying facts instead, it could name a difference the caller cannot
/// see in what he was shown — and then the difference and the states would
/// disagree, which is exactly what publishing both was meant to prevent.
fn changed_aspects(before: &JournalEventView, after: &JournalEventView) -> Vec<ChangedAspect> {
    let mut changed = Vec::new();
    if before.kind != after.kind {
        changed.push(ChangedAspect::Kind);
    }
    if what_moved(before) != what_moved(after) {
        changed.push(ChangedAspect::Amount);
    }
    if where_it_moved(before) != where_it_moved(after) {
        changed.push(ChangedAspect::Account);
    }
    if (before.effective_date, before.source_time, before.dates)
        != (after.effective_date, after.source_time, after.dates)
    {
        changed.push(ChangedAspect::Dates);
    }
    if before.description != after.description {
        changed.push(ChangedAspect::Counterparty);
    }
    if before.confidence != after.confidence {
        changed.push(ChangedAspect::Confidence);
    }
    changed
}

/// What one leg moved, with where it moved left out.
type Moved = (
    LegKind,
    Option<Money>,
    Option<Quantity>,
    Option<InstrumentId>,
);

/// Where one leg posted.
type Posted = (AccountId, Option<CustodyId>);

/// What moved, leg by leg.
fn what_moved(view: &JournalEventView) -> Vec<Moved> {
    view.legs
        .iter()
        .map(|leg| (leg.kind, leg.money, leg.quantity, leg.instrument))
        .collect()
}

/// Where it moved: the account the fact is filed under, and every leg's.
///
/// The fact's own account is compared beside the legs' rather than instead of
/// them: a fact with no leg at all — a valuation, a control assertion — still
/// names the account it is about, and moving one from `Main` to `Savings` is a
/// change the owner must be shown.
fn where_it_moved(view: &JournalEventView) -> (AccountId, Vec<Posted>) {
    (
        view.account,
        view.legs
            .iter()
            .map(|leg| (leg.account, leg.custody))
            .collect(),
    )
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
    use std::sync::Arc;

    use iaam_core::dates::{CashPostedDate, EffectiveOrder};
    use iaam_core::event::kind::EventKind;
    use iaam_core::event::provenance::{ParserVersion, Provenance, RawHash};
    use iaam_core::money::{CurrencyCode, PostedMinor};
    use iaam_core::reconciliation::evidence::IdentityScope;
    use iaam_store::SqliteStore;
    use time::macros::date;

    use super::*;
    use crate::AppServices;
    use crate::adapters::sqlite::SqliteAdapter;
    use crate::ports::{Clock, Principal, Scope};
    use crate::scenarios::correction::{
        CorrectionRequest, ImportTarget, correct_events, correct_import,
    };

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

    // An operation's history: the acts, not the facts.

    struct FixedClock;

    impl Clock for FixedClock {
        fn today(&self) -> Date {
            date!(2021 - 06 - 30)
        }
    }

    /// One owner, two accounts of his, and a journal to write facts into.
    ///
    /// Every name and figure below is invented: `Main` and `Savings` are the
    /// accounts, `Shop One` is what the source printed on the row.
    struct Ctx {
        owner: OwnerId,
        main: AccountId,
        savings: AccountId,
        source: SourceId,
        services: AppServices,
        principal: Principal,
    }

    impl Ctx {
        fn new() -> Self {
            let adapter = Arc::new(SqliteAdapter::new(
                SqliteStore::open_in_memory().expect("an in-memory journal"),
            ));
            let owner = OwnerId::new_random();
            Self {
                owner,
                main: AccountId::new_random(),
                savings: AccountId::new_random(),
                source: SourceId::new_random(),
                services: AppServices::new(
                    adapter.clone(),
                    adapter.clone(),
                    adapter.clone(),
                    adapter,
                    Arc::new(FixedClock),
                ),
                principal: Principal {
                    token_id: uuid::Uuid::new_v4(),
                    owner,
                    scope: Scope::Owner,
                },
            }
        }

        fn store(&self) -> &dyn Store {
            self.services.store.as_ref()
        }

        /// A deposit at one of his accounts, with `hash` making each fixture a
        /// distinct row of the journal.
        fn recorded(
            &self,
            hash: u8,
            minor: i64,
            account: AccountId,
            import: Option<ImportId>,
        ) -> iaam_core::event::Event {
            let amount = Money::new(PostedMinor::new(minor), CurrencyCode::Rub);
            let day = date!(2021 - 03 - 02);
            let mut provenance = Provenance::new(
                self.source,
                RawHash::parse(&format!("{hash:064x}")).expect("a raw hash"),
                ParserVersion("manual/1".into()),
            )
            .with_description("Shop One");
            if let Some(import) = import {
                provenance = provenance.with_import(import);
            }
            iaam_core::event::Event {
                id: EventId::new_random(),
                schema_version: iaam_core::event::SCHEMA_VERSION,
                owner: self.owner,
                account,
                kind: EventKind::CashIn { amount },
                dates: EventDates::for_cash(CashPostedDate(day)),
                order: EffectiveOrder::new(day, 0),
                legs: vec![Leg::cash(account, amount)],
                provenance,
                relation: Relation::None,
                confidence: Confidence::Known,
                idempotency_key: None,
            }
        }

        fn deposit(&self, hash: u8, minor: i64, account: AccountId) -> iaam_core::event::Event {
            self.recorded(hash, minor, account, None)
        }

        fn imported(&self, hash: u8, minor: i64, import: ImportId) -> iaam_core::event::Event {
            self.recorded(hash, minor, self.main, Some(import))
        }

        async fn write(&self, events: &[iaam_core::event::Event]) {
            self.services
                .store
                .append_events(events.to_vec(), IdentityScope::Source)
                .await
                .expect("the journal accepts the facts");
        }

        async fn history(&self, event: EventId) -> OperationHistory {
            read_operation_history(self.store(), self.owner, event)
                .await
                .expect("a history of an operation of his")
        }

        /// One correction is a reversal and a replacement of the same target,
        /// so an operation corrected twice is five facts. The first correction
        /// fixes the sum; the second says it landed at `Savings`.
        async fn corrected_twice(&self) -> [iaam_core::event::Event; 5] {
            let original = self.deposit(1, 4_500, self.main);
            let first_reversal = iaam_core::event::Event {
                relation: Relation::Reversal {
                    target: original.id,
                },
                ..self.deposit(2, 4_500, self.main)
            };
            let first_replacement = iaam_core::event::Event {
                relation: Relation::Replacement {
                    target: original.id,
                },
                ..self.deposit(3, 5_200, self.main)
            };
            let second_reversal = iaam_core::event::Event {
                relation: Relation::Reversal {
                    target: first_replacement.id,
                },
                ..self.deposit(4, 5_200, self.main)
            };
            let second_replacement = iaam_core::event::Event {
                relation: Relation::Replacement {
                    target: first_replacement.id,
                },
                ..self.deposit(5, 5_200, self.savings)
            };
            let written = [
                original,
                first_reversal,
                first_replacement,
                second_reversal,
                second_replacement,
            ];
            self.write(&written).await;
            written
        }
    }

    /// The identifier he most often holds is the one a correction just handed
    /// back to him, and the one his import gave him names the same operation.
    /// Which handle he happens to hold must not decide which question he may
    /// ask.
    #[tokio::test]
    async fn every_identifier_of_an_operation_reads_the_same_history() {
        let ctx = Ctx::new();
        let written = ctx.corrected_twice().await;
        let expected = ctx.history(written[0].id).await;

        for entered in &written {
            assert_eq!(
                ctx.history(entered.id).await,
                expected,
                "entering by {} answered a different question",
                entered.id.inner()
            );
        }
    }

    /// Four facts on top of the original, and two things he did. He must be
    /// shown the two, each with when it happened and what it made different.
    #[tokio::test]
    async fn an_operation_corrected_twice_reads_as_three_states_and_two_corrections() {
        let ctx = Ctx::new();
        let written = ctx.corrected_twice().await;
        let history = ctx.history(written[0].id).await;

        assert_eq!(
            history
                .steps
                .iter()
                .map(|step| step.act)
                .collect::<Vec<_>>(),
            vec![
                HistoryAct::Arrived,
                HistoryAct::Corrected,
                HistoryAct::Corrected
            ],
        );
        assert_eq!(
            history
                .steps
                .iter()
                .filter_map(|step| step.state.as_ref().map(|state| state.event))
                .collect::<Vec<_>>(),
            vec![written[0].id, written[2].id, written[4].id],
            "three states, oldest first"
        );

        assert!(
            history.steps[0].changed.is_empty(),
            "an arrival changed nothing, because there was nothing before it"
        );
        assert_eq!(
            history.steps[1].changed,
            vec![ChangedAspect::Amount],
            "the first correction fixed the sum and nothing else"
        );
        assert_eq!(
            history.steps[2].changed,
            vec![ChangedAspect::Account],
            "the second said it had landed at the other account"
        );

        assert_eq!(history.steps[1].reversal, Some(written[1].id));
        assert_eq!(history.steps[1].replacement, Some(written[2].id));
        assert_eq!(history.steps[2].reversal, Some(written[3].id));
        assert_eq!(history.steps[2].replacement, Some(written[4].id));
        assert_eq!(history.current, Some(written[4].id));

        for step in &history.steps {
            assert!(
                step.at.contains('T') && step.at.len() >= 20,
                "every act says when it was recorded: {}",
                step.at
            );
            assert!(
                !step.at.starts_with("2021-03-02"),
                "the recorded moment is this instance's clock, never the effective date: {}",
                step.at
            );
        }
    }

    /// A retraction leaves nothing standing. A reader offered a state after one
    /// would count a fact the owner took back.
    #[tokio::test]
    async fn a_retracted_operation_ends_with_nothing_standing() {
        let ctx = Ctx::new();
        let original = ctx.deposit(1, 4_500, ctx.main);
        let reversal = iaam_core::event::Event {
            relation: Relation::Reversal {
                target: original.id,
            },
            ..ctx.deposit(2, 4_500, ctx.main)
        };
        ctx.write(&[original.clone(), reversal.clone()]).await;

        let history = ctx.history(original.id).await;

        assert_eq!(
            history
                .steps
                .iter()
                .map(|step| step.act)
                .collect::<Vec<_>>(),
            vec![HistoryAct::Arrived, HistoryAct::Retracted],
        );
        let last = history.steps.last().expect("a last step");
        assert!(last.state.is_none(), "nothing stands after a retraction");
        assert!(
            last.changed.is_empty(),
            "a retraction left no state to compare"
        );
        assert_eq!(last.reversal, Some(reversal.id));
        assert_eq!(last.replacement, None);
        assert_eq!(
            history.current, None,
            "no fact of this operation counts now"
        );
    }

    /// Most of his journal is this: a fact that arrived and was never touched.
    /// Nothing about it may be phrased as a change.
    #[tokio::test]
    async fn an_operation_nothing_ever_touched_is_one_arrival() {
        let ctx = Ctx::new();
        let untouched = ctx.deposit(1, 4_500, ctx.main);
        ctx.write(std::slice::from_ref(&untouched)).await;

        let history = ctx.history(untouched.id).await;

        assert_eq!(history.steps.len(), 1);
        let step = &history.steps[0];
        assert_eq!(step.act, HistoryAct::Arrived);
        assert!(step.changed.is_empty(), "an arrival is not a change");
        assert_eq!(step.reversal, None);
        assert_eq!(step.replacement, None);
        assert_eq!(
            step.state.as_ref().map(|state| state.event),
            Some(untouched.id)
        );
        assert_eq!(history.current, Some(untouched.id));
    }

    /// A whole import taken back writes exactly the reversal one corrected row
    /// does. What the owner is looking at is his operation, and «this was taken
    /// back» is the same fact about it either way.
    #[tokio::test]
    async fn a_row_retracted_with_its_import_reads_as_one_retracted_alone() {
        let ctx = Ctx::new();
        let batch = ImportId::new_random();
        let alone = ctx.imported(1, 4_500, ImportId::new_random());
        let in_batch = ctx.imported(2, 6_100, batch);
        ctx.write(&[alone.clone(), in_batch.clone()]).await;

        correct_events(
            &ctx.services,
            &ctx.principal,
            true,
            &[CorrectionRequest::Reversal { target: alone.id }],
        )
        .await
        .expect("one row taken back on its own");
        correct_import(
            &ctx.services,
            &ctx.principal,
            true,
            ImportTarget::Named {
                source: ctx.source,
                import: batch,
            },
        )
        .await
        .expect("the whole import taken back");

        let one_row = ctx.history(alone.id).await;
        let whole_import = ctx.history(in_batch.id).await;

        assert_eq!(
            whole_import
                .steps
                .iter()
                .map(|step| step.act)
                .collect::<Vec<_>>(),
            one_row
                .steps
                .iter()
                .map(|step| step.act)
                .collect::<Vec<_>>(),
            "a row taken back with its import reads as one taken back alone"
        );
        let last = whole_import.steps.last().expect("a last step");
        assert_eq!(last.act, HistoryAct::Retracted);
        assert!(last.state.is_none());
        assert_eq!(whole_import.current, None);
    }

    /// Task 5 ends the backward walk at a `relation.target` this owner's
    /// journal does not hold, so the fact naming it is the head. The history
    /// publishes it as an arrival and claims nothing further about the target:
    /// the state carries the relation verbatim, which says exactly what the
    /// fact says and no more.
    #[tokio::test]
    async fn a_fact_naming_a_target_outside_his_journal_is_where_his_history_begins() {
        let ctx = Ctx::new();
        let stranger = EventId::new_random();
        let head = iaam_core::event::Event {
            relation: Relation::Replacement { target: stranger },
            ..ctx.deposit(1, 4_500, ctx.main)
        };
        ctx.write(std::slice::from_ref(&head)).await;

        let history = ctx.history(head.id).await;

        assert_eq!(history.steps.len(), 1);
        assert_eq!(history.steps[0].act, HistoryAct::Arrived);
        assert!(
            history.steps[0].changed.is_empty(),
            "there is no earlier state of his for it to have changed from"
        );
        assert_eq!(
            history.steps[0].state.as_ref().map(|state| state.relation),
            Some(Relation::Replacement { target: stranger }),
            "the head's own relation is published verbatim"
        );
        assert_eq!(history.current, Some(head.id));
    }

    /// An event identifier is a UUID, and a UUID confers no right to read
    /// someone else's journal. Holding one of another owner's must read exactly
    /// as holding an identifier of nothing at all — the refusal the listing
    /// gives an idempotency key that addresses nothing.
    #[tokio::test]
    async fn an_event_of_another_owner_is_not_found() {
        let ctx = Ctx::new();
        let his = ctx.deposit(1, 4_500, ctx.main);
        ctx.write(std::slice::from_ref(&his)).await;

        let error = read_operation_history(ctx.store(), OwnerId::new_random(), his.id)
            .await
            .expect_err("another owner's event is not his to read");

        let AppError::NotFound { id, .. } = error else {
            panic!("an identifier that addresses nothing is a missing resource");
        };
        assert_eq!(id, his.id.inner().to_string());
    }
}
