//! A row as its source stated it, before anyone concluded what it was (§10.4).
//!
//! [`crate::operation::SubmittedOperation`] carries a conclusion:
//! `Deposit` and `Withdrawal` assert a direction, `Transfer { to }` names an
//! account. A caller holding a bank row that says only "internal to this
//! institution" with an amount beside it has none of those, and the shape it was
//! offered forced it to invent one. It invented `deposit`, one of the rows was a
//! withdrawal, and a replacement correction had to undo it.
//!
//! **The fix is here rather than at classification.** Reaching `classify` from
//! the existing intake path would not have worked: by the time it could be
//! called the caller has already chosen an `OperationKind`, so the conclusion is
//! made and the evidence it was made from — the source's own direction word, the
//! counterparty string it printed — is no longer in the request. `classify`
//! would be handed the answer and asked to re-derive the question.
//!
//! So this module accepts the observation instead, and nothing in it is a fact
//! about money yet. An [`ObservedRow`] becomes an operation only once the
//! classification is settled — by the directory, by one of the owner's rules, or
//! by the owner answering.

use iaam_core::event::kind::IncomeKind;
use iaam_core::event::provenance::ParserVersion;
use iaam_core::ids::AccountId;
use iaam_core::money::CurrencyCode;
use serde::{Deserialize, Serialize};

use crate::classification::{
    Answer, Classification, ClassificationSubject, Counterparty, FarSide, Movement,
};
use crate::operation::{OperationDates, OperationKind, SubmittedOperation};
use crate::verdict::Rejection;

/// Which way the source said the money went.
///
/// A closed set of four, and the fourth is not a default. `Unknown` means the
/// source named no direction; `Inner` means it named one that does not resolve
/// to a direction — the word a bank prints for a movement it considers internal
/// to itself, which says the money did not leave the institution and nothing at
/// all about which of the owner's accounts was on which side.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservedDirection {
    /// The source said the money arrived.
    In,
    /// The source said the money left.
    Out,
    /// The source said the movement was internal to the institution, without
    /// saying which side this account was on.
    Inner,
    /// The source said nothing about direction.
    Unknown,
}

impl ObservedDirection {
    /// The direction as a [`Movement`], or `None` where the source stated none.
    ///
    /// `Inner` and `Unknown` both answer `None`, and they are still distinct
    /// values: `Inner` narrows the question — the counterparty is very likely
    /// another account at the same institution — while `Unknown` narrows
    /// nothing. Collapsing them here would be safe; collapsing them in the type
    /// would lose the narrowing.
    #[must_use]
    pub const fn movement(self) -> Option<Movement> {
        match self {
            Self::In => Some(Movement::In),
            Self::Out => Some(Movement::Out),
            Self::Inner | Self::Unknown => None,
        }
    }

    /// Parse the word a caller sent.
    ///
    /// A rejection rather than a fallback to `Unknown`: a caller that meant
    /// "out" and typed "outgoing" must be told, not silently asked a question it
    /// had already answered.
    pub fn parse(value: &str) -> Result<Self, Rejection> {
        match value {
            "in" => Ok(Self::In),
            "out" => Ok(Self::Out),
            "inner" => Ok(Self::Inner),
            "unknown" => Ok(Self::Unknown),
            other => Err(Rejection {
                field: "direction".to_owned(),
                expected: "in, out, inner or unknown".to_owned(),
                actual: other.to_owned(),
            }),
        }
    }

    /// Wire code. One place, so the transport cannot spell it differently.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::In => "in",
            Self::Out => "out",
            Self::Inner => "inner",
            Self::Unknown => "unknown",
        }
    }
}

/// Who the source said was on the other side.
///
/// There is deliberately no `OwnAccount` variant, unlike [`Counterparty`].
/// Recognising a printed name as one of the owner's accounts is a **conclusion**,
/// and this type is what the caller is allowed to state. The resolution happens
/// on this side of the wire, against the owner's directory, and it is what turns
/// a question into a derived internal transfer without asking anybody.
///
/// The source's own assertion that the far side is the owner's does not live
/// here either, and for a different reason: it names nobody, so it is not an
/// answer to «who», and it can be made about a row that also prints a name. It
/// is [`crate::classification::FarSide`], carried beside this field on
/// [`ObservedRow`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservedCounterparty {
    /// The source named a party, and this is the string it printed.
    Named(String),
    /// The source named nobody.
    Unknown,
}

