//! Classification rules and history recalculation (§10.4).
//!
//! **Classification is not an event field.** An event carries a fact —
//! `CashTransfer` with both accounts — while “inside or outside the perimeter”
//! is determined by the perimeter classifier (§4.10). Rules are needed where the
//! **type** of operation itself cannot be inferred from the data: a transfer to oneself versus
//! a transfer to a third party, a fee versus a withdrawal, income versus
//! a refund.
//!
//! Therefore, it is not the constructed operation that is classified, but the row's attributes,
//! visible **before** choosing the type: the counterparty account, the payment purpose,
//! the word the source used to name the operation, and the category the source
//! filed the row under.
//!
//! **History recalculation means new facts, not editing old ones.** Editing a
//! rule produces a plan of reversal and replacement; the journal remains
//! append-only (§4.8). The [`Correction`] type cannot express a change to an
//! event — this guarantees the form, not the caller's discipline.

use std::collections::BTreeMap;

use iaam_core::event::Event;
use iaam_core::event::correction::{CorrectionError, resolve};
use iaam_core::event::kind::{EventKind, FeeOrigin, IncomeKind};
use iaam_core::ids::{AccountId, ClassificationRuleId, EventId};
use serde::{Deserialize, Serialize};

/// Where the money is moving. Needed so that the question asked of the owner is relevant:
/// debits and credits have different branches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Movement {
    In,
    Out,
}

/// Who is on the other side.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Counterparty {
    /// The owner's account, identified by the directory.
    OwnAccount(AccountId),
    /// The account is named but not identified: a details line from the report.
    Named(String),
    /// No party is named at all.
    Unknown,
}

/// What the source said about **whose** account is on the far side.
///
/// Two values and neither is a default. This is not a direction: the claim says
/// nothing about which way the money ran, and a source that makes it commonly
/// says nothing about direction at all — that pairing is what
/// `iaam-cp94` was filed about. Nor is it a counterparty: there is no name in
/// it, which is exactly why it needed a place of its own.
///
/// **Why a field beside [`Counterparty`] and not a variant of it.** The two
/// answer independent questions — «who» and «whose» — and a source can answer
/// either without the other. A `Counterparty::OwnAccountUnidentified` variant
/// would make them exclusive, so a row that both asserts the far side is the
/// owner's *and* prints a string for it would have to drop one of them; the
/// string is what [`RuleMatcher`] matches on and what the directory resolves,
/// so dropping it would throw away the only thing that could name which
/// account. Read [`Counterparty::OwnAccount`] beside this: that variant is a
/// **conclusion the directory reached**, and this is a **claim the source
/// made**, and the first is strictly stronger because it names an account.
///
/// **Why not a `bool`.** [`Self::Unstated`] means the source said nothing about
/// the far side, which is not the same as saying the far side is somebody
/// else's — §4.9's distinction, and the one a `false` would quietly erase. No
/// source states the negative, so there is deliberately no third value for it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FarSide {
    /// The source said nothing about whose account is on the other side.
    #[default]
    Unstated,
    /// The source asserted the other side is one of the owner's own accounts,
    /// and did not say which one.
    OwnAccount,
}

impl FarSide {
    /// Parse the word a caller sent.
    ///
    /// A rejection rather than a fallback to [`Self::Unstated`], for
    /// `ObservedDirection::parse`'s reason: a caller that meant to relay the
    /// source's assertion and misspelt it must be told, not silently read as
    /// having relayed nothing.
    pub fn parse(value: &str) -> Result<Self, crate::verdict::Rejection> {
        match value {
            "unstated" => Ok(Self::Unstated),
            "own_account" => Ok(Self::OwnAccount),
            other => Err(crate::verdict::Rejection {
                field: "far_side".to_owned(),
                expected: "own_account or unstated".to_owned(),
                actual: other.to_owned(),
            }),
        }
    }

    /// Wire code. One place, so the transport cannot spell it differently.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Unstated => "unstated",
            Self::OwnAccount => "own_account",
        }
    }

    /// Whether the source asserted the far side is the owner's.
    #[must_use]
    pub const fn is_own_account(self) -> bool {
        matches!(self, Self::OwnAccount)
    }
}

/// Row attributes used to determine the operation type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassificationSubject {
    pub account: AccountId,
    pub counterparty: Counterparty,
    pub description: Option<String>,
    /// What the source called the operation. An open set: each
    /// broker uses its own words, so this is a string, not an enum.
    pub source_kind: Option<String>,
    /// What the source filed the row under, verbatim.
    ///
    /// The source's word for what the movement was **for**, where
    /// [`Self::source_kind`] above it is the source's word for what the
    /// movement **was**. Two facts, two fields, and never one slot — that is
    /// decision 0020 §2, and the state before it is what made a category rule
    /// written on a category match an operation word instead.
    ///
    /// **The same string `iaam_core::category::CategoryMatcher::SourceCategory`
    /// matches**, read out of the same place: `Provenance::source_category` for
    /// a fact already recorded, [`crate::observation::ObservedRow::source_category`]
    /// for a row still being read. The two rule vocabularies ask different
    /// questions of it — a category rule asks which of the owner's categories
    /// the row belongs in, a [`RuleMatcher`] asks what the row *is* — and the
    /// field they ask it of is one field with one meaning. It is matched
    /// exactly in both, because it is a value out of a controlled vocabulary
    /// one source prints and not prose.
    ///
    /// **Evidence, never a decision.** Nothing concludes anything from the word
    /// on its own: a profile transcribes it and stops (decision 0019 §6), and
    /// the only thing that turns it into a classification is a rule the owner
    /// wrote — which he can read back, retire, and replan.
    pub source_category: Option<String>,
    /// The category the **owner himself** filed the row under, at the source,
    /// verbatim.
    ///
    /// A different fact from [`Self::source_category`] above it, and the
    /// difference is whose decision it is. That one is what the institution
    /// filed the row under; this one is what the owner filed it under, in the
    /// institution's own app, and it is the answer he was being asked for once
    /// per row while nothing read the column.
    ///
    /// **Evidence and never a conclusion**, which is the rule most at risk on
    /// this field because the value looks like a conclusion. It is his decision
    /// in his *bank's* vocabulary, not in his categories here, and nothing maps
    /// it: what it is called here is one question per distinct value, asked
    /// once, whose answer reaches every row carrying that value.
    pub owner_category: Option<String>,
    /// The standardised code the source printed for the row, verbatim.
    ///
    /// The one word on the row that is not one institution's private
    /// vocabulary, so a rule written on it holds across institutions. Empty on
    /// rows the source assigns no code to, and `None` is what such a row says.
    pub source_code: Option<String>,
    /// Which way the money went, when the source said.
    ///
    /// `None` is not a default and not a missing field: it means the source
    /// stated an amount and no direction — a bank row printed as "internal to
    /// this institution" and nothing else. The two rows this distinguishes look
    /// identical once a direction has been supplied, and supplying one is the
    /// guess this type exists to refuse.
    pub movement: Option<Movement>,
    /// What the source said about whose account is on the far side.
    ///
    /// Beside `counterparty` rather than inside it, and beside `movement`
    /// rather than inside that: see [`FarSide`]. It is read after the rules,
    /// not before them, because a rule the owner wrote about this counterparty
    /// is a stronger statement than a word the source printed.
    pub far_side: FarSide,
}

