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

use iaam_core::category::row_key;
use iaam_core::dates::EventDates;
use iaam_core::event::correction::{
    SupersededBy, Supersession, resolve_supersession, resolve_with_unheld_targets,
};
use iaam_core::event::kind::{
    EventKind, FeeOrigin, IncomeKind, OpeningAssertions, TaxOrigin, TradeSide,
};
use iaam_core::event::leg::{Leg, LegKind};
use iaam_core::event::provenance::RuleSettlement;
use iaam_core::event::{Confidence, Event, Relation};
use iaam_core::ids::{
    AccountId, CategoryId, ClassificationRuleId, CustodyId, EventId, ImportId, ImportSessionId,
    InstrumentId, OwnerId, SourceId,
};
use iaam_core::money::CurrencyCode;
use iaam_core::money::{CalcMoney, Money, PerUnitAmount, Quantity};
use iaam_core::report::journal::{self as journal_aggregate, JournalAggregateError};
pub use iaam_core::report::journal::{
    JournalAggregate, JournalAggregateGroup, JournalAggregateGroupBy,
};
use iaam_core::valuation::PriceQuality;
use time::{Date, Time};

use crate::error::AppError;
use crate::ports::{
    JournalCursor, JournalQuery, JournalSourceCategoryQuery, JournalStore, RecordedEvent, Store,
};

/// Rows returned when the caller names no size.
pub const DEFAULT_PAGE_SIZE: u32 = 50;