/// Where the row came from, precisely enough to find it again.
///
/// Kept beside the observation rather than folded into it: a question asked
/// about a row outlives the response that carried it, and the answer has to name
/// the row it answers. A row identified only by its position in a batch stops
/// being identifiable the moment the batch is re-sent.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct RowIdentity {
    /// The document the row was read out of, as the source names it.
    pub document: Option<String>,
    /// The row's identifier within that document, as the source names it.
    pub row: Option<String>,
    /// The caller's own idempotency key for the row (§10.6).
    pub idempotency_key: Option<String>,
}

impl RowIdentity {
    /// A stable key for the row, or `None` when the caller gave nothing stable.
    ///
    /// `None` is honest rather than convenient: without it the same row
    /// re-submitted would open a second question about the same money, and the
    /// owner would answer one of them.
    #[must_use]
    pub fn key(&self) -> Option<String> {
        if let Some(key) = &self.idempotency_key {
            return Some(format!("idempotency/{key}"));
        }
        match (&self.document, &self.row) {
            (Some(document), Some(row)) => Some(format!("document/{document}/{row}")),
            (None, Some(row)) => Some(format!("row/{row}")),
            _ => None,
        }
    }
}

/// A row as the source stated it.
///
/// Every field is what the source said, or an explicit statement that it said
/// nothing. Nothing here has been normalised into a conclusion — in particular
/// `amount_minor` keeps the **source's own sign**, because the sign is evidence
/// about direction where the source used one and would be a fabricated direction
/// where it did not.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObservedRow {
    /// The account whose statement this row is on.
    pub account: AccountId,
    pub direction: ObservedDirection,
    /// The amount with the sign the source printed. Not made positive: making it
    /// positive discards the source's own statement about direction, and the
    /// whole point of this shape is to keep it.
    pub amount_minor: i64,
    pub currency: CurrencyCode,
    pub counterparty: ObservedCounterparty,
    /// What the source said about whose account is on the far side.
    ///
    /// A statement the source made, transcribed exactly as `direction` and
    /// `source_kind` are, and never inferred here: a converter may set it only
    /// where the export says so in words. `#[serde(default)]` because rows
    /// stored by an earlier build carry no such field, and what they carry is
    /// [`FarSide::Unstated`] — which is what they meant.
    #[serde(default)]
    pub far_side: FarSide,
    /// What the source called the operation, verbatim.
    ///
    /// The source's word for what the movement **was** — the cell a bank fills
    /// with "transfer" or "card payment". It is what the direction was read
    /// from, and it is kept beside that reading rather than instead of it, so a
    /// wrong reading stays visible against the word it was made from.
    pub source_kind: Option<String>,
    /// What the source called the operation's purpose, verbatim.
    ///
    /// The source's word for what the movement was **for**, and a different
    /// fact from [`Self::source_kind`] above it. Both are transcribed and
    /// neither is mapped: the owner's own rules match this one, and they are
    /// his, editable, and re-runnable over rows already recorded.
    ///
    /// **Two of his rule vocabularies read it, and they read one field.**
    /// `CategoryMatcher::SourceCategory` asks which of his categories the row
    /// belongs in; [`crate::classification::RuleMatcher::source_category`] asks
    /// what the row *is*. The second was missing until `iaam-93lz`, so a
    /// profile transcribing a category the way decision 0019 §6 requires
    /// produced a field no classification rule could name.
    ///
    /// This field did not exist while the observation path carried
    /// `source_kind` in the operation's `source_category` slot, so a category
    /// rule written on a source's category never matched an observed row and
    /// one written on an operation word matched rows the owner was not
    /// describing (`iaam-p683`).
    ///
    /// `#[serde(default)]` because a row stored by an earlier build carries no
    /// such field, exactly as [`Self::far_side`] does — and what such a row
    /// carries is `None`, which is what it meant: there was no way to state a
    /// source category at all.
    #[serde(default)]
    pub source_category: Option<String>,
    /// The category the **owner himself** filed the row under, at the source,
    /// verbatim.
    ///
    /// A different fact from [`Self::source_category`] above it, and the
    /// difference is whose decision it is. That one is the institution's own
    /// word for what the movement was for; this one is the owner's, taken in
    /// his institution's app and printed back in the export. Asking him again
    /// for what he has already told his bank is the worst question this system
    /// can ask, and it was asked once per row because nothing read this field.
    ///
    /// **Transcribed and never interpreted**, which is decision 0028's rule and
    /// is most at risk here, because the value looks like a conclusion and is
    /// not. It is his decision *in his bank's vocabulary*, not in his categories
    /// here, so nothing maps it: what it is called here is one question per
    /// distinct value, whose answer reaches every row carrying that value.
    ///
    /// `#[serde(default)]` because a row stored by an earlier build carries no
    /// such field, and `None` is what such a row meant: there was no way to
    /// state it at all.
    #[serde(default)]
    pub owner_category: Option<String>,
    /// The standardised code the source printed for the row, verbatim.
    ///
    /// The one word on the row that is not one institution's private
    /// vocabulary: the code is assigned by the payment network, so a rule
    /// written on it holds across institutions, where a rule written on a
    /// source's own category holds for one bank until it renames something. As
    /// the ground for a rule it covers a whole kind of spending where the
    /// printed description covers one merchant string.
    ///
    /// **Text and never a number.** It is an identifier printed with leading
    /// zeros, and a number loses them.
    ///
    /// **Nothing requires it.** The source leaves it empty on rows it assigns
    /// no code to — a transfer between the owner's own accounts is not a
    /// purchase from a merchant — and a profile that demanded it would refuse
    /// rows that are perfectly readable.
    ///
    /// `#[serde(default)]` for [`Self::owner_category`]'s reason.
    #[serde(default)]
    pub source_code: Option<String>,
    /// What the source printed as the description or payment purpose, verbatim.
    pub description: Option<String>,
    pub dates: OperationDates,
    pub source_time: Option<time::Time>,
    pub identity: RowIdentity,
}