/// Rule condition. Specified fields are joined with “and”.
///
/// **This is the vocabulary of «what the row is», and it is not the vocabulary
/// of «which category the row is filed under».** The other one is
/// `iaam_core::category::CategoryMatcher`, and the two must never come to mean
/// different things by one word. Where they name the same field they read the
/// same string out of the same place and compare it the same way: [`Self::kind`]
/// and [`Self::source_category`] are both exact comparisons against a
/// vocabulary one source controls, exactly as `CategoryMatcher::SourceCategory`
/// is. What differs is the question asked — *is this a fee?* against *is this
/// groceries?* — and what a matching rule then decides, which is the whole
/// reason there are two types and not one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleMatcher {
    pub counterparty_account: Option<String>,
    pub description_contains: Option<String>,
    /// The word the source used for the operation, matched exactly, against
    /// [`ClassificationSubject::source_kind`].
    ///
    /// Named `kind` rather than `source_kind` because that is what the field has
    /// been called in every stored rule and every published body since the type
    /// existed; renaming it would make every rule the owner has written
    /// unreadable to gain a consistency the doc comment can state instead.
    pub kind: Option<String>,
    /// The category the source filed the row under, matched exactly, against
    /// [`ClassificationSubject::source_category`].
    ///
    /// **The fourth arm, and the one the channel that ships needs most**
    /// (`iaam-93lz`). Decision 0019 §6 has a profile transcribe the source's own
    /// category and never map it, on the ground that the owner's rules do the
    /// mapping — they are his, editable, and re-runnable over rows already
    /// recorded, where a map baked into a profile is frozen into every fact at
    /// import. That argument needs the owner's rules to be able to *read* the
    /// field, and this vocabulary could not: the first profile prints no
    /// operation-type word at all, so [`Self::kind`] is `None` on every one of
    /// its rows and «a row the institution filed under this category is
    /// interest» could not be written as a standing rule at all.
    ///
    /// **Not scoped to a source, and that is argued rather than skipped** —
    /// decision 0026. The journal holds no handle that names an institution:
    /// `SourceId` is derived from owner, account and channel, so a rule scoped
    /// by it would have to be rewritten for every account the same bank's
    /// exports arrive on, and a `ParserVersion` names a profile *and its
    /// version*, so a rule scoped by it would stop firing the day the profile is
    /// corrected. The bound on a rule that is too wide is the one every other
    /// arm of this matcher already relies on: the fields join with «and», so a
    /// category condition can be narrowed by a counterparty or a description
    /// beside it, and a rule that fires wrongly is one the owner retires, which
    /// replans what it classified.
    pub source_category: Option<String>,
    /// The category the **owner himself** filed the row under at the source,
    /// matched exactly, against [`ClassificationSubject::owner_category`].
    ///
    /// **The strongest evidence a statement carries about what a row was, and
    /// it is the owner's own.** Where [`Self::source_category`] beside it is a
    /// word the institution files by for its own purposes — a transfer word
    /// covering every transfer, inward and outward — this is a decision he took
    /// himself and the export prints back. A rule written on it says «rows I
    /// filed under this word at my bank are this», which is a claim about his
    /// own filing and not about the bank's.
    ///
    /// **Still a condition and never a mapping.** Nothing concludes from the
    /// word on its own: the profile transcribes it and stops (decision 0028),
    /// and the only thing that turns it into a classification is a rule he
    /// wrote — which he can read back, retire and replan. That is the whole
    /// difference between one question per distinct value and a map frozen into
    /// every fact at import.
    pub owner_category: Option<String>,
    /// The source's standardised code, matched exactly, against
    /// [`ClassificationSubject::source_code`].
    ///
    /// **The one arm of this matcher that is not scoped to a vocabulary one
    /// institution controls.** The code is assigned by the payment network, so
    /// a rule written on it holds across institutions, where a rule written on a
    /// source's own category holds for one bank until it renames something. It
    /// also generalises differently: as the ground for a rule it covers a whole
    /// kind of spending, where a description condition covers one merchant
    /// string.
    ///
    /// Matched as text, exactly, and never as a number: it is an identifier
    /// printed with leading zeros. A row the source printed no code for is
    /// matched by no code condition — the source said nothing, and a condition
    /// asking what it said is not answered.
    pub source_code: Option<String>,
}

impl RuleMatcher {
    /// A condition that asks about nothing matches nothing.
    ///
    /// An “everything” rule can only be created by mistake, and silently
    /// reclassifying the entire portfolio with it is forbidden.
    #[must_use]
    pub const fn asks_nothing(&self) -> bool {
        self.counterparty_account.is_none()
            && self.description_contains.is_none()
            && self.kind.is_none()
            && self.source_category.is_none()
            && self.owner_category.is_none()
            && self.source_code.is_none()
    }

    /// Whether the condition matches the row.
    #[must_use]
    pub fn matches(&self, subject: &ClassificationSubject) -> bool {
        if self.asks_nothing() {
            return false;
        }
        let by_counterparty = self.counterparty_account.as_deref().is_none_or(
            |wanted| matches!(&subject.counterparty, Counterparty::Named(name) if name == wanted),
        );
        // Brokers write payment purposes however they please: a rule
        // sensitive to case would stop working on the next
        // report from the same broker.
        let by_description = self.description_contains.as_deref().is_none_or(|wanted| {
            subject
                .description
                .as_deref()
                .is_some_and(|text| text.to_lowercase().contains(&wanted.to_lowercase()))
        });
        let by_kind = self
            .kind
            .as_deref()
            .is_none_or(|wanted| subject.source_kind.as_deref() == Some(wanted));
        // Exactly, and against the category field alone. A source's category is
        // a value out of a vocabulary that source controls, so a substring test
        // would match one word inside another the institution means differently,
        // and reading it from `source_kind` as well would put the two words back
        // in one slot — which is the defect decision 0020 §2 took them apart to
        // end.
        let by_source_category = self
            .source_category
            .as_deref()
            .is_none_or(|wanted| subject.source_category.as_deref() == Some(wanted));
        // Each against its own field and each exactly, for the reason above and
        // for one more: the owner's word and the source's word are two
        // decisions by two parties, and a condition that read either from one
        // slot would fire on rows he was not describing — which is the defect
        // decision 0020 §2 took `source_kind` and `source_category` apart to
        // end, arriving here a second time with a third and a fourth word.
        let by_owner_category = self
            .owner_category
            .as_deref()
            .is_none_or(|wanted| subject.owner_category.as_deref() == Some(wanted));
        let by_source_code = self
            .source_code
            .as_deref()
            .is_none_or(|wanted| subject.source_code.as_deref() == Some(wanted));
        by_counterparty
            && by_description
            && by_kind
            && by_source_category
            && by_owner_category
            && by_source_code
    }
}