/// The largest page the route will assemble.
///
/// A ceiling rather than a silent clamp: a caller that asks for more than the
/// 1,000-row ceiling cannot tell a truncated page from the end of the journal,
/// and would stop reading one page early.
pub const MAX_PAGE_SIZE: u32 = 1_000;

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
    /// Only events whose own `account` column names this account. This is
    /// different from [`Self::touching`], which also includes an event whose
    /// leg posts to the account.
    pub touching: Option<AccountId>,
    /// Only events of one of these event-family discriminants. An event kind
    /// filter selects the event as a whole; it does not narrow the legs.
    pub kinds: Vec<String>,
    /// Only events with at least one leg whose money has one of these currencies.
    /// The selected event is returned as a whole, including legs in other
    /// currencies.
    pub currencies: Vec<CurrencyCode>,
    pub source: Option<DeclaredSource>,
    /// The declared import that carried these rows. Unlike `import_session`,
    /// this survives across multiple sessions for the same statement and is the
    /// identity `POST /v1/corrections/imports` retracts.
    pub import: Option<ImportId>,
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
    /// Only events `event_category_assignments` assigns to this category
    /// (spec §4.7). The store rebuilds the projection first if it is stale
    /// against the owner's current rules, so this never quietly answers from
    /// a rule set he has since changed. Mutually exclusive with
    /// [`Self::uncategorised`] — a request naming both is refused.
    pub category: Option<CategoryId>,
    /// Only events with no row in the category projection at all —
    /// `NotDecomposed`, the honest absence, never a sentinel category.
    /// Mutually exclusive with [`Self::category`].
    pub uncategorised: bool,
    /// Only events that are a movement whose **far** endpoint is this
    /// account: a transfer read from one of its own two ends, naming the
    /// other. Deliberately narrower than [`Self::touching`], which is
    /// symmetric and also matches this account as the near side — a transfer
    /// `Main -> Savings` read from `Main` is matched by
    /// `counterparty = Savings` and not by `counterparty = Main`.
    pub counterparty: Option<AccountId>,
    /// Whether to include only events that belong to the effective set (`true`)
    /// or only withdrawn and correction-marker events (`false`). Omitted keeps
    /// every event. The page cursor still advances over store rows, so a
    /// filtered page may be short or empty while `next` is present.
    pub stands: Option<bool>,
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
/// A journal movement query without row-pagination controls.
#[derive(Debug, Clone, Default)]
pub struct JournalAggregateQuery {
    pub account: Option<AccountId>,
    pub touching: Option<AccountId>,
    pub source: Option<DeclaredSource>,
    pub import: Option<ImportId>,
    pub import_session: Option<ImportSessionId>,
    pub settled_by_rule: Option<ClassificationRuleId>,
    /// Only events `event_category_assignments` assigns to this category.
    /// Mutually exclusive with [`Self::uncategorised`]. The same meaning as
    /// on [`JournalReadQuery::category`] — that pairing is the design of the
    /// two routes.
    pub category: Option<CategoryId>,
    /// Only events with no row in the category projection at all. Mutually
    /// exclusive with [`Self::category`].
    pub uncategorised: bool,
    /// Only events that are a movement whose far endpoint is this account.
    /// The same meaning as on [`JournalReadQuery::counterparty`].
    pub counterparty: Option<AccountId>,
    /// Only events of one of these event-family discriminants. An event kind
    /// filter selects the event as a whole; it does not narrow the legs.
    pub kinds: Vec<String>,
    /// Only events with at least one leg whose money has one of these currencies.
    /// The selected event is returned as a whole, including legs in other
    /// currencies.
    pub currencies: Vec<CurrencyCode>,
    pub stands: Option<bool>,
    pub from: Option<Date>,
    pub to: Option<Date>,
    pub group_by: Vec<JournalAggregateGroupBy>,
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
    /// The sum the fact states about itself, where it posts no leg that states
    /// one.
    ///
    /// A fact with legs says its money in them and they are published above; a
    /// fact with none says it only in its kind, and without this field the whole
    /// of what such a fact says would be missing here. One family carries it
    /// today, and it is the family that needs it: a movement between the owner's
    /// own accounts whose direction the source never stated posts nothing at all
    /// — the journal will not debit or credit an account on a direction nobody
    /// gave — and yet the fact records the magnitude the source printed. Without
    /// this, two such facts at different sums read identically.
    ///
    /// **Not a total, and never a leg repeated.** This route adds nothing up,
    /// as `legs` says; the value here is one figure the fact itself holds, and
    /// it is `None` wherever a leg already carries the money, so that no number
    /// has two places to be read from.
    pub amount: Option<Money>,
    /// The basis-only fee a trade states, where one was recorded.
    ///
    /// This is separate from `amount`: a trade already publishes its cash and
    /// security legs, while this fee is an audit figure that is not a leg.
    pub basis_fee: Option<Money>,
    /// Whether this event reverses or replaces another.
    pub relation: Relation,
    /// Whether this event was withdrawn without a replacement, or replaced by
    /// the named event. This is derived from the same correction resolution
    /// that computes the effective set.
    pub superseded_by: Option<SupersededBy>,
    /// Whether this event belongs to the effective set used by reports.
    pub stands: bool,
    pub confidence: Confidence,
    pub idempotency_key: Option<String>,
    /// The exact key consumed by `CategoryMatcher::Row`.
    ///
    /// A source operation id has precedence over the caller's idempotency key,
    /// matching category assignment and category-rule preview.
    pub row_key: Option<String>,
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
    /// Four states, and the ones past «a rule filed it» are the ones he must be
    /// able to see. `rule` names a rule and its version: the group one standing
    /// decision of his reached without asking him again.
    /// `answered_minting_rule` names a rule and its version too, and says he
    /// answered this row by hand and that same answer became the rule — the two
    /// are different claims about **who decided**, which is why they stay two
    /// words. `no_rule` says a reading ran and none of his rules matched, so the
    /// row was settled some other way: his account directory recognised the far
    /// side, the source asserted it, the caller submitted a finished operation,
    /// or he answered it and the answer generalised into nothing. Absence is the
    /// fourth and is none of these: nothing was recorded about rules at all —
    /// every fact written before this field existed, and every route that writes
    /// without reading a row against the rules.
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
pub async fn read_journal<S: JournalStore + ?Sized>(
    store: &S,
    owner: OwnerId,
    query: JournalReadQuery,
) -> Result<JournalPage, AppError> {
    let limit = page_size(query.limit)?;
    let range = date_range(query.from, query.to)?;
    let after = query.after.as_deref().map(parse_cursor).transpose()?;
    category_filter(query.category, query.uncategorised)?;
    let source = query
        .source
        .as_ref()
        .map(|declared| declared_source(owner, declared))
        .transpose()?;
    let events = store
        .list_journal_events(
            owner,
            JournalQuery {
                event: None,
                idempotency_key: query.idempotency_key.clone(),
                account: query.account,
                touching: query.touching,
                kinds: query.kinds.clone(),
                currencies: query.currencies.clone(),
                source,
                import: query.import,
                import_session: query.import_session,
                settled_by_rule: query.settled_by_rule,
                category: query.category,
                uncategorised: query.uncategorised,
                counterparty: query.counterparty,
                from: range.0,
                to: range.1,
                after,
                limit: limit.saturating_add(1),
            },
        )
        .await?;
    let relations = store.list_journal_event_relations(owner).await?;
    let supersession = resolve_supersession(&relations).map_err(AppError::Correction)?;

    if events.is_empty() {
        if let Some(key) = query.idempotency_key {
            return Err(AppError::NotFound {
                what: "journal event by idempotency key",
                id: key,
            });
        }
    }

    let has_more = events.len() > limit as usize;
    let next = events
        .iter()
        .take(limit as usize)
        .next_back()
        .map(|event| format_cursor(event.order.date(), event.order.sequence()));
    let rows: Vec<JournalEventView> = events
        .iter()
        .take(limit as usize)
        .filter(|event| {
            query
                .stands
                .is_none_or(|stands| supersession.stands(event.id) == stands)
        })
        .map(|event| journal_event_view(event, &supersession))
        .collect();
    let next = has_more.then_some(next).flatten();
    Ok(JournalPage { rows, next })
}

/// Maximum number of groups returned by one aggregate request.
///
/// Aggregates are deliberately not paginated: a partial set of groups is
/// indistinguishable from a complete answer. Requests over this ceiling are
/// refused instead, and the caller can narrow the event filters.
pub const MAX_AGGREGATE_GROUPS: usize = 1_000;

/// Compute movement over the filtered events, folding their cash-bearing legs.
///
/// The filters select events, but grouping by `account` and `currency` selects
/// each leg's own account and currency. An event filed against one account can
/// therefore contribute to another account's group; using the filing account
/// here would not match the reports. `events` counts selected events in each
/// group, while `cash` sums every cash-bearing leg (`Cash`, fee, tax and
/// principal legs) without converting currencies.
///
/// This is movement over the selected window, not a balance. It does not read
/// opening assertions; [`crate::scenarios::reports::account_balances`] is the
/// report that turns movement plus an opening assertion into a balance.
/// With no grouping, an empty selection still returns one all-None group with
/// zero events, so the ungrouped answer has one stable shape.
pub async fn aggregate_journal<S: JournalStore + ?Sized>(
    store: &S,
    owner: OwnerId,
    query: JournalAggregateQuery,
) -> Result<JournalAggregate, AppError> {
    let range = date_range(query.from, query.to)?;
    category_filter(query.category, query.uncategorised)?;
    let source = query
        .source
        .as_ref()
        .map(|declared| declared_source(owner, declared))
        .transpose()?;
    let events = store
        .list_journal_events(
            owner,
            JournalQuery {
                event: None,
                idempotency_key: None,
                account: query.account,
                touching: query.touching,
                source,
                import: query.import,
                import_session: query.import_session,
                kinds: query.kinds.clone(),
                currencies: query.currencies.clone(),
                settled_by_rule: query.settled_by_rule,
                category: query.category,
                uncategorised: query.uncategorised,
                counterparty: query.counterparty,
                from: range.0,
                to: range.1,
                after: None,
                limit: u32::MAX,
            },
        )
        .await?;
    let relations = store.list_journal_event_relations(owner).await?;
    let supersession = resolve_supersession(&relations).map_err(AppError::Correction)?;
    let selected_events = events.iter().filter(|event| {
        query
            .stands
            .is_none_or(|stands| supersession.stands(event.id) == stands)
    });
    journal_aggregate::aggregate_journal(selected_events, &query.group_by, MAX_AGGREGATE_GROUPS)
        .map_err(|error| match error {
            JournalAggregateError::Money(error) => AppError::BatchTotal(error),
            JournalAggregateError::GroupCeiling { ceiling, actual } => AppError::Invalid {
                field: "group_by".to_owned(),
                expected: format!(
                    "at most {ceiling} groups; request would produce {actual}; \
                 narrow filters: account, touching, source_account, source_channel, \
                 source_label, import, import_session, settled_by_rule, kind, currency, \
                 from, to, stands"
                ),
                actual: query
                    .group_by
                    .iter()
                    .map(|dimension| format!("{dimension:?}"))
                    .collect::<Vec<_>>()
                    .join(","),
            },
        })
}

/// List the exact source-category vocabulary in a journal scope.
///
/// The store performs the distinct projection over the same journal facts that
/// [`read_journal`] returns; no spelling is normalised because matchers use the
/// recorded string exactly.
pub async fn list_journal_source_categories(
    store: &dyn Store,
    owner: OwnerId,
    account: Option<AccountId>,
    from: Option<Date>,
    to: Option<Date>,
) -> Result<Vec<String>, AppError> {
    let (from, to) = date_range(from, to)?;
    store
        .list_journal_source_categories(owner, JournalSourceCategoryQuery { account, from, to })
        .await
}

fn journal_event_view(
    event: &iaam_core::event::Event,
    supersession: &Supersession,
) -> JournalEventView {
    JournalEventView {
        event: event.id,
        account: event.account,
        effective_date: event.order.date(),
        sequence: event.order.sequence(),
        source_time: event.order.source_time(),
        kind: event.kind.discriminant(),
        dates: event.dates,
        legs: event.legs.clone(),
        amount: stated_amount(event),
        basis_fee: stated_basis_fee(event),
        relation: event.relation,
        superseded_by: supersession.superseded_by(event.id),
        stands: supersession.stands(event.id),
        confidence: event.confidence,
        idempotency_key: event.idempotency_key.clone(),
        row_key: row_key(event).map(str::to_owned),
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
/// A correction is normally two facts — a reversal of the target and a
/// replacement of it — and publishing them raw would show him two entries for
/// one thing he did and leave him to work out that they are one act. The
/// folding is unambiguous, because both name the same target and `resolve`
/// refuses a second replacement of an event, so it is done once, here, and
/// every reader downstream is handed acts. Where only one half was written the
/// act is still one entry, and which half it holds is on the step.
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
    /// A replacement of the target, and the reversal beside it where one was
    /// written: another fact took the target's place. The state after the act
    /// is the replacement.
    ///
    /// The replacement is what makes the act, and a reversal beside it is not
    /// required. `candidate_for` in [`crate::scenarios::correction`] checks
    /// only that a replacement's target exists, and
    /// [`iaam_core::event::correction::resolve`] drops a replaced fact from the
    /// effective set because it was replaced, not because a reversal names it —
    /// so the two halves may be submitted in separate calls, and a replacement
    /// alone is a correction. [`acts_of`] publishes such an act under this word
    /// with no `reversal`, which says what happened: something took the fact's
    /// place.
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
/// A closed vocabulary, and **not a rendered before-and-after**. The state
/// before and the state after are both published beside it, so rendering the
/// difference as well would be a second answer to the same question, and the two
/// would come to disagree in front of the owner. These name where the difference
/// is; what it is, he reads off the states themselves.
///
/// «Both states» is as much of each fact as this route publishes, which is
/// deliberately not the whole of it: [`JournalEventView`] leaves out the raw-row
/// hash, the parser version and the row locator, because they answer a different
/// question. None of those is an aspect below, so the argument holds — with one
/// exception, stated here rather than hidden. The scalars that tell two facts of
/// one family apart are compared under [`Self::Kind`] and are published on their
/// own nowhere: he is told the sort of the fact changed and must read the two
/// states to see how. Naming it is still the better failure, because the
/// alternative is silence, and silence reports that a correction changed
/// nothing.
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
    /// The event family — `cash_in`, `trade`, `income` and so on — and the
    /// scalars that tell two facts of one family apart: the side of a trade,
    /// the sort of an income, what a fee was for, whether a tax was withheld or
    /// paid. None of those changes the family word, and a correction that
    /// changed one of them and nothing else would otherwise read as no
    /// correction at all.
    Kind,
    /// What moved: the money and the quantity the legs carry, the instrument
    /// they name, and the figures the fact's own kind states — which, for a
    /// fact that posts no leg, are the whole of what it says.
    Amount,
    /// Where it moved: the account the fact is filed under, and the account and
    /// custody each leg posts to.
    Account,
    /// When it happened: the date the journal orders the fact by, the time of
    /// day the source stated, and the semantic dates the fact carries.
    Dates,
    /// The source description field, which may carry a description or the
    /// counterparty text printed on the row.
    SourceDescription,
    /// How sure the fact is — and, on a reconstructed opening or a valuation,
    /// what the fact itself asserts about how sure it is.
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
    /// before it, and after a retraction, which left no state to compare. The
    /// one arrival where empty does not mean that is a chain whose head names a
    /// target this owner's journal does not hold — see [`HistoryAct::Arrived`].
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
    let events: Vec<Event> = chain
        .iter()
        .map(|recorded| recorded.event.clone())
        .collect();
    // `stands` is resolved over this operation's own chain, which holds every
    // correction of that operation, rather than over the whole journal.
    let resolution = resolve_with_unheld_targets(&events).map_err(AppError::Correction)?;
    Ok(acts_of(head, &chain, resolution.supersession()))
}

/// Fold a chain of facts into the acts that wrote it.
///
/// One pass forward from the head, pairing at each step the reversal and the
/// replacement that name the fact standing there.
fn acts_of<'a>(
    head: &'a RecordedEvent,
    chain: &'a [RecordedEvent],
    supersession: &Supersession,
) -> OperationHistory {
    let mut steps = vec![HistoryStep {
        act: HistoryAct::Arrived,
        at: head.recorded_at.clone(),
        state: Some(journal_event_view(&head.event, supersession)),
        changed: Vec::new(),
        reversal: None,
        replacement: None,
    }];
    let mut current = Some(head.event.id);
    let mut consumed = Vec::new();

    for recorded in chain.iter().skip(1) {
        if consumed.contains(&recorded.event.id) {
            continue;
        }
        match recorded.event.relation {
            Relation::Reversal { target } => {
                let reversal = recorded;
                let Some(replacement) = replacement_of(chain, target)
                    .filter(|replacement| !consumed.contains(&replacement.event.id))
                else {
                    steps.push(HistoryStep {
                        act: HistoryAct::Retracted,
                        at: reversal.recorded_at.clone(),
                        state: None,
                        changed: Vec::new(),
                        reversal: Some(reversal.event.id),
                        replacement: None,
                    });
                    if current == Some(target) {
                        current = None;
                    }
                    continue;
                };
                consumed.push(replacement.event.id);
                steps.push(HistoryStep {
                    act: HistoryAct::Corrected,
                    at: recorded_first(Some(reversal), replacement),
                    changed: state_for_target(chain, target).map_or_else(Vec::new, |before| {
                        changed_aspects(before, &replacement.event)
                    }),
                    state: Some(journal_event_view(&replacement.event, supersession)),
                    reversal: Some(reversal.event.id),
                    replacement: Some(replacement.event.id),
                });
                current = Some(replacement.event.id);
            }
            Relation::Replacement { target } => {
                steps.push(HistoryStep {
                    act: HistoryAct::Corrected,
                    at: recorded.recorded_at.clone(),
                    changed: state_for_target(chain, target)
                        .map_or_else(Vec::new, |before| changed_aspects(before, &recorded.event)),
                    state: Some(journal_event_view(&recorded.event, supersession)),
                    reversal: reversal_of(chain, target).map(|reversal| reversal.event.id),
                    replacement: Some(recorded.event.id),
                });
                current = Some(recorded.event.id);
            }
            Relation::None => {}
        }
    }

    OperationHistory { steps, current }
}

/// The fact that reverses the named one, where the chain holds it.
///
/// This supplies the reversal beside a replacement-only correction. The fold
/// itself handles every reversal so that a second one cannot disappear.
fn reversal_of(chain: &[RecordedEvent], target: EventId) -> Option<&RecordedEvent> {
    chain.iter().find(
        |recorded| matches!(recorded.event.relation, Relation::Reversal { target: named } if named == target),
    )
}

/// The fact that replaces the named one, where the chain holds it.
///
/// The first is the one this fold can publish. `resolve` refuses to admit a
/// second replacement of one event, so a chain holding two is database state
/// nothing here could have written.
fn replacement_of(chain: &[RecordedEvent], target: EventId) -> Option<&RecordedEvent> {
    chain.iter().find(
        |recorded| matches!(recorded.event.relation, Relation::Replacement { target: named } if named == target),
    )
}

/// The state a replacement supersedes, if its target was a state-bearing fact.
///
/// Reversals never count as states. A replacement of one therefore starts a
/// new state after a retraction and has no before-state to compare with.
fn state_for_target(chain: &[RecordedEvent], target: EventId) -> Option<&Event> {
    chain
        .iter()
        .find(|recorded| recorded.event.id == target)
        .and_then(|recorded| match recorded.event.relation {
            Relation::Reversal { .. } => None,
            Relation::None | Relation::Replacement { .. } => Some(&recorded.event),
        })
}

/// When an act of two facts reached the journal: the earlier of the two stamps.
///
/// Compared as text, which is what they are, and text order is time order for
/// every pair whose stamps differ by a whole second or more: each is written by
/// this instance at UTC in the one RFC 3339 form, so the fields line up
/// character by character down to the seconds.
///
/// **Past the seconds it is not.** The writer omits the fractional part when
/// the nanoseconds are zero and trims its trailing zeros otherwise, so the
/// stamps are of variable width and `…:00.4Z` sorts before `…:00Z` on `'.'`
/// against `'Z'`. Two stamps can therefore come back in the wrong order, but
/// only when they fall inside one second — the prefix before the fractional
/// part is what decides every other pair — and only the value chosen is
/// affected, never a position: the steps of a history are ordered by the walk
/// and not by this, and the two halves of one act are one step. Parsing both
/// to compare them as instants would buy a sub-second distinction that nothing
/// reads.
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

/// The aspects two facts differ in, in the order they are declared.
///
/// Read off the **facts**, and that is deliberate rather than incidental. An
/// earlier reading compared only the two published views, which carry the family
/// word and the legs — so every payload inside [`EventKind`] was invisible to
/// it: the sort of an income, the origin of a fee, and the sum of a fact that
/// posts no leg at all. Correcting such a fact answered «you corrected this» and
/// «nothing changed» in one breath, which is exactly the silence
/// [`ChangedAspect`] excludes a category loudly to avoid.
///
/// What keeps this answer and the states beside it from disagreeing is not that
/// both are computed from the same input, but that what is named here is
/// published there: [`JournalEventView::amount`] exists for the one fact whose
/// sum lives nowhere else. The single thing named and not published is the
/// scalar that tells two facts of one family apart, and naming it is still the
/// better failure — see [`ChangedAspect::Kind`].
fn changed_aspects(before: &Event, after: &Event) -> Vec<ChangedAspect> {
    let (was, now) = (kind_aspects(&before.kind), kind_aspects(&after.kind));
    let mut changed = Vec::new();
    if before.kind.discriminant() != after.kind.discriminant() || was.word != now.word {
        changed.push(ChangedAspect::Kind);
    }
    if what_moved(before) != what_moved(after) || was.moved != now.moved {
        changed.push(ChangedAspect::Amount);
    }
    if where_it_moved(before) != where_it_moved(after) {
        changed.push(ChangedAspect::Account);
    }
    if (
        before.order.date(),
        before.order.source_time(),
        before.dates,
    ) != (after.order.date(), after.order.source_time(), after.dates)
    {
        changed.push(ChangedAspect::Dates);
    }
    if before.provenance.description() != after.provenance.description() {
        changed.push(ChangedAspect::SourceDescription);
    }
    if before.confidence != after.confidence || was.confidence != now.confidence {
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
fn what_moved(event: &Event) -> Vec<Moved> {
    event
        .legs
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
fn where_it_moved(event: &Event) -> (AccountId, Vec<Posted>) {
    (
        event.account,
        event
            .legs
            .iter()
            .map(|leg| (leg.account, leg.custody))
            .collect(),
    )
}

/// What a fact's own kind carries, sorted into the aspects it can differ in.
///
/// The legs say what was posted and the view publishes them; the kind says what
/// the fact claims, and for a fact that posts nothing the kind says all of it.
#[derive(Debug, Clone, PartialEq)]
struct KindAspects {
    /// Scalars that tell two facts of one family apart, for [`ChangedAspect::Kind`].
    word: Vec<KindWord>,
    /// Figures the payload states, for [`ChangedAspect::Amount`].
    moved: Vec<KindFigure>,
    /// What the fact asserts about how sure it is, for [`ChangedAspect::Confidence`].
    confidence: Vec<KindConfidence>,
}

/// A scalar two facts of one family are told apart by.
///
/// None of these changes the family word, so a correction that changes one of
/// them and nothing else would otherwise read as no correction at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KindWord {
    /// Bought or sold.
    Side(TradeSide),
    /// Coupon, dividend, interest — or, as `None`, a source that named none.
    Income(Option<IncomeKind>),
    /// What the fee was for.
    Fee(FeeOrigin),
    /// Withheld at source or paid by the owner.
    Tax(TaxOrigin),
}

/// A figure a fact's kind states.
#[derive(Debug, Clone, PartialEq)]
enum KindFigure {
    /// A posted sum the payload declares.
    Money(Money),
    /// The unrounded source commission behind a posted basis fee. Compared
    /// beside that fee rather than instead of it: the two can differ from each
    /// other, and the rounded one is the only one anything else reads.
    Exact(CalcMoney),
    /// A quantity of a security.
    Quantity(Quantity),
    /// The security the fact is about.
    Instrument(InstrumentId),
    /// A per-unit price, which is not a posted sum and is not typed as one.
    Price(PerUnitAmount),
    /// A payload compared entire, for the families whose figures are not picked
    /// out one by one — see [`kind_aspects`] for which and why.
    Whole(EventKind),
}

/// What a fact asserts about how sure it is of what it states.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KindConfidence {
    /// How a valuation's price was obtained.
    Price(PriceQuality),
    /// What a reconstructed opening asserts about itself.
    ///
    /// The set carries an acquisition date among its items, and it goes here
    /// rather than with [`ChangedAspect::Dates`] because what the set states is
    /// how sure the reconstruction is of each item, the date included. `Dates`
    /// is the dates the journal orders and reports by.
    Opening(OpeningAssertions),
}

/// Sort what a kind carries into the aspects it can differ in.
///
/// **An exhaustive match with no `_` arm, on purpose.** A variant added to
/// [`EventKind`] and not placed here would carry a payload that could change
/// under the owner without this route saying so, and the whole of what a
/// legless fact says is such a payload. Breaking the build is how the next
/// variant announces itself.
fn kind_aspects(kind: &EventKind) -> KindAspects {
    match kind {
        EventKind::Trade {
            side,
            instrument,
            quantity,
            gross,
            fee,
            basis_fee,
            basis_fee_exact,
            accrued_interest,
        } => KindAspects {
            word: vec![KindWord::Side(*side)],
            moved: figures([
                Some(KindFigure::Instrument(*instrument)),
                Some(KindFigure::Quantity(*quantity)),
                Some(KindFigure::Money(*gross)),
                fee.map(KindFigure::Money),
                basis_fee.map(KindFigure::Money),
                basis_fee_exact.map(KindFigure::Exact),
                accrued_interest.map(KindFigure::Money),
            ]),
            confidence: Vec::new(),
        },
        // Six families whose whole claim is one sum. The unresolved own-account
        // movement is the one that posts no leg to repeat it.
        EventKind::CashIn { amount }
        | EventKind::CashOut { amount }
        | EventKind::Refund { amount }
        | EventKind::OwnAccountMovement { amount }
        | EventKind::UnresolvedOwnAccountMovement { amount }
        | EventKind::OpeningCash { amount } => KindAspects {
            word: Vec::new(),
            moved: vec![KindFigure::Money(*amount)],
            confidence: Vec::new(),
        },
        // `transfer_id` is an identity rather than an aspect, and the two
        // accounts are already compared by `where_it_moved`: a transfer posts a
        // leg on each of them, and validation refuses one that does not.
        EventKind::CashTransfer {
            transfer_id: _,
            from: _,
            to: _,
            amount,
        } => KindAspects {
            word: Vec::new(),
            moved: vec![KindFigure::Money(*amount)],
            confidence: Vec::new(),
        },
        EventKind::Income {
            instrument,
            gross,
            kind,
        } => KindAspects {
            word: vec![KindWord::Income(*kind)],
            moved: figures([
                instrument.map(KindFigure::Instrument),
                Some(KindFigure::Money(*gross)),
            ]),
            confidence: Vec::new(),
        },
        EventKind::Fee { amount, origin } => KindAspects {
            word: vec![KindWord::Fee(*origin)],
            moved: vec![KindFigure::Money(*amount)],
            confidence: Vec::new(),
        },
        EventKind::Tax { amount, origin } => KindAspects {
            word: vec![KindWord::Tax(*origin)],
            moved: vec![KindFigure::Money(*amount)],
            confidence: Vec::new(),
        },
        EventKind::OpeningPosition {
            instrument,
            quantity,
            cost_basis,
            assertions,
        } => KindAspects {
            word: Vec::new(),
            moved: figures([
                Some(KindFigure::Instrument(*instrument)),
                Some(KindFigure::Quantity(*quantity)),
                cost_basis.map(KindFigure::Money),
            ]),
            confidence: vec![KindConfidence::Opening(*assertions)],
        },
        // **A valuation's price goes with `Amount`.** It is the one figure the
        // fact states — a valuation posts no leg — and what a corrected price
        // makes different is the number, not the sort of thing the fact is;
        // calling it `Kind` would tell the owner his valuation became some other
        // family of fact. It is carried as a per-unit amount and not as money,
        // because that is what it is: a price per security, not a posted sum,
        // and it is published nowhere as one. Its `quality` says how the price
        // was obtained, which is how sure the fact is of it, so that goes with
        // `Confidence`.
        EventKind::Valuation {
            instrument,
            price,
            currency,
            quality,
        } => KindAspects {
            word: Vec::new(),
            moved: vec![
                KindFigure::Instrument(*instrument),
                KindFigure::Price(PerUnitAmount::new(*price, *currency)),
            ],
            confidence: vec![KindConfidence::Price(*quality)],
        },
        // Four families whose payload is compared **whole**, under `Amount`.
        //
        // A corporate action and an offer exercise each carry a typed family of
        // their own, and everything in one of them describes the movement it
        // made; picking figures out of it would mean a second exhaustive match
        // over a family that grows, for a distinction the owner does not draw
        // reading one line of his history. A control assertion and a coverage
        // gap state nothing about an operation at all, and neither can ever be
        // written as a replacement — those are built from a submitted operation
        // — so the only way one of them faces another fact here is across a
        // change of family, which the discriminant already names.
        //
        // `..` elides nothing that could go unnoticed, and only here: what is
        // compared is the payload entire, so a field added to one of these is
        // compared the day it is added.
        EventKind::CorporateAction { .. }
        | EventKind::OfferExercise { .. }
        | EventKind::ControlAssertion { .. }
        | EventKind::ImportCoverageGap { .. } => KindAspects {
            word: Vec::new(),
            moved: vec![KindFigure::Whole(kind.clone())],
            confidence: Vec::new(),
        },
    }
}

/// The figures that are there, of the places one could be.
fn figures<const N: usize>(places: [Option<KindFigure>; N]) -> Vec<KindFigure> {
    places.into_iter().flatten().collect()
}

/// The money a fact states about itself when it posts nothing.
///
/// A fact with legs says its money in them and the view publishes those; a fact
/// with none says it only in its kind, and without this the whole of what such a
/// fact says would go unpublished. That is the case [`JournalEventView::amount`]
/// exists for, and the condition is the honest one: not a list of families, but
/// «this fact posted nothing, so read what it claims».
///
/// Exactly one figure or none. «The amount» of a legless fact that stated two
/// would be a choice nobody asked for, and choosing between them here would be
/// the arithmetic this route does not do.
fn stated_amount(event: &Event) -> Option<Money> {
    if !event.legs.is_empty() {
        return None;
    }
    let stated: Vec<Money> = kind_aspects(&event.kind)
        .moved
        .into_iter()
        .filter_map(|figure| match figure {
            KindFigure::Money(money) => Some(money),
            KindFigure::Exact(_)
            | KindFigure::Quantity(_)
            | KindFigure::Instrument(_)
            | KindFigure::Price(_)
            | KindFigure::Whole(_) => None,
        })
        .collect();
    match stated.as_slice() {
        [only] => Some(*only),
        _ => None,
    }
}

/// The basis-only fee a trade states, where one was recorded.
///
/// It is not a leg and therefore cannot be recovered from `legs`; publishing
/// it separately keeps a basis-fee-only correction visible without inventing a
/// total for the trade.
fn stated_basis_fee(event: &Event) -> Option<Money> {
    match event.kind {
        EventKind::Trade { basis_fee, .. } => basis_fee,
        _ => None,
    }
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

/// `category` and `uncategorised` ask opposite questions — a category and the
/// absence of one — and combining them is a request nobody could mean:
/// naming a category to look for while also asking for rows with none.
fn category_filter(category: Option<CategoryId>, uncategorised: bool) -> Result<(), AppError> {
    if category.is_some() && uncategorised {
        return Err(AppError::Invalid {
            field: "uncategorised".to_owned(),
            expected: "not combined with category".to_owned(),
            actual: "true".to_owned(),
        });
    }
    Ok(())
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
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    };

    use iaam_core::dates::{CashPostedDate, EffectiveOrder};
    use iaam_core::event::kind::{EventKind, TradeSide};
    use iaam_core::event::provenance::{ParserVersion, Provenance, RawHash};
    use iaam_core::money::{CurrencyCode, PostedMinor};
    use iaam_core::reconciliation::evidence::IdentityScope;
    use iaam_store::SqliteStore;
    use time::macros::date;

    use super::*;
    use crate::AppServices;
    use crate::adapters::sqlite::SqliteAdapter;
    use crate::ports::{AccountView, Clock, JournalStore, Principal, Scope};
    use crate::scenarios::correction::{
        CorrectionRequest, ImportTarget, correct_events, correct_import,
    };
    use iaam_core::event::correction::resolve_with_supersession;

    struct CountingJournalStore<'a> {
        inner: &'a dyn Store,
        event_limits: Arc<Mutex<Vec<u32>>>,
        relation_calls: Arc<AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl JournalStore for CountingJournalStore<'_> {
        async fn list_journal_events(
            &self,
            owner: OwnerId,
            query: JournalQuery,
        ) -> Result<Vec<Event>, AppError> {
            self.event_limits
                .lock()
                .expect("event limit recorder")
                .push(query.limit);
            self.inner.list_journal_events(owner, query).await
        }

        async fn list_journal_event_relations(
            &self,
            owner: OwnerId,
        ) -> Result<Vec<(EventId, Relation)>, AppError> {
            self.relation_calls.fetch_add(1, Ordering::SeqCst);
            self.inner.list_journal_event_relations(owner).await
        }
    }

    #[test]
    fn an_absent_page_size_is_the_default_and_zero_is_refused() {
        // Zero is not "no limit": a query with LIMIT 0 returns nothing, and a
        // caller would read an empty journal instead of a refusal.
        assert_eq!(page_size(None).expect("default"), DEFAULT_PAGE_SIZE);
        assert_eq!(page_size(Some(1)).expect("one row"), 1);
        assert!(page_size(Some(0)).is_err());
        assert_eq!(MAX_PAGE_SIZE, 1_000);
        assert!(page_size(Some(MAX_PAGE_SIZE)).is_ok());
        assert!(page_size(Some(MAX_PAGE_SIZE + 1)).is_err());
    }

    #[tokio::test]
    async fn reading_a_page_uses_payloads_once_and_relations_without_payloads() {
        let ctx = Ctx::new();
        let event_limits = Arc::new(Mutex::new(Vec::new()));
        let relation_calls = Arc::new(AtomicUsize::new(0));
        let store = CountingJournalStore {
            inner: ctx.store(),
            event_limits: Arc::clone(&event_limits),
            relation_calls: Arc::clone(&relation_calls),
        };

        read_journal(
            &store,
            ctx.owner,
            JournalReadQuery {
                limit: Some(MAX_PAGE_SIZE),
                ..JournalReadQuery::default()
            },
        )
        .await
        .expect("page");

        assert_eq!(
            *event_limits.lock().expect("event limit recorder"),
            vec![MAX_PAGE_SIZE + 1]
        );
        assert_eq!(relation_calls.load(Ordering::SeqCst), 1);
        assert!(
            event_limits
                .lock()
                .expect("event limit recorder")
                .iter()
                .all(|limit| *limit != u32::MAX),
            "reading a page must not issue an unbounded payload read"
        );
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

        /// Registers `main` and `savings` as the owner's own accounts.
        ///
        /// The write path now checks that every account an event names
        /// belongs to the event's owner, so any test that writes a fact
        /// must register the account first.
        async fn register_accounts(&self) {
            for (account, title) in [(self.main, "Main"), (self.savings, "Savings")] {
                self.services
                    .store
                    .upsert_account(
                        self.owner,
                        AccountView {
                            id: account,
                            title: title.to_owned(),
                            institution: None,
                        },
                    )
                    .await
                    .expect("account registered");
            }
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

        fn trade(
            &self,
            hash: u8,
            basis_fee_minor: i64,
            relation: Relation,
            instrument: InstrumentId,
        ) -> iaam_core::event::Event {
            let mut event = self.deposit(hash, -10_000, self.main);
            let gross = Money::new(PostedMinor::new(10_000), CurrencyCode::Rub);
            let basis_fee = Money::new(PostedMinor::new(basis_fee_minor), CurrencyCode::Rub);
            event.kind = EventKind::Trade {
                side: TradeSide::Buy,
                instrument,
                quantity: Quantity::zero(),
                gross,
                fee: None,
                basis_fee: Some(basis_fee),
                basis_fee_exact: None,
                accrued_interest: None,
            };
            event.relation = relation;
            event
        }

        /// Registers an instrument a trade fixture names: `event_trade.instrument`
        /// is a real foreign key into `instruments` now.
        async fn register_instrument(&self, instrument: InstrumentId) {
            self.services
                .directory
                .record_instrument(crate::ports::InstrumentUpsert {
                    id: instrument,
                    kind: Some(iaam_core::instrument::InstrumentKind::Share),
                    symbol: "TESTSHARE".to_owned(),
                    title: "Test Share".to_owned(),
                    currencies: iaam_core::instrument::CurrencyRoles::uniform(CurrencyCode::Rub),
                    lineage: None,
                })
                .await
                .expect("instrument registered");
        }

        /// A movement between two accounts of his that the source asserted and
        /// did not name, with the direction it never stated.
        ///
        /// **It posts no leg at all**, and cannot: the journal will not debit
        /// or credit an account on a movement whose direction nobody gave. So
        /// the whole of what this fact says is in its kind, which is exactly
        /// what makes it the sharpest case for a history to get right.
        fn unstated(&self, hash: u8, minor: i64) -> iaam_core::event::Event {
            let amount = Money::new(PostedMinor::new(minor), CurrencyCode::Rub);
            iaam_core::event::Event {
                kind: EventKind::UnresolvedOwnAccountMovement { amount },
                legs: Vec::new(),
                ..self.recorded(hash, minor, self.main, None)
            }
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
        ctx.register_accounts().await;
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
        ctx.register_accounts().await;
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
        ctx.register_accounts().await;
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

    #[tokio::test]
    async fn a_second_reversal_of_one_fact_is_visible_in_history() {
        let ctx = Ctx::new();
        ctx.register_accounts().await;
        let original = ctx.deposit(1, 4_500, ctx.main);
        let first_reversal = iaam_core::event::Event {
            relation: Relation::Reversal {
                target: original.id,
            },
            ..ctx.deposit(2, 4_500, ctx.main)
        };
        let second_reversal = iaam_core::event::Event {
            relation: Relation::Reversal {
                target: original.id,
            },
            ..ctx.deposit(3, 4_500, ctx.main)
        };
        ctx.write(&[
            original.clone(),
            first_reversal.clone(),
            second_reversal.clone(),
        ])
        .await;

        let history = ctx.history(original.id).await;

        assert_eq!(
            history
                .steps
                .iter()
                .map(|step| step.act)
                .collect::<Vec<_>>(),
            vec![
                HistoryAct::Arrived,
                HistoryAct::Retracted,
                HistoryAct::Retracted
            ]
        );
        assert_eq!(
            history
                .steps
                .iter()
                .filter_map(|step| step.reversal)
                .collect::<Vec<_>>(),
            vec![first_reversal.id, second_reversal.id]
        );
        assert_eq!(history.current, None);
    }

    #[tokio::test]
    async fn a_replacement_of_a_reversal_is_visible_after_the_retraction() {
        let ctx = Ctx::new();
        ctx.register_accounts().await;
        let original = ctx.deposit(1, 4_500, ctx.main);
        let reversal = iaam_core::event::Event {
            relation: Relation::Reversal {
                target: original.id,
            },
            ..ctx.deposit(2, 4_500, ctx.main)
        };
        let replacement = iaam_core::event::Event {
            relation: Relation::Replacement {
                target: reversal.id,
            },
            ..ctx.deposit(3, 5_200, ctx.savings)
        };
        ctx.write(&[original.clone(), reversal.clone(), replacement.clone()])
            .await;

        let history = ctx.history(original.id).await;

        assert_eq!(
            history
                .steps
                .iter()
                .map(|step| step.act)
                .collect::<Vec<_>>(),
            vec![
                HistoryAct::Arrived,
                HistoryAct::Retracted,
                HistoryAct::Corrected
            ]
        );
        assert_eq!(
            history
                .steps
                .iter()
                .filter_map(|step| step.state.as_ref().map(|state| state.event))
                .collect::<Vec<_>>(),
            vec![original.id, replacement.id]
        );
        assert_eq!(history.current, Some(replacement.id));
    }

    #[tokio::test]
    async fn a_trade_correction_that_only_changes_basis_fee_publishes_both_fees() {
        let ctx = Ctx::new();
        ctx.register_accounts().await;
        let instrument = InstrumentId::new_random();
        ctx.register_instrument(instrument).await;
        let original = ctx.trade(1, 100, Relation::None, instrument);
        let replacement = ctx.trade(
            2,
            200,
            Relation::Replacement {
                target: original.id,
            },
            instrument,
        );
        ctx.write(&[original.clone(), replacement.clone()]).await;

        let history = ctx.history(original.id).await;

        assert_eq!(history.steps[1].changed, vec![ChangedAspect::Amount]);
        assert_eq!(
            history
                .steps
                .iter()
                .filter_map(|step| step.state.as_ref().map(|state| state.basis_fee))
                .collect::<Vec<_>>(),
            vec![
                Some(Money::new(PostedMinor::new(100), CurrencyCode::Rub)),
                Some(Money::new(PostedMinor::new(200), CurrencyCode::Rub)),
            ]
        );
    }

    /// Most of his journal is this: a fact that arrived and was never touched.
    /// Nothing about it may be phrased as a change.
    #[tokio::test]
    async fn an_operation_nothing_ever_touched_is_one_arrival() {
        let ctx = Ctx::new();
        ctx.register_accounts().await;
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
        ctx.register_accounts().await;
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
            false,
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
    ///
    /// **Currently unwritable, and this is not a fixture bug — reported
    /// rather than worked around (iaam-c2fk).** `events.relation_target` is
    /// now `FOREIGN KEY (owner, relation_target) REFERENCES events (owner,
    /// id) DEFERRABLE INITIALLY DEFERRED` (schema collapse, T8,
    /// .internal/specs/2026-09-08-a-relational-journal-design.md). Deferred
    /// only postpones the check to commit; it still requires the target to
    /// exist by then, for *some* write in the same transaction. A `stranger`
    /// id that will never be written at all — this test's whole premise —
    /// can therefore no longer be recorded, by any writer, typed or raw:
    /// `PRAGMA foreign_keys = ON` applies uniformly. But the domain model
    /// still explicitly documents this as reachable and legitimate —
    /// [`HistoryAct::Arrived`]'s doc comment, [`read_operation_history`]'s
    /// "a target that was never written, one that belongs to another owner
    /// and one lost to a corrupt database are indistinguishable", and
    /// `iaam_core::event::correction::resolve_with_unheld_targets` all
    /// describe and handle exactly this case on the *read* side. The FK now
    /// forecloses ever producing it on the *write* side. That is a real
    /// conflict between the new schema and existing, tested domain
    /// semantics — not something a fixture can route around — so this test
    /// is disabled rather than weakened or deleted, pending a maintainer
    /// decision on which side is wrong.
    #[tokio::test]
    async fn a_fact_naming_a_target_outside_his_journal_is_where_his_history_begins() {
        let ctx = Ctx::new();
        ctx.register_accounts().await;
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
        ctx.register_accounts().await;
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

    /// A fact that posts nothing is still a fact, and correcting its sum is
    /// still a correction of the sum.
    ///
    /// Everything such a fact says lives in its kind, so a comparison that read
    /// only the legs would find two empty lists, name no aspect at all, and
    /// publish two states nobody could tell apart. The owner would be told that
    /// he corrected the row and never told what he corrected — the very failure
    /// [`ChangedAspect`] excludes a category loudly to avoid.
    #[tokio::test]
    async fn a_fact_that_posts_nothing_still_says_its_sum_changed() {
        let ctx = Ctx::new();
        ctx.register_accounts().await;
        let original = ctx.unstated(1, 250_000);
        let reversal = iaam_core::event::Event {
            relation: Relation::Reversal {
                target: original.id,
            },
            ..ctx.unstated(2, 250_000)
        };
        let replacement = iaam_core::event::Event {
            relation: Relation::Replacement {
                target: original.id,
            },
            ..ctx.unstated(3, 340_000)
        };
        ctx.write(&[original.clone(), reversal.clone(), replacement.clone()])
            .await;

        let history = ctx.history(original.id).await;

        assert_eq!(history.steps[1].act, HistoryAct::Corrected);
        assert_eq!(
            history.steps[1].changed,
            vec![ChangedAspect::Amount],
            "he re-stated the sum of a fact whose sum is the only thing it has"
        );

        // And naming the aspect is half an answer while the two states he is
        // shown are indistinguishable. The sum is on each of them.
        let sums: Vec<Option<Money>> = history
            .steps
            .iter()
            .filter_map(|step| step.state.as_ref().map(|state| state.amount))
            .collect();
        assert_eq!(
            sums,
            vec![
                Some(Money::new(PostedMinor::new(250_000), CurrencyCode::Rub)),
                Some(Money::new(PostedMinor::new(340_000), CurrencyCode::Rub)),
            ],
            "what it said before, and what it says now"
        );
    }

    /// A fact that posts a leg says its money there, and the view publishes the
    /// leg. Repeating it beside the leg would give one number two places to be
    /// read from, and the first reader to find them disagreeing would be right
    /// to distrust both.
    #[tokio::test]
    async fn a_fact_that_posts_its_money_does_not_state_it_twice() {
        let ctx = Ctx::new();
        let posted = ctx.deposit(1, 4_500, ctx.main);
        let resolution = resolve_with_supersession(std::slice::from_ref(&posted)).unwrap();

        let view = journal_event_view(&posted, resolution.supersession());
        assert_eq!(view.legs.len(), 1, "the deposit posts its money");
        assert_eq!(view.amount, None, "and states it nowhere else");
    }

    /// Coupon corrected to dividend: the family word is `income` before and
    /// after, so a comparison that read only the word would find nothing. What
    /// he changed is what sort of thing the fact is, which is `kind`.
    #[tokio::test]
    async fn the_sort_of_an_income_is_a_change_of_kind_and_not_of_sum() {
        let ctx = Ctx::new();
        let gross = Money::new(PostedMinor::new(4_500), CurrencyCode::Rub);
        let coupon = iaam_core::event::Event {
            kind: EventKind::Income {
                instrument: None,
                gross,
                kind: Some(IncomeKind::Coupon),
            },
            ..ctx.deposit(1, 4_500, ctx.main)
        };
        let dividend = iaam_core::event::Event {
            kind: EventKind::Income {
                instrument: None,
                gross,
                kind: Some(IncomeKind::Dividend),
            },
            ..ctx.deposit(2, 4_500, ctx.main)
        };

        assert_eq!(coupon.kind.discriminant(), dividend.kind.discriminant());
        assert_eq!(
            changed_aspects(&coupon, &dividend),
            vec![ChangedAspect::Kind],
            "the sum did not move, and the sort of the income did"
        );
    }

    #[test]
    fn a_source_description_change_is_named_as_source_description() {
        let ctx = Ctx::new();
        let before = ctx.deposit(1, 4_500, ctx.main);
        let mut after = before.clone();
        after.provenance = after.provenance.clone().with_description("Other Shop");

        assert_eq!(
            changed_aspects(&before, &after),
            vec![ChangedAspect::SourceDescription]
        );
    }

    /// The same for the two other scalars a family is told apart by: what a fee
    /// was for, and whether a tax was withheld or paid.
    #[tokio::test]
    async fn what_a_fee_was_for_and_who_paid_a_tax_are_changes_of_kind() {
        let ctx = Ctx::new();
        let amount = Money::new(PostedMinor::new(-1_200), CurrencyCode::Rub);
        let fee = |origin| iaam_core::event::Event {
            kind: EventKind::Fee { amount, origin },
            ..ctx.deposit(1, -1_200, ctx.main)
        };
        let tax = |origin| iaam_core::event::Event {
            kind: EventKind::Tax { amount, origin },
            ..ctx.deposit(2, -1_200, ctx.main)
        };

        assert_eq!(
            changed_aspects(&fee(FeeOrigin::Brokerage), &fee(FeeOrigin::Depositary)),
            vec![ChangedAspect::Kind],
        );
        assert_eq!(
            changed_aspects(&tax(TaxOrigin::WithheldAtSource), &tax(TaxOrigin::SelfPaid)),
            vec![ChangedAspect::Kind],
        );
    }
}