impl ObservedRow {
    /// The row's attributes, as the classifier asks about them.
    ///
    /// `resolved` is what the owner's directory made of the printed counterparty:
    /// `Some(account)` when it recognised one of the owner's own accounts. The
    /// caller of this function does the recognising, because a pure function has
    /// no directory — and this is the seam through which
    /// [`Counterparty::OwnAccount`] reaches `classify` and produces a derived
    /// internal transfer with no question asked.
    #[must_use]
    pub fn subject(&self, resolved: Option<AccountId>) -> ClassificationSubject {
        ClassificationSubject {
            account: self.account,
            counterparty: match (resolved, &self.counterparty) {
                (Some(account), _) => Counterparty::OwnAccount(account),
                (None, ObservedCounterparty::Named(name)) => Counterparty::Named(name.clone()),
                (None, ObservedCounterparty::Unknown) => Counterparty::Unknown,
            },
            description: self.description.clone(),
            source_kind: self.source_kind.clone(),
            // Carried into the subject rather than kept for the envelope alone.
            // The field arrived with `iaam-p683` and nothing read it here, so
            // the owner could transcribe a source's category through the whole
            // channel and still not write a rule on it — which is the half of
            // decision 0019 §6 that was missing (`iaam-93lz`).
            source_category: self.source_category.clone(),
            // Carried for `source_category`'s reason and one of its own: this
            // is the answer the instance would otherwise ask him for once per
            // row, so a row that reaches `classify` without it is a row whose
            // own evidence was dropped on the way in.
            owner_category: self.owner_category.clone(),
            source_code: self.source_code.clone(),
            movement: self.movement(),
            far_side: self.far_side,
        }
    }

    /// Which way the money went, when the source said so.
    ///
    /// The source's own direction word and nothing else. The sign printed on
    /// the amount is **not** consulted, here or anywhere: a row with no
    /// direction word has no direction, whatever its sign happens to be,
    /// because a bank that prints every amount positive would otherwise have
    /// every row read as an inflow. The sign is still kept on
    /// [`Self::amount_minor`] as the evidence it is — read by whoever weighs
    /// evidence, never by this function, which answers only what the source
    /// stated.
    #[must_use]
    pub const fn movement(&self) -> Option<Movement> {
        self.direction.movement()
    }

    /// The name the counterparty is matched by in a rule, when there is one.
    #[must_use]
    pub fn counterparty_name(&self) -> Option<&str> {
        match &self.counterparty {
            ObservedCounterparty::Named(name) => Some(name.as_str()),
            ObservedCounterparty::Unknown => None,
        }
    }