/// What the operation turned out to be.
///
/// **This is the vocabulary a rule is written in, and a rule carries no
/// direction.** A rule fires on rows the owner has never seen; a direction
/// carried over from the row he wrote it on would be asserted about all of them.
/// So `InternalTransfer` names the account on the far side and says nothing
/// about which way the money ran — see [`Self::implied_movement`].
///
/// **A rule does carry what the row *is*, including how finely it is named.**
/// That is the line the direction rule does not cross and the reason
/// [`Self::Income`] holds an [`IncomeKind`]: "this is interest on a balance" is
/// a claim about every row the matcher matches, exactly as "this is a fee" is,
/// while "the money went out" is a fact about one row. A kind kept off this type
/// and put only on [`Answer`] would settle the row the owner looked at and be
/// dropped the moment his answer became a rule, which is how the observation
/// channel came to record every arrival as income of no stated kind while a
/// converter reading the same statement named one (`iaam-7l7v`).
///
/// The set is closed and the five members are the outcomes a **cash statement
/// row** can have. There is deliberately no `Tax`: [`classification_of`] answers
/// `None` for a recorded tax, so tax sits outside recalculation altogether, and
/// admitting it here would overturn that in passing rather than by decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Classification {
    /// A transfer between the owner's own accounts. `to` is the **far side** of
    /// the movement — the account that is not the row's own — whichever
    /// direction it went in.
    InternalTransfer {
        to: AccountId,
    },
    ExternalFlow,
    Fee {
        origin: FeeOrigin,
    },
    /// Money a counterparty returned, reversing an earlier outflow.
    ///
    /// Not [`Self::ExternalFlow`] with money arriving, which is what an
    /// observation resolved as a return used to become. The journal draws the
    /// distinction — `EventKind::Refund` is subtracted from what went out, in
    /// the category the money was spent in, while `CashIn` is money entering the
    /// perimeter — so a returned purchase recorded as a deposit overstates both
    /// what arrived and what was spent, by the same sum, in the same month.
    Refund,
    /// Money the account earned, and the kind of earning where one is named.
    ///
    /// `kind: None` means **no kind was stated**, in the sense §4.9 fixes for
    /// the same field on `EventKind::Income` and `OperationKind::Income`. It is
    /// not a wildcard: a rule written with `None` asserts that the rows it
    /// matches name no kind, and [`recompute_plan`] will accordingly propose
    /// correcting a coupon it matches down to income of no stated kind. That is
    /// the rule saying what it says, and the plan is proposed rather than
    /// applied, so the owner sees it before anything is written. Spelling
    /// "leave the kind alone" would need a second meaning for one value, and one
    /// spelling with two meanings is the defect §4.9 exists to prevent.
    Income {
        kind: Option<IncomeKind>,
    },
    /// A movement between the owner's own accounts whose far side is **not**
    /// named.
    ///
    /// The sixth outcome, and the weaker sibling of [`Self::InternalTransfer`]
    /// rather than a shade of [`Self::ExternalFlow`]. The two internal outcomes
    /// differ by one thing and it is the thing that decides the journal shape:
    /// `InternalTransfer` names the account on the far side, so the fact can be
    /// a complete `CashTransfer` carrying both; this one does not, so the fact
    /// carries one endpoint and says the other is the owner's and unnamed.
    ///
    /// It is in the rule vocabulary and not only on [`Answer`], for decision
    /// 0006's reason: «anything the source calls this is a movement between my
    /// own accounts» is a claim about every row a matcher matches, which is
    /// exactly what a rule is for, and it is the one standing decision that
    /// makes a statement full of such rows import without a question each.
    ///
    /// It carries no direction, and [`Self::implied_movement`] answers `None`
    /// for it. That is not a gap to be filled later: the journal has a shape
    /// for this movement **with** a direction and a shape for it **without**
    /// one, so a row that states no direction is recorded rather than asked
    /// about.
    OwnAccountMovement,
}

impl Classification {
    /// The direction this classification states on its own, when it states one.
    ///
    /// Three of the five do, and they do because the classification **is** the
    /// direction: a fee leaves the account, income arrives at it, and a refund
    /// is money coming back. Nothing has to be read off the row to know that,
    /// and answering `None` for them would send a settled row back to the owner
    /// as a question.
    ///
    /// `Refund` was the one that had to be argued rather than read off. Its
    /// direction looks like a property of the row — the sign the source printed,
    /// the direction word beside it — and the row does state one, which is why
    /// `movement_of` consults the row **before** this function. But that is true
    /// of income too, and the question here is a different one: what does the
    /// classification claim when the row states nothing? A refund that left the
    /// account is not an under-specified refund, it is not a fact this journal
    /// holds at all — `EventKind::Refund` carries a single positive cash leg —
    /// so `None` here would open a question with no admissible answer. The
    /// contract therefore stands unchanged, and the arm that refuses `Refund`
    /// leaving the account lives in `ObservedRow::resolve` beside the one that
    /// already refuses income leaving it.
    ///
    /// The other two answer `None`, and that is the whole point of the function
    /// existing. `ExternalFlow` never claimed one. `InternalTransfer` looks as
    /// though it does, because it names an account — and comparing that account
    /// with the row's own does yield an answer of the right *type*. It is still
    /// a guess: the account named is the far side, and both
    /// [`Answer::SentToOwnAccount`] and [`Answer::ReceivedFromOwnAccount`]
    /// record the far side here, so the comparison reads money that arrived as
    /// money that left. Deriving a direction from it was the defect this
    /// function was written to make unavailable.
    ///
    /// Which leaves exactly two ways a direction can enter: the source stated
    /// one ([`ClassificationSubject::movement`]) or the owner did
    /// ([`Answer::movement`]). Both are total over types that distinguish the
    /// two directions structurally. There is no third.
    #[must_use]
    pub const fn implied_movement(self) -> Option<Movement> {
        match self {
            Self::Fee { .. } => Some(Movement::Out),
            Self::Income { .. } | Self::Refund => Some(Movement::In),
            Self::InternalTransfer { .. } | Self::ExternalFlow | Self::OwnAccountMovement => None,
        }
    }
}

/// The owner's decision recorded by the rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassificationRule {
    pub id: ClassificationRuleId,
    pub version: u32,
    pub matcher: RuleMatcher,
    pub outcome: Classification,
}