    /// The magnitude, which is what every conclusive operation kind wants.
    ///
    /// A rejection rather than an absolute value: `i64::MIN` has no positive
    /// counterpart, and an amount of zero is a row that states no movement,
    /// which is not a movement of zero.
    fn magnitude(&self) -> Result<i64, Rejection> {
        let magnitude = self.amount_minor.checked_abs().filter(|value| *value > 0);
        magnitude.ok_or_else(|| Rejection {
            field: "amount".to_owned(),
            expected: "a non-zero amount the source stated".to_owned(),
            actual: self.amount_minor.to_string(),
        })
    }

    /// The operation this row is, once the classification and direction are
    /// settled.
    ///
    /// This is the **only** place an observation becomes a conclusion, and it
    /// takes both arguments because neither alone is enough:
    /// [`Classification::ExternalFlow`] does not say which way the money went,
    /// and a direction does not say whether the outflow was a fee.
    ///
    /// Between them the two arguments now reach every conclusion a cash
    /// statement row can be — deposit, withdrawal, transfer either way, fee,
    /// income with the kind the owner named, and refund. That list is the
    /// parity `iaam-7l7v` was about: while `Refund` was missing from the
    /// classification vocabulary, a caller that submitted an observation and let
    /// the server conclude got a strictly poorer journal than one that concluded
    /// for itself, and no question could repair it because none was ever asked
    /// about a return.
    ///
    /// `InternalTransfer { to }` names the **far side** of the movement and not
    /// a destination, so it is read against `movement` rather than against this
    /// row's own account: outgoing, the money left for `to`; incoming, it
    /// arrived from `to` and the operation is submitted from there. Comparing
    /// `to` with this account instead would answer for both cases at once and be
    /// wrong for one of them, which is iaam-xf49 — see
    /// [`Classification::implied_movement`]. An internal transfer whose far side
    /// is this very account is refused rather than recorded as a transfer to
    /// itself.
    ///
    /// **`movement` is optional, and exactly one classification survives its
    /// absence.** Every other outcome needs a direction — a deposit and a
    /// withdrawal are the same row with two answers — so `None` beside any of
    /// them is refused rather than resolved into a guess, which is the whole
    /// reason `Question::UnresolvedDirection` exists.
    /// [`Classification::OwnAccountMovement`] is the exception because the
    /// journal has a shape for it without one: the fact records that a movement
    /// the source attributed to the owner's own accounts happened, and posts
    /// nothing. Making the argument non-optional and adding a second entry
    /// point was considered and refused — the pair of functions would have to
    /// agree about which classifications each admits, and this match is where
    /// that agreement is checked once.
    pub fn resolve(
        &self,
        classification: Classification,
        movement: Option<Movement>,
    ) -> Result<SubmittedOperation, Rejection> {
        let amount_minor = self.magnitude()?;
        let currency = self.currency;
        let Some(movement) = movement else {
            return match classification {
                Classification::OwnAccountMovement => Ok(SubmittedOperation {
                    account: self.account,
                    kind: OperationKind::OwnAccountMovement {
                        movement: None,
                        amount_minor,
                        currency,
                    },
                    ..self.envelope(self.account)
                }),
                other => Err(Rejection {
                    field: "answer".to_owned(),
                    expected: "a direction, which every outcome except a movement between \
                               own accounts needs before it can be recorded"
                        .to_owned(),
                    actual: format!("{}, with no direction", outcome_word(other)),
                }),
            };
        };
        let kind = match (classification, movement) {
            (Classification::OwnAccountMovement, movement) => OperationKind::OwnAccountMovement {
                movement: Some(movement),
                amount_minor,
                currency,
            },
            (Classification::InternalTransfer { to }, Movement::Out) => {
                if to == self.account {
                    return Err(self.self_transfer());
                }
                OperationKind::Transfer {
                    to,
                    amount_minor,
                    currency,
                }
            }
            (Classification::InternalTransfer { to }, Movement::In) => {
                if to == self.account {
                    return Err(self.self_transfer());
                }
                // The money arrived, so the operation belongs to the account it
                // left: a transfer is submitted from its sending side, and the
                // event carries a leg on each.
                return Ok(SubmittedOperation {
                    account: to,
                    kind: OperationKind::Transfer {
                        to: self.account,
                        amount_minor,
                        currency,
                    },
                    ..self.envelope(to)
                });
            }
            (Classification::ExternalFlow, Movement::Out) => OperationKind::Withdrawal {
                amount_minor,
                currency,
            },
            (Classification::ExternalFlow, Movement::In) => OperationKind::Deposit {
                amount_minor,
                currency,
            },
            (Classification::Fee { origin }, Movement::Out) => OperationKind::Fee {
                amount_minor,
                currency,
                origin,
            },
            (Classification::Income { kind }, Movement::In) => OperationKind::Income {
                // The row is a cash statement line and names no security. An
                // instrument is not the same absence as the kind beside it: the
                // kind is a word the owner can say about every row a rule
                // matches, while the instrument is a different security on every
                // row, so there is nothing for a rule or an answer to carry.
                instrument: None,
                gross_minor: amount_minor,
                currency,
                // Whatever named the classification named this too, or named
                // nothing. `None` is still "not stated" (§4.9) — it is now the
                // owner's silence rather than the resolver's, which is the
                // difference between a fact he can supply and one nothing could.
                kind: kind as Option<IncomeKind>,
            },
            // A refund is money coming back on a purchase, so the operation is
            // the returning half and never the purchase. Nothing here reaches
            // for the outflow it reverses: the journal pairs them by category in
            // the money-flow report, and a link invented at intake would be a
            // claim about which purchase came back.
            (Classification::Refund, Movement::In) => OperationKind::Refund {
                amount_minor,
                currency,
            },
            // A fee that arrived, income that left, and a refund that left are
            // not rows this system can record: refusing is the only answer that
            // does not write something nobody asserted.
            //
            // The refund arm is reached by a route the other two are not. Both
            // answers that name a refund state that money arrived, so an owner
            // cannot produce this pair; a **rule** can, because a rule carries no
            // direction and fires on rows the owner has never seen — a matcher
            // written on a merchant's name matches that merchant's purchases as
            // well as its returns. The rejection is per row and the import
            // continues (§10.1), so the owner sees exactly which rows his rule
            // was too wide for instead of finding purchases filed as returns.
            (Classification::Fee { .. }, Movement::In)
            | (Classification::Income { .. } | Classification::Refund, Movement::Out) => {
                return Err(Rejection {
                    field: "answer".to_owned(),
                    expected: "an answer whose direction matches what it names: a fee leaves \
                               the account, and income and a refund arrive at it"
                        .to_owned(),
                    actual: format!("{} money", movement_word(movement)),
                });
            }
        };
        Ok(SubmittedOperation {
            account: self.account,
            kind,
            ..self.envelope(self.account)
        })
    }

    /// The row resolved by an answer the owner gave.
    ///
    /// **The answer's direction is read first and the row's second**, and that
    /// order is the contract: an answer states its own direction and the owner
    /// is entitled to contradict the source, so a row printed as incoming and
    /// answered `paid` is recorded as leaving.
    ///
    /// Seven of the eight answers state one, so for them the fallback is dead
    /// code. [`Answer::BetweenOwnAccounts`] states none — it says whose the far
    /// side is and not which way the money ran — and it is the one answer for
    /// which the row's own word decides: a source that printed a sign gives the
    /// movement its leg, and a source that printed nothing leaves the fact
    /// legless, which is the pair of shapes
    /// [`Classification::OwnAccountMovement`] resolves into above. Refusing the
    /// pairing instead would refuse the only answer the owner of such a row can
    /// truthfully give.
    pub fn resolve_with(&self, answer: Answer) -> Result<SubmittedOperation, Rejection> {
        self.resolve(
            answer.classification(),
            answer.movement().or_else(|| self.movement()),
        )
    }

    fn self_transfer(&self) -> Rejection {
        Rejection {
            field: "answer".to_owned(),
            expected: "an account different from the one the row is on".to_owned(),
            actual: self.account.inner().to_string(),
        }
    }

    /// Everything about the row that does not depend on what it turned out to be.
    ///
    /// `account` is passed rather than read from `self` because a received
    /// internal transfer is submitted from the sending account, and the envelope
    /// is otherwise identical.
    fn envelope(&self, account: AccountId) -> SubmittedOperation {
        SubmittedOperation {
            account,
            // Replaced by every caller; a placeholder is needed because
            // `OperationKind` has no meaningless value and inventing one would
            // put it in the type.
            kind: OperationKind::Deposit {
                amount_minor: 1,
                currency: self.currency,
            },
            dates: self.dates,
            source_time: self.source_time,
            idempotency_key: self.identity.idempotency_key.clone(),
            source_operation_id: self.identity.row.clone(),
            // Each word to its own field. This used to carry `source_kind`,
            // and the pair round-tripped — `scenarios/classification.rs` read
            // it back out as `source_kind` again — so nothing failed and the
            // owner's category rules matched the wrong rows in silence
            // (`iaam-p683`).
            source_category: self.source_category.clone(),
            owner_category: self.owner_category.clone(),
            source_code: self.source_code.clone(),
            source_kind: self.source_kind.clone(),
            description: self.description.clone(),
        }
    }
}