impl ClassificationRule {
    /// The rule's wording.
    ///
    /// The rule must be visible: without wording, there is nothing with which to explain
    /// the previous classification (§10.4).
    #[must_use]
    pub fn describe(&self) -> String {
        let mut conditions = Vec::new();
        if let Some(account) = &self.matcher.counterparty_account {
            conditions.push(format!("counterparty account — {account}"));
        }
        if let Some(text) = &self.matcher.description_contains {
            conditions.push(format!("payment purpose contains «{text}»"));
        }
        if let Some(kind) = &self.matcher.kind {
            conditions.push(format!("source called the operation «{kind}»"));
        }
        if let Some(category) = &self.matcher.source_category {
            conditions.push(format!("source filed the row under «{category}»"));
        }
        if let Some(category) = &self.matcher.owner_category {
            conditions.push(format!("you filed the row under «{category}» there"));
        }
        if let Some(code) = &self.matcher.source_code {
            conditions.push(format!("source printed the code «{code}»"));
        }
        let conditions = if conditions.is_empty() {
            "no conditions, so the rule does not apply".to_owned()
        } else {
            conditions.join(" and ")
        };
        format!(
            "version {}: if {conditions}, then {}",
            self.version,
            describe_outcome(self.outcome)
        )
    }
}

/// The outcome in words, one static sentence per outcome.
///
/// The income kind is spelled out rather than appended, so the wording stays
/// static: a rule the owner reads back must say which earning it claims, because
/// "this is income" and "this is interest on a balance" are different decisions
/// and only one of them can be wrong about a coupon.
fn describe_outcome(outcome: Classification) -> &'static str {
    match outcome {
        Classification::InternalTransfer { .. } => "this is a transfer between own accounts",
        Classification::OwnAccountMovement => {
            "this is a movement between own accounts, and the source names no far side"
        }
        Classification::ExternalFlow => "this is movement outside the portfolio",
        Classification::Fee { .. } => "this is a fee",
        Classification::Refund => "this is money a counterparty returned",
        Classification::Income { kind: None } => "this is income",
        Classification::Income {
            kind: Some(IncomeKind::Coupon),
        } => "this is income: a coupon",
        Classification::Income {
            kind: Some(IncomeKind::Dividend),
        } => "this is income: a dividend",
        Classification::Income {
            kind: Some(IncomeKind::DepositInterest),
        } => "this is income: interest on a balance",
    }
}

/// Why the operation was classified this way.
///
/// **Three values and not two, and the third is the one a reader has to be able
/// to see** (`iaam-rdya`). [`Self::Derived`] and [`Self::Asserted`] were one
/// word while both meant «no rule was needed», and they are not one thing: the
/// first is a conclusion **this system** reached — the owner's directory
/// recognised the string the source printed and named one of his accounts — and
/// the second is a claim **the source** made about its own row, which nothing
/// here checked and nothing here can check. Asserting a far side is the
/// cheapest way for a profile to make questions disappear, so a plan that
/// spelt the two alike gave a reader no way to catch a profile that over-asserts.
///
/// There is deliberately no value for «the owner answered». An [`Answer`] is
/// not a classification result: it does not come out of [`classify`], it
/// reaches [`crate::observation::ObservedRow::resolve_with`] directly, and a
/// variant here would be a fourth spelling of a fact this type never sees.
/// Where a caller publishes how a row was settled it has to say that in its own
/// vocabulary, over the answer it holds and this basis together.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Basis {
    /// Derived from data: the row said what it was, and no rule was needed.
    ///
    /// The one case today is a counterparty the owner's directory recognised as
    /// one of his own accounts, which names the far side and settles the row.
    Derived,
    /// The source asserted the far side is one of the owner's accounts.
    ///
    /// Weaker than [`Self::Derived`] although it also needs no rule, and told
    /// apart from it for exactly that reason: no account is named, nothing was
    /// resolved, and the whole of the evidence is a word the document printed
    /// about itself. See [`FarSide`].
    Asserted,
    /// Owner's decision.
    Rule {
        rule: ClassificationRuleId,
        version: u32,
    },
}

/// Question for the owner.
///
/// An enumeration, not a string: the question is sent to the API and rendered
/// with human-readable account names, which the pure function does not have,
/// and a string containing a UUID is not a specific question.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "question", rename_all = "snake_case")]
pub enum Question {
    /// Recipient account named but not recognized.
    IsTransferInternal {
        account: AccountId,
        counterparty: String,
    },
    /// Debit without a named counterparty: fee or withdrawal?
    IsOutflowAFee { account: AccountId },
    /// Receipt without a named counterparty: income, a refund, or money in?
    ///
    /// The three are not shades of one another. Income is money the capital
    /// earned, a refund is an earlier outflow coming back, and `Received` is
    /// money arriving from outside that is neither. The reports separate all
    /// three, so answering one for another is not a wording choice.
    IsInflowIncome { account: AccountId },
    /// The source gave an amount and no direction.
    ///
    /// **This one is not a yes/no, unlike its three siblings**, and a reader who
    /// assumes it is will build the wrong answer form. The other three are asked
    /// about a row whose direction is already settled — two of them branch on
    /// [`Movement`] to be asked at all. This one is asked when nothing has
    /// settled the direction: a bank printed a word meaning "internal to this
    /// institution" with an amount beside it, and neither which way the money
    /// went nor the account on the other side can be read out of that.
    ///
    /// Answering it therefore names a direction **and** a classification at
    /// once, which is what [`Answer`] is shaped for. Splitting it into "which
    /// way?" followed by "and what was it?" would put the owner in a chain of
    /// questions where one has to be asked before the next can be phrased, and
    /// the first answer would sit somewhere provisional while the second was
    /// pending.
    UnresolvedDirection {
        account: AccountId,
        /// The word the source used for the operation, verbatim, when it used
        /// one — [`ClassificationSubject::source_kind`]. `INNER` is the one this
        /// variant was written for; it is what a bank prints where another bank
        /// prints "debit" or "credit", and it is retained rather than
        /// interpreted. `None` means the source named the operation nothing at
        /// all, which is a weaker statement, not the same one.
        stated: Option<String>,
        /// The party the source named, when it named one. A named counterparty
        /// that the directory did not recognise still narrows the question.
        counterparty: Option<String>,
    },
}

impl Question {
    /// The account the question is about.
    #[must_use]
    pub const fn account(&self) -> AccountId {
        match self {
            Self::IsTransferInternal { account, .. }
            | Self::IsOutflowAFee { account }
            | Self::IsInflowIncome { account }
            | Self::UnresolvedDirection { account, .. } => *account,
        }
    }