/// One outcome in a word, for a rejection the caller can act on.
const fn outcome_word(classification: Classification) -> &'static str {
    match classification {
        Classification::InternalTransfer { .. } => "a transfer to a named own account",
        Classification::ExternalFlow => "movement outside the portfolio",
        Classification::Fee { .. } => "a fee",
        Classification::Refund => "a refund",
        Classification::Income { .. } => "income",
        Classification::OwnAccountMovement => "a movement between own accounts",
    }
}

const fn movement_word(movement: Movement) -> &'static str {
    match movement {
        Movement::In => "incoming",
        Movement::Out => "outgoing",
    }
}

/// One submitted line.
///
/// The two arms are the whole of iaam-6qsa: a caller that **has** concluded is
/// still right to say so, and a caller that has not is no longer forced to
/// invent one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "intake", rename_all = "snake_case")]
pub enum Intake {
    /// The caller decided what the row is.
    Concluded { operation: Box<SubmittedOperation> },
    /// The caller reported what the source said and decided nothing.
    Observed {
        row: Box<ObservedRow>,
        /// What read the row, where something inside this product did.
        ///
        /// `None` is the ordinary case and it is not a missing value: it means
        /// the row arrived as JSON and nothing here read a document to produce
        /// it, so the version the fact records is
        /// [`crate::operation::PARSER_VERSION`] — the caller was the reader.
        /// `Some` is a reader in this product saying so: the source-profile
        /// engine writes `profile/<id>/<version>` here, and that is how a fact
        /// comes to record what actually read it rather than the one value
        /// `normalize` used to stamp on every channel alike (`iaam-h69n`).
        ///
        /// It is **not** a field of the published request shape, and the DTO
        /// conversion never fills it. A caller that could name its own reader
        /// could claim a profile's version for rows it typed by hand, and the
        /// set of rows a buggy profile wrote would stop being a set.
        ///
        /// `#[serde(default)]` because a row stored by an earlier build carries
        /// no such field, and what such a row carries is `None` — which is what
        /// it meant.
        #[serde(default)]
        reader: Option<ParserVersion>,
    },
}

impl Intake {
    /// A stable key for the row, when the caller gave one.
    #[must_use]
    pub fn row_key(&self) -> Option<String> {
        match self {
            Self::Concluded { operation } => operation
                .idempotency_key
                .as_ref()
                .map(|key| format!("idempotency/{key}")),
            Self::Observed { row, .. } => row.identity.key(),
        }
    }

    /// What read this row, where something inside this product did.
    ///
    /// Asked once here rather than by each caller matching on the tag, which is
    /// how the two arms come to be read differently. A conclusion answers
    /// `None` and always will: a caller that concluded what a row was is the
    /// reader of it, whatever produced the bytes it read.
    #[must_use]
    pub const fn reader(&self) -> Option<&ParserVersion> {
        match self {
            Self::Concluded { .. } => None,
            Self::Observed { reader, .. } => reader.as_ref(),
        }
    }

    /// Whether the caller submitted a conclusion.
    #[must_use]
    pub const fn is_concluded(&self) -> bool {
        matches!(self, Self::Concluded { .. })
    }

    /// The account the row is on, as the caller stated it.
    ///
    /// Both arms carry one and neither derives it: an observation names the
    /// account whose statement printed the line, and a conclusion names the
    /// account the operation is recorded against. It is one question — «whose
    /// row is this» — so it is answered once here rather than by each caller
    /// matching on the tag, which is how the two arms come to be read
    /// differently.
    ///
    /// Not the event's account, which can differ: a transfer is submitted from
    /// the sending side and the receiving statement's row still belongs to the
    /// receiving account. What this answers is which statement the row came
    /// off.
    #[must_use]
    pub const fn account(&self) -> AccountId {
        match self {
            Self::Concluded { operation } => operation.account,
            Self::Observed { row, .. } => row.account,
        }
    }
}