    /// The answers that answer **this** question.
    ///
    /// Published with the question rather than assumed by the caller: an answer
    /// the question does not admit is a different mistake from an answer that is
    /// wrong, and only the first can be refused.
    ///
    /// [`Answer::SentToOwnAccount`] and [`Answer::ReceivedFromOwnAccount`] name
    /// an account the question cannot know, so they appear here in their
    /// `AnswerShape` form: the alternative says *an account is required*, and
    /// the answering call supplies it.
    ///
    /// **[`AnswerShape::Refund`] is offered by three of the four and refused by
    /// [`Self::IsOutflowAFee`]**, and the split is by what each question leaves
    /// open rather than by which of them mentions returns. `IsOutflowAFee` is
    /// the one question both of whose alternatives run the same way — a fee and
    /// a payment out both leave the account — so it is asked only where the
    /// direction is settled outward, and every answer it admits agrees with
    /// that. The other three already publish alternatives pointing both ways:
    /// `IsTransferInternal` offers `paid` beside `received` although the row
    /// stated a direction, because an answer states its own and the owner is
    /// entitled to contradict the source. A refund arriving is admissible
    /// wherever `received` is, and the case it exists for — a merchant the
    /// directory does not recognise, printed by name beside a positive amount —
    /// is `IsTransferInternal`'s, not `IsInflowIncome`'s.
    ///
    /// **[`AnswerShape::BetweenOwnAccounts`] is offered exactly where the two
    /// answers that name an own account are**, and the rule is that one and not
    /// a list of variants: a question admitting «money I moved to my own
    /// account» is a question about a row that could be the near half of a
    /// movement between two of them, which is the row he may know this much
    /// about and no more. So `IsTransferInternal` and `UnresolvedDirection`
    /// offer it and the other two do not — an outflow that is either a fee or a
    /// payment, and an inflow that is either an earning or a receipt, are asked
    /// about rows for which no own-account word is admissible at all, and
    /// adding this one there would offer an answer to a question nobody put.
    #[must_use]
    pub fn alternatives(&self) -> Vec<AnswerShape> {
        match self {
            Self::IsTransferInternal { .. } => vec![
                AnswerShape::SentToOwnAccount,
                AnswerShape::ReceivedFromOwnAccount,
                AnswerShape::BetweenOwnAccounts,
                AnswerShape::Paid,
                AnswerShape::Received,
                AnswerShape::Refund,
            ],
            Self::IsOutflowAFee { .. } => vec![AnswerShape::Fee, AnswerShape::Paid],
            Self::IsInflowIncome { .. } => vec![
                AnswerShape::Income,
                AnswerShape::Received,
                AnswerShape::Refund,
            ],
            Self::UnresolvedDirection { .. } => vec![
                AnswerShape::SentToOwnAccount,
                AnswerShape::ReceivedFromOwnAccount,
                AnswerShape::BetweenOwnAccounts,
                AnswerShape::Paid,
                AnswerShape::Received,
                AnswerShape::Fee,
                AnswerShape::Income,
                AnswerShape::Refund,
            ],
        }
    }

    /// Whether this question admits that answer.
    #[must_use]
    pub fn admits(&self, answer: &Answer) -> bool {
        self.alternatives().contains(&answer.shape())
    }

    /// The decision this question puts, paired with what the source said about
    /// the row's direction.
    ///
    /// `stated` is [`ClassificationSubject::movement`] for the row that raised
    /// it — what the **source** said, never what an answer would say. See
    /// [`QuestionSubject`] for why the pair and not the question alone, and for
    /// why a caller that has the row is the only one who can build this.
    #[must_use]
    pub fn about(&self, stated: Option<Movement>) -> QuestionSubject {
        QuestionSubject {
            question: self.clone(),
            stated,
        }
    }
}

/// What a question is a question **about**, so that the same decision put twice
/// is recognisable as one decision.
///
/// **Not [`Question`] itself, and the ingredient it is missing is the direction
/// the source stated** (`iaam-q5og`). Two rows naming one counterparty raise
/// equal [`Question::IsTransferInternal`] values whichever way each of them
/// ran: [`question_for`] builds that variant for `(Named(_), Some(_))` without
/// reading which `Some` it was, deliberately, because the owner may contradict
/// the source and the alternatives point both ways. An [`Answer`] then states a
/// direction of its own and `ObservedRow::resolve_with` records **that** one —
/// so one answer carried across two equal questions would record an arrival as
/// a departure, silently, in the one figure a money-flow report is read for.
/// The stated direction is therefore part of the identity, and two rows the
/// source disagreed about are two decisions.
///
/// The other three variants need no such ingredient, and are not given one for
/// symmetry: [`Question::IsOutflowAFee`] and [`Question::IsInflowIncome`] are
/// each asked only where the direction is settled, and settled the opposite
/// way, so they already differ by variant; [`Question::UnresolvedDirection`] is
/// asked only where the source stated none, so the field is `None` on every row
/// that raises it. The field is carried regardless rather than matched on per
/// variant, because a per-variant rule is a claim about which questions exist
/// and this type should not have to be revisited when a fifth one does.
///
/// **This is not a rule and says nothing about next month.** Equality here
/// settles rows already fed to one session, which the owner reads in that
/// session's assessment before anything is committed and can abandon whole. A
/// [`ClassificationRule`] classifies rows nobody has looked at, including rows
/// from months not yet imported. Decision 0029 keeps the two apart, and
/// `may_generalise` is untouched by it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuestionSubject {
    question: Question,
    stated: Option<Movement>,
}

impl QuestionSubject {
    /// The question every row with this subject raised.
    ///
    /// One question and not one per row, which is the point: a caller putting
    /// the decision to the owner once shows him this, and the rows are what it
    /// covers. The row-by-row wording lives on the stored prompt, because that
    /// one names its row (decision 0012) and this one names none.
    #[must_use]
    pub const fn question(&self) -> &Question {
        &self.question
    }

    /// What the source said about the direction of the rows this covers.
    ///
    /// `None` means the source stated none — which is a statement about the
    /// rows, not a wildcard: a subject with `None` is unequal to one carrying a
    /// direction, so an answer given for a directionless row does not reach a
    /// row whose direction the source printed.
    #[must_use]
    pub const fn stated(&self) -> Option<Movement> {
        self.stated
    }
}

/// One alternative a question offers, without the value it needs.
///
/// The wire vocabulary of [`Answer`], minus the account two of the answers
/// carry. It exists so a question can publish what may be said to it before
/// anyone has said anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnswerShape {
    SentToOwnAccount,
    ReceivedFromOwnAccount,
    Paid,
    Received,
    Fee,
    Income,
    Refund,
    /// «It was between accounts of mine, and I cannot say which one.»
    ///
    /// The eighth, and the only one that is not a claim about what the row
    /// **was** in the sense the other seven are: it says what the document did
    /// not contain. [`Self::generalises`] is where that difference is spent.
    BetweenOwnAccounts,
}

impl AnswerShape {
    /// Wire code. One place, so the transport cannot spell it differently.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::SentToOwnAccount => "sent_to_own_account",
            Self::ReceivedFromOwnAccount => "received_from_own_account",
            Self::Paid => "paid",
            Self::Received => "received",
            Self::Fee => "fee",
            Self::Income => "income",
            Self::Refund => "refund",
            // The word he said, not what this system does with it. «An
            // unplaceable movement» and «an own-account movement with the far
            // side unnamed» are both true and both describe the record; what he
            // told us is that the money went between accounts of his.
            Self::BetweenOwnAccounts => "between_own_accounts",
        }
    }

    /// Whether the answer must name one of the owner's accounts.
    ///
    /// [`Self::BetweenOwnAccounts`] answers `false`, and the whole of that
    /// answer is the `false`: it is the one own-account word for the owner who
    /// knows the far side is his and does not know which account it is.
    #[must_use]
    pub const fn needs_account(self) -> bool {
        matches!(self, Self::SentToOwnAccount | Self::ReceivedFromOwnAccount)
    }

    /// Whether recording this answer may also become a standing rule.
    ///
    /// Seven of the eight say what the row **was** — a fee, a return, an
    /// earning, money that left the perimeter, a movement to a named account of
    /// his — and every one of those is a claim about each later row a matcher
    /// matches. That is what a [`ClassificationRule`] is, and it is why the
    /// classification vocabulary carries them.
    ///
    /// [`Self::BetweenOwnAccounts`] says something else: that **this document**
    /// did not contain the other side. A rule made of it would file every later
    /// row of the same shape as a movement nothing can place — including, next
    /// month, the rows whose far half *is* in the export and which the pairing
    /// would have settled completely. A standing decision made of «I cannot
    /// say» converts future known cases into unknown ones, and it does it
    /// silently, which is the property that makes it worth refusing rather than
    /// warning about.
    ///
    /// **A decision and not a discovery.** Nothing in the vocabulary forbids the
    /// rule — [`Classification::OwnAccountMovement`] is in it, because a
    /// **source** that files every such row this way is making exactly the
    /// standing claim a rule is for. What is refused is minting one from the
    /// owner's «I cannot say». It is cheap to reverse if a live session shows
    /// him answering the same thing every month.
    #[must_use]
    pub const fn generalises(self) -> bool {
        !matches!(self, Self::BetweenOwnAccounts)
    }

    /// What choosing this alternative does to the owner's money-flow report.
    ///
    /// **Why this is a sentence per alternative and not a longer question.**
    /// The prompt asks what the row was; this says what each answer to it
    /// decides. Put in the prompt, the eight consequences would be one string
    /// holding a mapping from a word to its effect — a structure sent as prose,
    /// which `docs/api/conventions.md` §5 refuses for the reason that a client
    /// composing an answer has to parse it back out. Put here, the consequence
    /// travels **attached to the word it belongs to**, in the same list the
    /// caller already reads to find out what may be said, and a caller that
    /// shows the owner one alternative shows him its effect with it.
    ///
    /// **Why static text and not a computed projection.** Every sentence below
    /// is a claim about `MoneyFlow::absorb` in `iaam-core`, reached through
    /// [`Self`] → [`Answer::classification`] → `ObservedRow::resolve` →
    /// `normalize` → `EventKind`. The projection cannot be run here — there is
    /// no journal, no contour and no category index at the moment a question is
    /// asked — so the link is pinned by test instead: `tests/observation.rs`
    /// walks that chain for the pair the owner is most likely to confuse and
    /// asserts the two land in different quantities.
    ///
    /// **Why the money-flow report and not the returns path.** They disagree
    /// about one member, and the disagreement is recorded on
    /// `EventKind::flow_endpoints`: a refund is `InboundFromOutside` for the
    /// contour classifier, because a returns calculation must see the cash
    /// cross the boundary, while the household report subtracts it from
    /// spending. Naming both in one sentence would make every alternative read
    /// as a caveat; the household report is the one the owner reads a month of
    /// statements against, so it is the one named, and the sentence for
    /// `Refund` says which report it is talking about.
    ///
    /// **One of the eight also says what it does to his standing decisions**,
    /// and the asymmetry is deliberate. What a question says about that before
    /// it is answered is one sentence for the whole question —
    /// `GeneralisationProspect` in `iaam-app` — and it is true of seven of the
    /// eight words. [`Self::BetweenOwnAccounts`] is the one it is false of (see
    /// [`Self::generalises`]), so the correction travels attached to the word
    /// it corrects, which is this list's whole mechanism.
    #[must_use]
    pub const fn consequence(self) -> &'static str {
        match self {
            Self::SentToOwnAccount => {
                "The money left this account for the account you name, and both accounts get \
                 a leg. The money-flow report counts it under transfers between your own \
                 accounts and as neither money that came in nor money that went out — unless \
                 the account you name is outside the contour, where it counts as money that \
                 went out that no category explains."
            }
            Self::ReceivedFromOwnAccount => {
                "The money arrived at this account from the account you name, and both \
                 accounts get a leg; the operation is recorded from the sending side. The \
                 money-flow report counts it under transfers between your own accounts and as \
                 neither money that came in nor money that went out — unless the account you \
                 name is outside the contour, where the money did cross the boundary and \
                 counts as money that came in."
            }
            Self::Paid => {
                "The money left the perimeter. The money-flow report counts it as money that \
                 went out, under the category your rules give the row, or as an outflow \
                 nothing explains where no rule matches it."
            }
            Self::Received => {
                "The money came into the perimeter from outside. The money-flow report counts \
                 it as money that came in — the same line as new money you added — and not as \
                 anything the capital earned and not as money of your own coming back."
            }
            Self::Fee => {
                "The money left as a cost charged against the account, on a fee leg of its \
                 own. The money-flow report counts it under fees and not under what went out \
                 on purchases, so no spending category is asked for."
            }
            Self::Income => {
                "The money is what the capital earned. The money-flow report counts it under \
                 earnings, attributed to this account — a cash statement row names no \
                 security, so none is recorded — and not as money that came in from outside."
            }
            Self::Refund => {
                "A counterparty gave back an earlier outflow. The money-flow report subtracts \
                 it from what went out, in the category the money was spent in, \
                 rather than adding it to what came in or to what the capital earned."
            }
            Self::BetweenOwnAccounts => {
                "The money moved between accounts of yours and nothing here says which account \
                 the other side is. The money-flow report stops counting the amount as money \
                 that went out, and no spending category is asked for; it does not count it \
                 under transfers between your own accounts either, because no account is named \
                 on the far side — the amount is reported separately, as one the report could \
                 not place. Where the statement said which way the money ran, this account's \
                 balance moves by it; where it said nothing, nothing is posted anywhere and \
                 only the amount is reported. This answer also keeps no standing decision, \
                 because it says what this statement did not contain rather than what the line \
                 was: a later statement's lines are settled as they always were."
            }
        }
    }
}

/// The owner's answer to one question.
///
/// Seven of the eight variants name a direction **and** a classification,
/// because a directionless row needs both and a single answer is the only way
/// to give both without something provisional existing in between.
///
/// [`Self::BetweenOwnAccounts`] is the eighth and names only the
/// classification, for the reason recorded on it: the journal has a shape for
/// that movement **without** a direction, so it is the one answer that does not
/// have to supply one. See [`Self::movement`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "answer", rename_all = "snake_case")]
pub enum Answer {
    /// The money left this account for the owner's own account named here.
    SentToOwnAccount { to: AccountId },
    /// The money arrived at this account from the owner's own account named here.
    ReceivedFromOwnAccount { from: AccountId },
    /// The money left the perimeter, and it was not a fee.
    Paid,
    /// The money arrived from outside the perimeter, and it is not income.
    Received,
    /// The money left the perimeter as a fee.
    Fee { origin: FeeOrigin },
    /// The money arrived from outside the perimeter as income.
    ///
    /// `kind` is what the owner calls the earning, and `None` means he named
    /// none — not that the system will work one out. It is carried on the answer
    /// and not only on the request because [`Self::classification`] must be able
    /// to hand it on: an answer whose kind stopped at the row would leave every
    /// later row matching the same rule as income of no stated kind, which is
    /// the half of `iaam-7l7v` that is not about refunds.
    Income { kind: Option<IncomeKind> },
    /// The money arrived as a counterparty returning an earlier outflow.
    ///
    /// Distinct from [`Self::Received`], which is money arriving from outside
    /// that nobody is giving back. The journal reports the two in opposite
    /// columns — a refund is subtracted from what went out — so this is the one
    /// answer whose absence made the honest path record a worse fact than the
    /// converter that concluded for itself.
    Refund,
    /// The money moved between accounts of the owner's, and he cannot say which
    /// account the far side is.
    ///
    /// **The claim is his, and it is weaker than the two answers beside it.**
    /// [`Self::SentToOwnAccount`] and [`Self::ReceivedFromOwnAccount`] name the
    /// far account, so they record a complete movement whose other half exists;
    /// this one records that the movement happened and that its far endpoint is
    /// his and unnamed. Before it existed, a person who knew exactly this had
    /// two words and both wrote a wrong fact — naming a far account invents a
    /// half the document does not hold, and «I paid somebody» files an internal
    /// move as spending. A source printing the same claim was already believed
    /// (`FarSide::OwnAccount`); the owner was not, which is decision 0006's
    /// defect one row along.
    ///
    /// **It carries no direction and does not need one**, which is why it is a
    /// bare variant. Where the source printed a sign, that sign is the
    /// direction and the fact posts one cash leg; where it printed none, the
    /// fact is the legless one — the journal cannot debit or credit an account
    /// on a movement whose direction nobody stated. `ObservedRow::resolve_with`
    /// is where the two meet, and `EventKind::UnresolvedOwnAccountMovement`
    /// carries the reason.
    ///
    /// **It is not an assertion that the amount stayed inside a contour.** No
    /// contour can prove it holds every account the owner has, so an unnamed
    /// own endpoint is classified as indeterminate and the amount is *reported*
    /// as one no report could place. What he gets is that his spending stops
    /// being overstated; the stronger claim — that the far side is inside the
    /// group he drew — is a different claim and is not this one.
    BetweenOwnAccounts,
}

impl Answer {
    /// Which alternative this answer is.
    #[must_use]
    pub const fn shape(&self) -> AnswerShape {
        match self {
            Self::SentToOwnAccount { .. } => AnswerShape::SentToOwnAccount,
            Self::ReceivedFromOwnAccount { .. } => AnswerShape::ReceivedFromOwnAccount,
            Self::Paid => AnswerShape::Paid,
            Self::Received => AnswerShape::Received,
            Self::Fee { .. } => AnswerShape::Fee,
            Self::Income { .. } => AnswerShape::Income,
            Self::Refund => AnswerShape::Refund,
            Self::BetweenOwnAccounts => AnswerShape::BetweenOwnAccounts,
        }
    }

    /// Which way the money went, where the answer says.
    ///
    /// **`None` for exactly one answer, and it is not a widening of the
    /// contract.** The seven that state a direction still state one, and the
    /// caller that resolves a row reads this before the row's own word, so an
    /// owner contradicting the source still wins. [`Self::BetweenOwnAccounts`]
    /// states none because it has none to state — he was asked which of his
    /// accounts the far side is, not which way the money ran — and the row is
    /// then resolved with the direction the **source** printed, or with none if
    /// it printed none. That is `ObservedRow::resolve_with`, and it is the only
    /// place the two are put together.
    ///
    /// Answering `Some` here for it would have meant guessing a direction, and
    /// answering it in the type would have meant an `Option` on every variant
    /// that has one.
    #[must_use]
    pub const fn movement(&self) -> Option<Movement> {
        match self {
            Self::SentToOwnAccount { .. } | Self::Paid | Self::Fee { .. } => Some(Movement::Out),
            Self::ReceivedFromOwnAccount { .. }
            | Self::Received
            | Self::Income { .. }
            | Self::Refund => Some(Movement::In),
            Self::BetweenOwnAccounts => None,
        }
    }

    /// The decision the answer records, in the vocabulary a rule is written in.
    ///
    /// `ReceivedFromOwnAccount { from }` becomes `InternalTransfer { to: from }`
    /// and that is not a slip: a rule matches on the **counterparty**, and the
    /// account a rule names is the far side of the movement, whichever way it
    /// went. The direction the owner gave is deliberately dropped here, because
    /// it was a fact about *his row* and a rule is a claim about every row that
    /// matches it.
    ///
    /// So this is a lossy projection on purpose, and the loss must not be
    /// reversed by comparing the named account with the row's own: see
    /// [`Classification::implied_movement`]. The direction that answers **this**
    /// row is on [`Self::movement`], and the two are handed to
    /// `ObservedRow::resolve` together.
    #[must_use]
    pub const fn classification(&self) -> Classification {
        match self {
            Self::SentToOwnAccount { to } => Classification::InternalTransfer { to: *to },
            Self::ReceivedFromOwnAccount { from } => Classification::InternalTransfer { to: *from },
            Self::Paid | Self::Received => Classification::ExternalFlow,
            Self::Fee { origin } => Classification::Fee { origin: *origin },
            Self::Income { kind } => Classification::Income { kind: *kind },
            Self::Refund => Classification::Refund,
            // The sixth outcome, which until now only a **source** could reach.
            // The projection is lossy here as it is everywhere else in this
            // function, and one loss is deliberate beyond the direction: what
            // this answer generalises into is nothing at all
            // ([`AnswerShape::generalises`]), so the value below never becomes
            // a matcher's outcome by this route.
            Self::BetweenOwnAccounts => Classification::OwnAccountMovement,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClassificationResult {
    Resolved {
        classification: Classification,
        basis: Basis,
    },
    /// Cannot be inferred from data and is not covered by a rule. Guessing is forbidden.
    Ambiguous { question: Question },
}

/// Classification of a row.
///
/// Among several matching rules, the highest version wins: a change
/// creates a new version, and the highest is the owner's latest decision.
#[must_use]
pub fn classify(
    subject: &ClassificationSubject,
    rules: &[ClassificationRule],
) -> ClassificationResult {
    if let Counterparty::OwnAccount(to) = subject.counterparty {
        return ClassificationResult::Resolved {
            classification: Classification::InternalTransfer { to },
            basis: Basis::Derived,
        };
    }
    let chosen = rules
        .iter()
        .filter(|rule| rule.matcher.matches(subject))
        .max_by_key(|rule| rule.version);
    if let Some(rule) = chosen {
        return ClassificationResult::Resolved {
            classification: rule.outcome,
            basis: Basis::Rule {
                rule: rule.id,
                version: rule.version,
            },
        };
    }
    // **After** the rules and **before** the question. After, because a rule is
    // the owner's own standing decision about this counterparty and the word
    // here is the source's; a rule that says «this name is a shop» must win
    // over a bank that files the row as internal to itself. Before the
    // question, because there is nothing left to ask: the source has said what
    // the row is, and the only thing still open — which of his accounts — is
    // what no answer of his makes the row wait for, since the journal can
    // record the movement without it.
    if subject.far_side.is_own_account() {
        return ClassificationResult::Resolved {
            classification: Classification::OwnAccountMovement,
            // Not `Derived`, which is what this arm used to answer: nothing was
            // derived here, the source said so (`iaam-rdya`). See [`Basis`].
            basis: Basis::Asserted,
        };
    }
    ClassificationResult::Ambiguous {
        question: question_for(subject),
    }
}

fn question_for(subject: &ClassificationSubject) -> Question {
    match (&subject.counterparty, subject.movement) {
        (Counterparty::Named(counterparty), Some(_)) => Question::IsTransferInternal {
            account: subject.account,
            counterparty: counterparty.clone(),
        },
        (Counterparty::Unknown, Some(Movement::Out)) => Question::IsOutflowAFee {
            account: subject.account,
        },
        (Counterparty::Unknown, Some(Movement::In)) => Question::IsInflowIncome {
            account: subject.account,
        },
        // The source stated no direction. None of the three yes/no questions can
        // be asked: two of them branch on the movement to exist at all, and the
        // first would take "is this counterparty your own account?" as settling a
        // row whose direction is still open — the answer would be recorded and
        // the guess would be made anyway, one step further along.
        (Counterparty::Named(counterparty), None) => Question::UnresolvedDirection {
            account: subject.account,
            stated: subject.source_kind.clone(),
            counterparty: Some(counterparty.clone()),
        },
        (Counterparty::Unknown, None) => Question::UnresolvedDirection {
            account: subject.account,
            stated: subject.source_kind.clone(),
            counterparty: None,
        },
        // An own account does not reach this point: it is handled in `classify`
        // as inferred from data.
        (Counterparty::OwnAccount(to), _) => Question::IsTransferInternal {
            account: subject.account,
            counterparty: to.inner().to_string(),
        },
    }
}

/// What classification the event already expresses.
///
/// `None` — the event carries no classification: a trade, valuation,
/// reconstructed beginning, and control assertion are facts,
/// not owner's decisions and are not subject to recalculation.
///
/// Exhaustive `match`: a new event kind must fail to compile
/// here rather than silently fall out of recalculation.
#[must_use]
pub const fn classification_of(event: &Event) -> Option<Classification> {
    match event.kind {
        EventKind::CashTransfer { to, .. } => Some(Classification::InternalTransfer { to }),
        // Both read back as what they are. Reading them as `ExternalFlow` — the
        // only outcome that used to exist for them — would make every such fact
        // in the journal look to `recompute_plan` like a row still waiting to
        // be corrected into an outflow, which is the correction that put the
        // amount in «spent» in the first place.
        EventKind::OwnAccountMovement { .. } | EventKind::UnresolvedOwnAccountMovement { .. } => {
            Some(Classification::OwnAccountMovement)
        }
        EventKind::CashIn { .. } | EventKind::CashOut { .. } => Some(Classification::ExternalFlow),
        // A recorded refund reads back as a refund, not as the external flow it
        // used to read back as. The two were the same answer while the
        // vocabulary had one word for both; now that it has two, saying
        // `ExternalFlow` here would make every refund in the journal look to
        // `recompute_plan` like a row a refund rule still has to correct.
        EventKind::Refund { .. } => Some(Classification::Refund),
        EventKind::Fee { origin, .. } => Some(Classification::Fee { origin }),
        EventKind::Tax { .. } => None,
        EventKind::Income { kind, .. } => Some(Classification::Income { kind }),
        EventKind::Trade { .. }
        | EventKind::OpeningPosition { .. }
        | EventKind::OpeningCash { .. }
        | EventKind::Valuation { .. }
        | EventKind::ControlAssertion { .. }
        | EventKind::ImportCoverageGap { .. }
        // Corporate action and tender offer are facts, not decisions
        // of the owner, and are not subject to recalculation by rules. `Income` here
        // would be a plausible silent error: amortization is a
        // return of own capital (§6.5), and assigning it to income
        // means overstating income by the entire returned principal.
        | EventKind::CorporateAction { .. }
        | EventKind::OfferExercise { .. } => None,
    }
}

/// One recalculation-plan correction.
///
/// Unfolds in exactly two steps — reversal and replacement. There is no “change
/// event” variant in the type: the append-only log is enforced by the
/// form, not by the caller's promise (§4.8).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Correction {
    pub target: EventId,
    pub was: Classification,
    pub becomes: Classification,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CorrectionStep {
    Reverse {
        target: EventId,
    },
    Replace {
        target: EventId,
        classification: Classification,
    },
}

impl Correction {
    #[must_use]
    pub const fn steps(&self) -> [CorrectionStep; 2] {
        [
            CorrectionStep::Reverse {
                target: self.target,
            },
            CorrectionStep::Replace {
                target: self.target,
                classification: self.becomes,
            },
        ]
    }
}

/// Plan for recalculating history after a rule change.
///
/// Built from the **active** set of events: reversed
/// and replaced events are not included. Hence idempotence — after
/// applying the plan, the replacement event becomes active, whose
/// classification already matches the rule, and rerunning produces an
/// empty plan automatically, without a separate “already done” check.
///
/// A row not covered by a rule remains unchanged: recalculation does not
/// guess where ingestion does not guess.
pub fn recompute_plan(
    events: &[Event],
    subjects: &BTreeMap<EventId, ClassificationSubject>,
    rules: &[ClassificationRule],
) -> Result<Vec<Correction>, CorrectionError> {
    let mut plan = Vec::new();
    for event in resolve(events)? {
        let (Some(subject), Some(was)) = (subjects.get(&event.id), classification_of(event)) else {
            continue;
        };
        let ClassificationResult::Resolved {
            classification: becomes,
            ..
        } = classify(subject, rules)
        else {
            continue;
        };
        if becomes != was {
            plan.push(Correction {
                target: event.id,
                was,
                becomes,
            });
        }
    }
    Ok(plan)
}
