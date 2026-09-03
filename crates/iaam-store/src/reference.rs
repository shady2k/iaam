//! Reference data: accounts, instruments, and environment versions.

use iaam_core::contour::{ContourDefinition, ContourId, ContourVersion};
use iaam_core::ids::{AccountId, CustodyId, InstrumentId, OwnerId, SourceId};
use iaam_core::instrument::{
    AliasInterval, AliasNamespace, CurrencyRoles, InstrumentKind, Lineage, LineageReason,
};
use iaam_core::money::CurrencyCode;
use iaam_core::report::balances::NegativeBalanceExpectation;
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use time::Date;
use time::format_description::well_known::Iso8601;

use crate::{ResolveError, SqliteStore, StoreError, now};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountRecord {
    pub id: AccountId,
    pub owner: OwnerId,
    pub title: String,
    pub institution: Option<String>,
}

/// What kind of cash an account holds, as the owner declares it.
///
/// **Nothing branches on this value, and nothing may start.** It is a grouping
/// label: report grouping reads it to render a heading, and no rule, no
/// projection, no classification, no validation, no invariant and no refusal
/// reads it at all (decision 0004 §3). A later feature that wants to branch on
/// it is evidence that the objection recorded in `iaam-d41s` was right, and the
/// answer then is to give that feature its own declaration rather than to grow
/// this one.
///
/// One branch is named because it is the one that would be reached for first,
/// and it is forbidden by name: **which negative balances are impossible must
/// not be derived from this label.** "A savings account cannot be overdrawn,
/// therefore warn" is precisely the reasoning `iaam-d41s` refuses, and it is
/// wrong on the first ordinary technical overdraft.
///
/// The type lives in the storage adapter rather than in `iaam-core` on purpose:
/// the core is where rules live, and a label no rule may read has no business
/// being reachable from one.
///
/// Cash only. `Balances` separates cash from positions structurally, so
/// `brokerage` and `security_position` are not values here: a position on an
/// instrument is what the journal records and needs no declaration.
///
/// Unset is a value — "not stated" — and is expressed by `Option::None` rather
/// than by a variant, so a caller cannot pass it where a stated class is meant.
/// It is never inferred from a title or from a transaction pattern.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CashAssetClass {
    Deposit,
    Savings,
    CardAccount,
    Wallet,
}

impl CashAssetClass {
    /// All variants. This exists for table-driven tests: a list assembled by
    /// hand in a test would silently drift from the `enum`.
    pub const ALL: [Self; 4] = [
        Self::Deposit,
        Self::Savings,
        Self::CardAccount,
        Self::Wallet,
    ];

    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Deposit => "deposit",
            Self::Savings => "savings",
            Self::CardAccount => "card_account",
            Self::Wallet => "wallet",
        }
    }

    /// Parse a code. `None`, rather than a default, ensures an unknown class
    /// reaches the caller instead of becoming a deposit.
    #[must_use]
    pub fn from_code(code: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|class| class.code() == code)
    }
}

/// The identity a source prints for one account.
///
/// `provider` is the client's own label for the source; it scopes
/// `provider_account_id`, which is **opaque to iaam** — not parsed, not
/// shape-checked, not validated against a register, and never rendered where a
/// title belongs. Equality and uniqueness are the entire contract, and they are
/// enough for the upsert (decision 0004 §1).
///
/// A struct rather than two adjacent `String` fields on the account: the
/// compiler cannot see two `String` values passed the wrong way round, and an
/// identity stored backwards would look like a legitimate one belonging to
/// nobody.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AccountIdentity {
    pub provider: String,
    pub provider_account_id: String,
}

/// A further identifier that reaches one account, valid over an interval.
///
/// Two cards over one underlying account are one account with two aliases, and
/// its balance is counted once. A card that stopped working is an alias whose
/// interval closed: there is no binding lifecycle, so "expired", "reissued",
/// "blocked" and "closed" are the same fact here, deliberately (decision 0004
/// §2).
///
/// [`AliasInterval`] is reused rather than respelled: instruments already carry
/// exactly this shape for exactly this reason, and a second spelling of a
/// half-open interval is a second set of off-by-one bugs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountAliasRecord {
    /// Opaque for the same reason `provider_account_id` is opaque.
    pub value: String,
    pub interval: AliasInterval,
}

/// An account with everything decision 0004 gives it.
///
/// Separate from [`AccountRecord`], which stays the summary the pre-existing
/// callers read. The separation is the point: the identity, the aliases and the
/// class travel only to the callers built to carry them, so a reader of the
/// summary cannot begin to branch on a label that nothing may branch on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountDetailRecord {
    pub id: AccountId,
    pub owner: OwnerId,
    pub title: String,
    pub institution: Option<String>,
    /// `None` — the account carries no external identity. Accounts written
    /// before decision 0004 are all in this state, and none of them is given one
    /// by guesswork.
    pub identity: Option<AccountIdentity>,
    pub cash_class: Option<CashAssetClass>,
    /// What the owner says a negative balance on this account would mean
    /// (`iaam-d41s`). `None` is «he has not said».
    ///
    /// A **second** value beside `cash_class`, never derived from it. Decision
    /// 0004 §3 forbids the merge by name: «a savings account cannot be
    /// overdrawn, therefore warn» is wrong on the first ordinary technical
    /// overdraft. The type comes from `iaam_core::report::balances` because
    /// that is the one place that reads it — unlike [`CashAssetClass`], which
    /// lives here precisely so no rule can reach it.
    pub negative_balance_expectation: Option<NegativeBalanceExpectation>,
    pub aliases: Vec<AccountAliasRecord>,
}

/// What [`SqliteStore::create_account`] did.
///
/// `Existing` is not a failure: it is the upsert by external identity working.
/// It is distinguished from `Created` so the caller can tell the truth about
/// what happened rather than reporting a creation that did not occur.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccountCreation {
    Created(AccountDetailRecord),
    /// The identity was already known: this is the account created last time.
    Existing(AccountDetailRecord),
}

/// One declaration in a replacement: the owner's word, his withdrawal of it, or
/// his silence.
///
/// Three states, not two, and the third is the whole reason this type exists. A
/// replacement that spelled «leave this alone» and «he states none» the same way
/// would clear, on every call, every field the caller did not happen to
/// mention — and one of the fields it governs decides which account a later
/// import lands on.
///
/// [`AccountTransferStatementRecord`] draws the same line one noun away: an
/// empty partner list is «money moves between this account and none of my
/// others», and having said nothing at all is a different fact. The distinction
/// is borrowed rather than reinvented, because a second spelling of it is a
/// second place for a silence to be read as an answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Declared<T> {
    /// Not mentioned. The stored value stands exactly as it stood.
    Untouched,
    /// Stated as none. The stored value is cleared, and the account goes back to
    /// «he has not said» — which is a thing he is allowed to say.
    Cleared,
    /// Stated as this.
    Stated(T),
}

/// The declarations an account carries beside its title, as the owner now states
/// them.
///
/// Three independent statements rather than one set, so each carries its own
/// [`Declared`]. Folding them into a single replaced value would make stating a
/// cash class withdraw an identity, which is exactly the accident the third
/// state exists to prevent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountDeclarations {
    pub identity: Declared<AccountIdentity>,
    pub cash_class: Declared<CashAssetClass>,
    pub negative_balance_expectation: Declared<NegativeBalanceExpectation>,
}

/// What [`SqliteStore::replace_account_declarations`] recorded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountDeclarationsRecorded {
    pub account: AccountDetailRecord,
    /// The identity the account carried until this call, when the call replaced
    /// it with a different one or withdrew it.
    ///
    /// `None` covers the three cases that need no announcement: the account
    /// carried no identity, the call did not mention the identity, or the
    /// identity stated is the one already recorded. Giving an identity to an
    /// account that had none is an ordinary first statement; re-pointing one is
    /// not, and this field is how the caller is told which of the two happened.
    pub previous_identity: Option<AccountIdentity>,
}

/// The current version of a contour owned by one portfolio owner.
///
/// The title travels with the identity because it is a property of the version,
/// not of the contour: a caller reading the composition back needs the name the
/// owner gave it, and a caller adding an account to it needs the name it already
/// carries rather than being asked to retype one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContourRecord {
    pub id: ContourId,
    pub owner: OwnerId,
    pub version: ContourVersion,
    /// The title recorded with that version.
    pub title: String,
}

/// The owner's recorded statement about one account's transfer partners.
///
/// `partners` may be empty, and that is not the same as having no record: an
/// empty list is «money moves between this account and none of my others»,
/// while the absence of a record altogether is «he has not said».
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountTransferStatementRecord {
    pub account: AccountId,
    pub partners: Vec<AccountId>,
}

/// The owner's recorded statement that an account sits outside every contour.
///
/// Only the deliberate exclusion is a record. Membership is read from the
/// contour composition, and an account with neither is awaiting a decision —
/// a state expressed by the absence of a row rather than by a third value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountScopeExclusionRecord {
    pub account: AccountId,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstrumentRecord {
    pub id: InstrumentId,
    /// `None` — the parent is not set. Such an instrument is treated as
    /// incomplete, rather than as a stock by default (§4.9, §5.4).
    pub kind: Option<InstrumentKind>,
    pub symbol: String,
    pub title: String,
    pub currencies: CurrencyRoles,
    pub lineage: Option<Lineage>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustodyRecord {
    pub id: CustodyId,
    pub owner: OwnerId,
    pub title: String,
    pub institution: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AliasRecord {
    pub namespace: AliasNamespace,
    pub value: String,
    pub instrument: InstrumentId,
    pub interval: AliasInterval,
    pub source: SourceId,
}

/// Changing an instrument's external code.
///
/// A struct, rather than six positional arguments. It is not just about the
/// clippy threshold: `from` and `to` are two adjacent `&str` values that the compiler
/// cannot detect as swapped, resulting in an alias
/// registered backwards (§15.1).
#[derive(Debug, Clone)]
pub struct AliasRename {
    pub namespace: AliasNamespace,
    /// The code in effect before the change.
    pub from: String,
    /// The code in effect from the change date.
    pub to: String,
    /// Change date: the old interval is closed by it, and the new one is opened by it.
    pub on: Date,
    pub instrument: InstrumentId,
    pub source: SourceId,
}

/// Dates in storage use ISO-8601, as everywhere else in the schema.
fn date_to_text(value: Date) -> String {
    value
        .format(&Iso8601::DATE)
        .expect("date is formatted as ISO-8601")
}

fn text_to_date(value: &str) -> Result<Date, StoreError> {
    Date::parse(value, &Iso8601::DATE).map_err(|_| StoreError::NotFound {
        what: "alias date",
        id: value.to_owned(),
    })
}

impl SqliteStore {
    /// Creating or updating an account.
    ///
    /// The `WHERE accounts.owner = excluded.owner` condition is mandatory:
    /// without it, a request with another owner's (or a guessed) identifier
    /// would overwrite another owner's account name (§14).
    pub fn upsert_account(&self, account: &AccountRecord) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT INTO accounts (id, owner, title, institution, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT (id) DO UPDATE SET
                 title = excluded.title,
                 institution = excluded.institution
             WHERE accounts.owner = excluded.owner",
            params![
                account.id.inner().to_string(),
                account.owner.inner().to_string(),
                account.title,
                account.institution,
                now(),
            ],
        )?;
        Ok(())
    }

    pub fn list_accounts(&self, owner: OwnerId) -> Result<Vec<AccountRecord>, StoreError> {
        let mut statement = self.conn.prepare(
            "SELECT id, title, institution FROM accounts WHERE owner = ?1 ORDER BY title, id",
        )?;
        let rows = statement.query_map([owner.inner().to_string()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
            ))
        })?;
        let mut accounts = Vec::new();
        for row in rows {
            let (id, title, institution) = row?;
            accounts.push(AccountRecord {
                id: AccountId(parse_uuid(&id, "account")?),
                owner,
                title,
                institution,
            });
        }
        Ok(accounts)
    }

    /// Create an account, upserting by external identity.
    ///
    /// A create carrying an identity that already exists returns the account
    /// created last time rather than minting a second one, and changes nothing
    /// about it. The title is a display name: repeating an identity under a
    /// different title is not a rename, and treating it as one would let a
    /// re-import silently overwrite what the owner reads.
    ///
    /// An account carrying no identity is always created. Two accounts that
    /// state no identity are not the same account, and merging them on the
    /// strength of a shared absence is the one mistake the partial uniqueness
    /// index exists to prevent.
    ///
    /// Immediate transaction: the lookup and the insert are a check-then-act,
    /// and two concurrent imports of one statement would otherwise both find
    /// nothing and both mint an account.
    pub fn create_account(
        &mut self,
        account: &AccountDetailRecord,
    ) -> Result<AccountCreation, StoreError> {
        let transaction = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;

        if let Some(identity) = &account.identity {
            let existing: Option<String> = transaction
                .query_row(
                    "SELECT id FROM accounts
                     WHERE owner = ?1 AND provider = ?2 AND provider_account_id = ?3",
                    params![
                        account.owner.inner().to_string(),
                        identity.provider,
                        identity.provider_account_id,
                    ],
                    |row| row.get(0),
                )
                .optional()?;
            if let Some(id) = existing {
                let id = AccountId(parse_uuid(&id, "account")?);
                let stored = read_account_detail(&transaction, account.owner, id)?;
                transaction.commit()?;
                return Ok(AccountCreation::Existing(stored));
            }
        }

        transaction.execute(
            "INSERT INTO accounts
                 (id, owner, title, institution, created_at,
                  provider, provider_account_id, cash_class,
                  negative_balance_expectation)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                account.id.inner().to_string(),
                account.owner.inner().to_string(),
                account.title,
                account.institution,
                now(),
                account.identity.as_ref().map(|it| it.provider.as_str()),
                account
                    .identity
                    .as_ref()
                    .map(|it| it.provider_account_id.as_str()),
                account.cash_class.map(CashAssetClass::code),
                account
                    .negative_balance_expectation
                    .map(NegativeBalanceExpectation::code),
            ],
        )?;
        for alias in &account.aliases {
            transaction.execute(
                "INSERT INTO account_aliases (owner, account, value, valid_from, valid_to)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    account.owner.inner().to_string(),
                    account.id.inner().to_string(),
                    alias.value,
                    date_to_text(alias.interval.valid_from),
                    alias.interval.valid_to.map(date_to_text),
                ],
            )?;
        }
        let stored = read_account_detail(&transaction, account.owner, account.id)?;
        transaction.commit()?;
        Ok(AccountCreation::Created(stored))
    }

    /// Replace one account's aliases with the set the owner now states.
    ///
    /// Replacement rather than an add-one/close-one pair, following
    /// `record_account_transfer_statement`: the owner states what is true now,
    /// and a diff against what he said last time is a second thing to get wrong.
    /// A card that stopped working is stated as an alias whose interval closed,
    /// alongside whichever alias replaced it.
    ///
    /// The account must be the owner's. The delete alone would silently succeed
    /// against a stranger's account, so ownership is established first rather
    /// than left to the foreign key on the insert — an empty set would never
    /// reach one.
    pub fn replace_account_aliases(
        &mut self,
        owner: OwnerId,
        account: AccountId,
        aliases: &[AccountAliasRecord],
    ) -> Result<(), StoreError> {
        let transaction = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let held: Option<String> = transaction
            .query_row(
                "SELECT id FROM accounts WHERE owner = ?1 AND id = ?2",
                params![owner.inner().to_string(), account.inner().to_string()],
                |row| row.get(0),
            )
            .optional()?;
        if held.is_none() {
            return Err(StoreError::NotFound {
                what: "account",
                id: account.inner().to_string(),
            });
        }

        transaction.execute(
            "DELETE FROM account_aliases WHERE owner = ?1 AND account = ?2",
            params![owner.inner().to_string(), account.inner().to_string()],
        )?;
        for alias in aliases {
            transaction.execute(
                "INSERT INTO account_aliases (owner, account, value, valid_from, valid_to)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    owner.inner().to_string(),
                    account.inner().to_string(),
                    alias.value,
                    date_to_text(alias.interval.valid_from),
                    alias.interval.valid_to.map(date_to_text),
                ],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    /// Replace the declarations an account carries: the identity its source
    /// prints, the class of cash the owner says it holds, and what he expects a
    /// negative balance on it to mean.
    ///
    /// The three could previously be stated only at creation, and
    /// [`Self::create_account`] deliberately ignores them when it finds the
    /// identity already known — that call is an upsert by identity, not an
    /// update of one. So every account that already existed could never acquire
    /// any of the three, which is the defect this method closes.
    ///
    /// Replacement rather than a patch, following
    /// [`Self::replace_account_aliases`]: the owner says what is true now. What
    /// is different here is that the three are separate statements, so the
    /// replacement is per field and [`Declared::Untouched`] is what leaves one
    /// alone.
    ///
    /// **Re-pointing an identity is allowed, and is reported rather than
    /// refused.** The tempting rule — refuse a change once facts were imported
    /// under the old identity — cannot be stated against this schema. `events`
    /// records a fact against an account id and a free `source` label; no
    /// column and no event kind records the external identity in force when the
    /// row arrived, and the journal is append-only in the database, so nothing
    /// can be backfilled to make it. A refusal would therefore have to be
    /// conditioned on «this account has facts at all», which is a different
    /// claim: it refuses an account whose whole history was typed in by hand
    /// under no identity, and it still does not answer the question anyone
    /// asked. What the caller gets instead is [`previous_identity`], so a
    /// re-pointing is visible as a re-pointing.
    ///
    /// [`previous_identity`]: AccountDeclarationsRecorded::previous_identity
    ///
    /// Immediate transaction: the collision check and the update are a
    /// check-then-act, and two calls claiming one identity would otherwise both
    /// find it free.
    pub fn replace_account_declarations(
        &mut self,
        owner: OwnerId,
        account: AccountId,
        declarations: &AccountDeclarations,
    ) -> Result<AccountDeclarationsRecorded, StoreError> {
        let transaction = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        // Reading the account first establishes that it is this owner's: the
        // update alone would report success having matched no row, and the
        // caller would be told he had recorded a statement about someone else's
        // account (§14).
        let held = read_account_detail(&transaction, owner, account)?;

        let identity = match &declarations.identity {
            Declared::Untouched => held.identity.clone(),
            Declared::Cleared => None,
            Declared::Stated(identity) => Some(identity.clone()),
        };
        // The partial unique index would abort the update with a message naming
        // two columns; the owner needs to be told that another of his accounts
        // already answers to this identity. The check is skipped when the
        // identity is unchanged, because a row does not collide with itself.
        if let Some(wanted) = identity.as_ref() {
            if held.identity.as_ref() != Some(wanted) {
                let taken: Option<String> = transaction
                    .query_row(
                        "SELECT id FROM accounts
                         WHERE owner = ?1 AND provider = ?2 AND provider_account_id = ?3",
                        params![
                            owner.inner().to_string(),
                            wanted.provider,
                            wanted.provider_account_id,
                        ],
                        |row| row.get(0),
                    )
                    .optional()?;
                if taken.is_some() {
                    return Err(StoreError::AlreadyExists {
                        what: "an account with that external identity",
                    });
                }
            }
        }

        let cash_class = match &declarations.cash_class {
            Declared::Untouched => held.cash_class,
            Declared::Cleared => None,
            Declared::Stated(class) => Some(*class),
        };
        let expectation = match &declarations.negative_balance_expectation {
            Declared::Untouched => held.negative_balance_expectation,
            Declared::Cleared => None,
            Declared::Stated(expectation) => Some(*expectation),
        };

        transaction.execute(
            "UPDATE accounts
                SET provider = ?3,
                    provider_account_id = ?4,
                    cash_class = ?5,
                    negative_balance_expectation = ?6
              WHERE owner = ?1 AND id = ?2",
            params![
                owner.inner().to_string(),
                account.inner().to_string(),
                identity.as_ref().map(|it| it.provider.as_str()),
                identity.as_ref().map(|it| it.provider_account_id.as_str()),
                cash_class.map(CashAssetClass::code),
                expectation.map(NegativeBalanceExpectation::code),
            ],
        )?;

        let stored = read_account_detail(&transaction, owner, account)?;
        transaction.commit()?;

        // Announced only when an identity was displaced. Giving one to an
        // account that had none is ordinary, and restating the one already
        // recorded displaced nothing.
        let previous_identity = match &declarations.identity {
            Declared::Untouched => None,
            Declared::Cleared | Declared::Stated(_) => held
                .identity
                .filter(|previous| identity.as_ref() != Some(previous)),
        };
        Ok(AccountDeclarationsRecorded {
            account: stored,
            previous_identity,
        })
    }

    /// Every account of one owner, with the identity, aliases and class it
    /// carries.
    ///
    /// An account written before decision 0004 reads back with no identity, no
    /// class and no aliases. That is the honest answer rather than a defect: the
    /// migration invented nothing.
    pub fn list_account_details(
        &self,
        owner: OwnerId,
    ) -> Result<Vec<AccountDetailRecord>, StoreError> {
        let mut statement = self.conn.prepare(
            "SELECT id, title, institution, provider, provider_account_id, cash_class,
                    negative_balance_expectation
             FROM accounts WHERE owner = ?1 ORDER BY title, id",
        )?;
        let rows = statement.query_map([owner.inner().to_string()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, Option<String>>(6)?,
            ))
        })?;
        let mut accounts = Vec::new();
        for row in rows {
            let (
                id,
                title,
                institution,
                provider,
                provider_account_id,
                cash_class,
                negative_balance_expectation,
            ) = row?;
            accounts.push(AccountDetailRecord {
                id: AccountId(parse_uuid(&id, "account")?),
                owner,
                title,
                institution,
                identity: external_identity(provider, provider_account_id),
                cash_class: parse_cash_class(cash_class.as_deref())?,
                negative_balance_expectation: parse_negative_balance_expectation(
                    negative_balance_expectation.as_deref(),
                )?,
                aliases: Vec::new(),
            });
        }
        drop(statement);

        let mut statement = self.conn.prepare(
            "SELECT account, value, valid_from, valid_to FROM account_aliases
             WHERE owner = ?1 ORDER BY account, valid_from, value",
        )?;
        let rows = statement.query_map([owner.inner().to_string()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
            ))
        })?;
        for row in rows {
            let (account, value, valid_from, valid_to) = row?;
            let account = AccountId(parse_uuid(&account, "account")?);
            let alias = AccountAliasRecord {
                value,
                interval: AliasInterval {
                    valid_from: text_to_date(&valid_from)?,
                    valid_to: valid_to.as_deref().map(text_to_date).transpose()?,
                },
            };
            if let Some(target) = accounts.iter_mut().find(|entry| entry.id == account) {
                target.aliases.push(alias);
            }
        }
        Ok(accounts)
    }

    pub fn upsert_instrument(&self, instrument: &InstrumentRecord) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT INTO instruments
                 (id, kind, symbol, title,
                  denomination_currency, settlement_currency, quote_currency,
                  lineage_parent, lineage_reason, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
             ON CONFLICT (id) DO UPDATE SET
                 kind = excluded.kind,
                 symbol = excluded.symbol,
                 title = excluded.title,
                 denomination_currency = excluded.denomination_currency,
                 settlement_currency = excluded.settlement_currency,
                 quote_currency = excluded.quote_currency,
                 lineage_parent = excluded.lineage_parent,
                 lineage_reason = excluded.lineage_reason",
            params![
                instrument.id.inner().to_string(),
                instrument.kind.map(InstrumentKind::code),
                instrument.symbol,
                instrument.title,
                instrument.currencies.denomination.code(),
                instrument.currencies.settlement.code(),
                instrument.currencies.quote.code(),
                instrument.lineage.map(|l| l.parent.inner().to_string()),
                instrument.lineage.map(|l| l.reason.code()),
                now(),
            ],
        )?;
        Ok(())
    }

    /// Creating or updating a custody place.
    ///
    /// The `WHERE custody_places.owner = excluded.owner` condition
    /// required for the same reason as accounts: without it, a request
    /// with someone else's identifier would overwrite another
    /// owner's storage location (§14).
    pub fn upsert_custody_place(&self, place: &CustodyRecord) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT INTO custody_places (id, owner, title, institution, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT (id) DO UPDATE SET
                 title = excluded.title,
                 institution = excluded.institution
             WHERE custody_places.owner = excluded.owner",
            params![
                place.id.inner().to_string(),
                place.owner.inner().to_string(),
                place.title,
                place.institution,
                now(),
            ],
        )?;
        Ok(())
    }

    pub fn list_custody_places(&self, owner: OwnerId) -> Result<Vec<CustodyRecord>, StoreError> {
        let mut statement = self.conn.prepare(
            "SELECT id, title, institution FROM custody_places
             WHERE owner = ?1 ORDER BY title, id",
        )?;
        let rows = statement.query_map([owner.inner().to_string()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
            ))
        })?;
        let mut places = Vec::new();
        for row in rows {
            let (id, title, institution) = row?;
            places.push(CustodyRecord {
                id: CustodyId(parse_uuid(&id, "custody")?),
                owner,
                title,
                institution,
            });
        }
        Ok(places)
    }

    pub fn record_alias(&self, alias: &AliasRecord) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT INTO instrument_aliases
                 (namespace, value, instrument, valid_from, valid_to, source, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                alias.namespace.code(),
                alias.value,
                alias.instrument.inner().to_string(),
                date_to_text(alias.interval.valid_from),
                alias.interval.valid_to.map(date_to_text),
                alias.source.inner().to_string(),
                now(),
            ],
        )?;
        Ok(())
    }

    /// Change the external code: close the old interval, open a new one.
    ///
    /// A single transaction is required: between the two operations, the old code
    /// is already closed, while the new one has not yet been created, and concurrent
    /// document resolution would get `Unknown` instead of the instrument.
    pub fn rename_alias(&mut self, rename: &AliasRename) -> Result<(), StoreError> {
        let AliasRename {
            namespace,
            from,
            to,
            on,
            instrument,
            source,
        } = rename;
        let (namespace, on, instrument, source) = (*namespace, *on, *instrument, *source);
        let instrument_text = instrument.inner().to_string();
        let transaction = self.conn.transaction()?;
        let changed = transaction.execute(
            "UPDATE instrument_aliases SET valid_to = ?1
             WHERE namespace = ?2 AND value = ?3 AND instrument = ?4 AND valid_to IS NULL",
            params![date_to_text(on), namespace.code(), from, &instrument_text],
        )?;
        if changed != 1 {
            return Err(StoreError::AliasNotFoundForInstrument {
                namespace: namespace.code(),
                value: from.clone(),
                instrument: instrument_text,
            });
        }
        transaction.execute(
            "INSERT INTO instrument_aliases
                 (namespace, value, instrument, valid_from, valid_to, source, created_at)
             VALUES (?1, ?2, ?3, ?4, NULL, ?5, ?6)",
            params![
                namespace.code(),
                to,
                instrument.inner().to_string(),
                date_to_text(on),
                source.inner().to_string(),
                now(),
            ],
        )?;
        transaction.commit()?;
        Ok(())
    }

    /// Instrument by external code on a given date.
    pub fn resolve_instrument(
        &self,
        namespace: AliasNamespace,
        value: &str,
        on: Date,
    ) -> Result<InstrumentId, ResolveError> {
        let mut statement = self
            .conn
            .prepare(
                "SELECT instrument, valid_from, valid_to FROM instrument_aliases
                 WHERE namespace = ?1 AND value = ?2 ORDER BY valid_from",
            )
            .map_err(StoreError::from)?;
        let rows = statement
            .query_map(params![namespace.code(), value], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            })
            .map_err(StoreError::from)?;

        let mut known: Vec<(String, Date, Option<Date>)> = Vec::new();
        for row in rows {
            let (instrument, from, to) = row.map_err(StoreError::from)?;
            let from = text_to_date(&from)?;
            let to = to.as_deref().map(text_to_date).transpose()?;
            known.push((instrument, from, to));
        }

        if known.is_empty() {
            return Err(ResolveError::Unknown {
                namespace: namespace.code(),
                value: value.to_owned(),
            });
        }

        let matching: Vec<&(String, Date, Option<Date>)> = known
            .iter()
            .filter(|(_, from, to)| {
                AliasInterval {
                    valid_from: *from,
                    valid_to: *to,
                }
                .covers(on)
            })
            .collect();

        match matching.as_slice() {
            [] => {
                let known_from = known
                    .first()
                    .map(|(_, from, _)| date_to_text(*from))
                    .unwrap_or_default();
                let known_to = known
                    .last()
                    .and_then(|(_, _, to)| *to)
                    .map_or_else(|| "open".to_owned(), date_to_text);
                Err(ResolveError::NotOnDate {
                    namespace: namespace.code(),
                    value: value.to_owned(),
                    on: date_to_text(on),
                    known_from,
                    known_to,
                })
            }
            [(instrument, _, _)] => Ok(InstrumentId(
                parse_uuid(instrument, "instrument").map_err(ResolveError::Store)?,
            )),
            many => Err(ResolveError::Ambiguous {
                namespace: namespace.code(),
                value: value.to_owned(),
                on: date_to_text(on),
                candidates: many.len(),
            }),
        }
    }

    pub fn instrument(&self, id: InstrumentId) -> Result<Option<InstrumentRecord>, StoreError> {
        let row = self
            .conn
            .query_row(
                "SELECT kind, symbol, title, denomination_currency,
                        settlement_currency, quote_currency,
                        lineage_parent, lineage_reason
                 FROM instruments WHERE id = ?1",
                [id.inner().to_string()],
                |row| {
                    Ok((
                        row.get::<_, Option<String>>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, String>(5)?,
                        row.get::<_, Option<String>>(6)?,
                        row.get::<_, Option<String>>(7)?,
                    ))
                },
            )
            .optional()?;
        row.map(|parts| decode_instrument(id, parts)).transpose()
    }

    pub fn list_instruments(&self) -> Result<Vec<InstrumentRecord>, StoreError> {
        let mut statement = self.conn.prepare(
            "SELECT id, kind, symbol, title, denomination_currency,
                    settlement_currency, quote_currency,
                    lineage_parent, lineage_reason
             FROM instruments ORDER BY symbol, id",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, Option<String>>(7)?,
                row.get::<_, Option<String>>(8)?,
            ))
        })?;
        rows.map(|row| {
            let (id, kind, symbol, title, denomination, settlement, quote, parent, reason) = row?;
            let id = InstrumentId(parse_uuid(&id, "instrument")?);
            decode_instrument(
                id,
                (
                    kind,
                    symbol,
                    title,
                    denomination,
                    settlement,
                    quote,
                    parent,
                    reason,
                ),
            )
        })
        .collect()
    }

    pub fn list_aliases(&self) -> Result<Vec<AliasRecord>, StoreError> {
        let mut statement = self.conn.prepare(
            "SELECT namespace, value, instrument, valid_from, valid_to, source
             FROM instrument_aliases ORDER BY namespace, value, valid_from",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, String>(5)?,
            ))
        })?;
        let mut aliases = Vec::new();
        for row in rows {
            let (namespace, value, instrument, valid_from, valid_to, source) = row?;
            let namespace =
                AliasNamespace::from_code(&namespace).ok_or_else(|| StoreError::NotFound {
                    what: "namespace",
                    id: namespace,
                })?;
            let instrument = InstrumentId(parse_uuid(&instrument, "instrument")?);
            let valid_from = text_to_date(&valid_from)?;
            let valid_to = valid_to.as_deref().map(text_to_date).transpose()?;
            let source = SourceId(parse_uuid(&source, "source")?);
            aliases.push(AliasRecord {
                namespace,
                value,
                instrument,
                interval: AliasInterval {
                    valid_from,
                    valid_to,
                },
                source,
            });
        }
        Ok(aliases)
    }

    /// New version of the circuit composition.
    ///
    /// The version is immutable: changing the composition creates a new row rather than an UPDATE.
    /// This is enforced by a trigger in the schema, not only by this method.
    pub fn insert_contour_version(
        &mut self,
        owner: OwnerId,
        definition: &ContourDefinition,
        title: &str,
        accounts: &[AccountId],
    ) -> Result<(), StoreError> {
        let transaction = self.conn.transaction()?;
        transaction.execute(
            "INSERT INTO contour_versions (owner, contour, version, title, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                owner.inner().to_string(),
                definition.id().0.to_string(),
                definition.version().0,
                title,
                now(),
            ],
        )?;
        for account in accounts {
            // The foreign key on (owner, account) will reject someone else's account:
            // a circuit made up of someone else's accounts is access to someone else's money,
            // not an input error.
            transaction.execute(
                "INSERT INTO contour_accounts (owner, contour, version, account)
                 VALUES (?1, ?2, ?3, ?4)",
                params![
                    owner.inner().to_string(),
                    definition.id().0.to_string(),
                    definition.version().0,
                    account.inner().to_string(),
                ],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    /// List each owned contour once, at its latest version.
    ///
    /// The query starts from contour versions rather than membership rows so an
    /// empty contour remains visible; `load_contour` intentionally cannot
    /// distinguish that case from a missing version.
    ///
    /// The title comes from the row the aggregate selected, joined back rather
    /// than picked out of the grouped set: SQLite would hand back the title of
    /// an arbitrary version otherwise, and the name of a superseded composition
    /// is exactly the wrong answer for a caller checking what the perimeter is
    /// called now.
    pub fn list_contours(&self, owner: OwnerId) -> Result<Vec<ContourRecord>, StoreError> {
        let mut statement = self.conn.prepare(
            "SELECT current.contour, current.version, current.title
             FROM contour_versions AS current
             JOIN (SELECT contour, MAX(version) AS version
                   FROM contour_versions
                   WHERE owner = ?1
                   GROUP BY contour) AS latest
               ON latest.contour = current.contour AND latest.version = current.version
             WHERE current.owner = ?1
             ORDER BY current.contour",
        )?;
        let rows = statement.query_map([owner.inner().to_string()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, u32>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        let mut contours = Vec::new();
        for row in rows {
            let (id, version, title) = row?;
            contours.push(ContourRecord {
                id: ContourId(parse_uuid(&id, "contour")?),
                owner,
                version: ContourVersion(version),
                title,
            });
        }
        Ok(contours)
    }

    /// Circuit composition at a version **for the specified owner**.
    ///
    /// The owner is part of the query rather than checked afterward: a circuit
    /// identifier is a UUID, and a UUID is not an access right (§14).
    pub fn load_contour(
        &self,
        owner: OwnerId,
        contour: ContourId,
        version: ContourVersion,
    ) -> Result<Option<ContourDefinition>, StoreError> {
        let mut statement = self.conn.prepare(
            "SELECT account FROM contour_accounts
             WHERE owner = ?1 AND contour = ?2 AND version = ?3",
        )?;
        let rows = statement.query_map(
            params![owner.inner().to_string(), contour.0.to_string(), version.0],
            |row| row.get::<_, String>(0),
        )?;
        let mut accounts = Vec::new();
        for row in rows {
            accounts.push(AccountId(parse_uuid(&row?, "contour_account")?));
        }
        if accounts.is_empty() {
            return Ok(None);
        }
        Ok(Some(ContourDefinition::new(contour, version, accounts)))
    }

    /// Record, or replace, the owner's decision that an account is outside every
    /// contour.
    ///
    /// An upsert rather than an insert: a disposition is a current statement,
    /// not a history, and the owner restating it with a better reason must not
    /// fail. The `WHERE` clause on the conflict branch is the one accounts and
    /// custody places already carry — without it a request naming a guessed
    /// identifier would overwrite another owner's statement (§14).
    pub fn record_account_scope_exclusion(
        &self,
        owner: OwnerId,
        exclusion: &AccountScopeExclusionRecord,
    ) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT INTO account_scope_exclusions (owner, account, reason, recorded_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (owner, account) DO UPDATE SET
                 reason = excluded.reason,
                 recorded_at = excluded.recorded_at
             WHERE account_scope_exclusions.owner = excluded.owner",
            params![
                owner.inner().to_string(),
                exclusion.account.inner().to_string(),
                exclusion.reason,
                now(),
            ],
        )?;
        Ok(())
    }

    /// Withdraw the statement, returning the account to «awaiting a decision».
    ///
    /// Deleting the row rather than writing a third value: the absence of a row
    /// is what «undecided» means everywhere else in this table, and two ways of
    /// spelling one state is how they disagree.
    pub fn clear_account_scope_exclusion(
        &self,
        owner: OwnerId,
        account: AccountId,
    ) -> Result<(), StoreError> {
        self.conn.execute(
            "DELETE FROM account_scope_exclusions WHERE owner = ?1 AND account = ?2",
            params![owner.inner().to_string(), account.inner().to_string()],
        )?;
        Ok(())
    }

    /// Record, or replace, the owner's statement about one account's transfer
    /// partners.
    ///
    /// Replacing rather than merging: the statement is «these, and no others»,
    /// and a call that only added would leave the owner unable to withdraw a
    /// partner he named by mistake. An empty list is a statement too — «money
    /// moves between this account and none of my others» — and it is why the
    /// statement row exists apart from the partner rows.
    pub fn record_account_transfer_statement(
        &mut self,
        owner: OwnerId,
        account: AccountId,
        partners: &[AccountId],
    ) -> Result<(), StoreError> {
        let transaction = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        write_transfer_statement(&transaction, owner, account, partners)?;
        transaction.commit()?;
        Ok(())
    }

    /// Record, or replace, several of those statements in one transaction.
    ///
    /// Transport, not meaning. Each entry is the same complete enumeration the
    /// single-account form records, and stating one account's partners still
    /// says nothing about any other account's — the relation is directed here
    /// because the question «these, and no others» is answerable only per
    /// account.
    ///
    /// What the batch adds is atomicity, and it is why the loop the caller could
    /// have written is not good enough: [`Self::record_account_transfer_statement`]
    /// commits per call, so a failure on the fifth of twelve would leave four
    /// statements replaced and eight standing as they were — the owner having
    /// half-said something he was saying all at once. Everything here lands in
    /// one immediate transaction or none of it does.
    ///
    /// An empty batch is a no-op rather than an error.
    pub fn record_account_transfer_statements(
        &mut self,
        owner: OwnerId,
        statements: &[AccountTransferStatementRecord],
    ) -> Result<(), StoreError> {
        if statements.is_empty() {
            return Ok(());
        }
        let transaction = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        for statement in statements {
            write_transfer_statement(&transaction, owner, statement.account, &statement.partners)?;
        }
        transaction.commit()?;
        Ok(())
    }

    /// Withdraw the statement, returning the account to «awaiting a decision».
    ///
    /// The partner rows go with it: the absence of a statement row is what
    /// «undecided» means, and partners left behind would be an answer nobody
    /// currently stands behind.
    pub fn clear_account_transfer_statement(
        &mut self,
        owner: OwnerId,
        account: AccountId,
    ) -> Result<(), StoreError> {
        let transaction = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute(
            "DELETE FROM account_transfer_partners WHERE owner = ?1 AND account = ?2",
            params![owner.inner().to_string(), account.inner().to_string()],
        )?;
        transaction.execute(
            "DELETE FROM account_transfer_statements WHERE owner = ?1 AND account = ?2",
            params![owner.inner().to_string(), account.inner().to_string()],
        )?;
        transaction.commit()?;
        Ok(())
    }

    /// Every statement the owner has made, with the partners each names.
    ///
    /// A statement with no partners is returned with an empty list rather than
    /// omitted: it is the answer «none of my others», and dropping it here
    /// would put the account back into the queue the owner has already answered.
    pub fn list_account_transfer_statements(
        &self,
        owner: OwnerId,
    ) -> Result<Vec<AccountTransferStatementRecord>, StoreError> {
        let mut statement = self.conn.prepare(
            "SELECT account FROM account_transfer_statements
             WHERE owner = ?1 ORDER BY account",
        )?;
        let rows =
            statement.query_map([owner.inner().to_string()], |row| row.get::<_, String>(0))?;
        let mut statements = Vec::new();
        for row in rows {
            statements.push(AccountTransferStatementRecord {
                account: AccountId(parse_uuid(&row?, "account")?),
                partners: Vec::new(),
            });
        }

        let mut partners = self.conn.prepare(
            "SELECT account, partner FROM account_transfer_partners
             WHERE owner = ?1 ORDER BY account, partner",
        )?;
        let rows = partners.query_map([owner.inner().to_string()], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        for row in rows {
            let (account, partner) = row?;
            let account = AccountId(parse_uuid(&account, "account")?);
            let partner = AccountId(parse_uuid(&partner, "account")?);
            if let Some(found) = statements
                .iter_mut()
                .find(|statement| statement.account == account)
            {
                found.partners.push(partner);
            }
        }
        Ok(statements)
    }

    pub fn list_account_scope_exclusions(
        &self,
        owner: OwnerId,
    ) -> Result<Vec<AccountScopeExclusionRecord>, StoreError> {
        let mut statement = self.conn.prepare(
            "SELECT account, reason FROM account_scope_exclusions
             WHERE owner = ?1 ORDER BY account",
        )?;
        let rows = statement.query_map([owner.inner().to_string()], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut exclusions = Vec::new();
        for row in rows {
            let (account, reason) = row?;
            exclusions.push(AccountScopeExclusionRecord {
                account: AccountId(parse_uuid(&account, "account")?),
                reason,
            });
        }
        Ok(exclusions)
    }

    /// Highest circuit version. Report without an explicitly specified version
    /// is calculated from the last one—and writes it to the applied rules.
    pub fn latest_contour_version(
        &self,
        owner: OwnerId,
        contour: ContourId,
    ) -> Result<Option<ContourVersion>, StoreError> {
        let version: Option<u32> = self.conn.query_row(
            "SELECT MAX(version) FROM contour_versions WHERE owner = ?1 AND contour = ?2",
            params![owner.inner().to_string(), contour.0.to_string()],
            |row| row.get(0),
        )?;
        Ok(version.map(ContourVersion))
    }
}

/// Both halves of an external identity, or neither.
///
/// The partial uniqueness index binds only rows that carry both columns, so a
/// row carrying one of them is not an identity and must not be read as one.
fn external_identity(
    provider: Option<String>,
    provider_account_id: Option<String>,
) -> Option<AccountIdentity> {
    match (provider, provider_account_id) {
        (Some(provider), Some(provider_account_id)) => Some(AccountIdentity {
            provider,
            provider_account_id,
        }),
        _ => None,
    }
}

/// A stored class code, refusing one this build does not know.
///
/// An unrecognised code is an error rather than `None`: `None` means "the owner
/// has not said", and reading a value the owner did say as a value he did not
/// would put a wrong heading on a report he is meant to trust.
fn parse_cash_class(code: Option<&str>) -> Result<Option<CashAssetClass>, StoreError> {
    code.map(|code| {
        CashAssetClass::from_code(code).ok_or_else(|| StoreError::InvalidValue {
            field: "accounts.cash_class",
            value: code.to_owned(),
        })
    })
    .transpose()
}

/// A stored expectation code, refusing one this build does not know.
///
/// An unrecognised code is an error rather than `None`, for the reason
/// [`parse_cash_class`] gives: `None` means «the owner has not said», and
/// reading a statement he did make as one he did not would drop a warning he
/// asked for.
fn parse_negative_balance_expectation(
    code: Option<&str>,
) -> Result<Option<NegativeBalanceExpectation>, StoreError> {
    code.map(|code| {
        NegativeBalanceExpectation::from_code(code).ok_or_else(|| StoreError::InvalidValue {
            field: "accounts.negative_balance_expectation",
            value: code.to_owned(),
        })
    })
    .transpose()
}

/// One account with its identity, class and aliases, read inside a transaction.
fn read_account_detail(
    transaction: &rusqlite::Transaction<'_>,
    owner: OwnerId,
    id: AccountId,
) -> Result<AccountDetailRecord, StoreError> {
    let (title, institution, provider, provider_account_id, cash_class, expectation) = transaction
        .query_row(
            "SELECT title, institution, provider, provider_account_id, cash_class,
                    negative_balance_expectation
             FROM accounts WHERE owner = ?1 AND id = ?2",
            params![owner.inner().to_string(), id.inner().to_string()],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, Option<String>>(5)?,
                ))
            },
        )
        .optional()?
        .ok_or_else(|| StoreError::NotFound {
            what: "account",
            id: id.inner().to_string(),
        })?;

    let mut statement = transaction.prepare(
        "SELECT value, valid_from, valid_to FROM account_aliases
         WHERE owner = ?1 AND account = ?2 ORDER BY valid_from, value",
    )?;
    let rows = statement.query_map(
        params![owner.inner().to_string(), id.inner().to_string()],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
            ))
        },
    )?;
    let mut aliases = Vec::new();
    for row in rows {
        let (value, valid_from, valid_to) = row?;
        aliases.push(AccountAliasRecord {
            value,
            interval: AliasInterval {
                valid_from: text_to_date(&valid_from)?,
                valid_to: valid_to.as_deref().map(text_to_date).transpose()?,
            },
        });
    }

    Ok(AccountDetailRecord {
        id,
        owner,
        title,
        institution,
        identity: external_identity(provider, provider_account_id),
        cash_class: parse_cash_class(cash_class.as_deref())?,
        negative_balance_expectation: parse_negative_balance_expectation(expectation.as_deref())?,
        aliases,
    })
}

/// Write one transfer statement inside a transaction the caller owns.
///
/// The single-account form and the batch form share this body rather than each
/// carrying its own copy of the SQL: a second copy is a second thing to keep in
/// step, and the one that drifted would record a statement subtly unlike the
/// one the other records.
fn write_transfer_statement(
    conn: &rusqlite::Connection,
    owner: OwnerId,
    account: AccountId,
    partners: &[AccountId],
) -> Result<(), StoreError> {
    conn.execute(
        "INSERT INTO account_transfer_statements (owner, account, recorded_at)
         VALUES (?1, ?2, ?3)
         ON CONFLICT (owner, account) DO UPDATE SET recorded_at = excluded.recorded_at
         WHERE account_transfer_statements.owner = excluded.owner",
        params![
            owner.inner().to_string(),
            account.inner().to_string(),
            now(),
        ],
    )?;
    conn.execute(
        "DELETE FROM account_transfer_partners WHERE owner = ?1 AND account = ?2",
        params![owner.inner().to_string(), account.inner().to_string()],
    )?;
    for partner in partners {
        conn.execute(
            "INSERT OR IGNORE INTO account_transfer_partners (owner, account, partner)
             VALUES (?1, ?2, ?3)",
            params![
                owner.inner().to_string(),
                account.inner().to_string(),
                partner.inner().to_string(),
            ],
        )?;
    }
    Ok(())
}

fn parse_uuid(value: &str, what: &'static str) -> Result<uuid::Uuid, StoreError> {
    uuid::Uuid::parse_str(value).map_err(|_| StoreError::NotFound {
        what,
        id: value.to_owned(),
    })
}

type InstrumentParts = (
    Option<String>,
    String,
    String,
    String,
    String,
    String,
    Option<String>,
    Option<String>,
);

fn decode_instrument(
    id: InstrumentId,
    parts: InstrumentParts,
) -> Result<InstrumentRecord, StoreError> {
    let (kind, symbol, title, denomination, settlement, quote, parent, reason) = parts;
    let kind = kind
        .map(|value| {
            InstrumentKind::from_code(&value).ok_or_else(|| StoreError::NotFound {
                what: "instrument type",
                id: value,
            })
        })
        .transpose()?;
    let currencies = CurrencyRoles {
        denomination: CurrencyCode::from_code(&denomination).ok_or_else(|| {
            StoreError::NotFound {
                what: "instrument currency",
                id: denomination,
            }
        })?,
        settlement: CurrencyCode::from_code(&settlement).ok_or_else(|| StoreError::NotFound {
            what: "instrument currency",
            id: settlement,
        })?,
        quote: CurrencyCode::from_code(&quote).ok_or_else(|| StoreError::NotFound {
            what: "instrument currency",
            id: quote,
        })?,
    };
    let lineage = match (parent, reason) {
        (None, None) => None,
        (Some(parent), Some(reason)) => Some(Lineage {
            parent: InstrumentId(parse_uuid(&parent, "lineage_parent")?),
            reason: LineageReason::from_code(&reason).ok_or_else(|| StoreError::NotFound {
                what: "instrument origin reason",
                id: reason,
            })?,
        }),
        (parent, reason) => {
            return Err(StoreError::NotFound {
                what: "full instrument origin",
                id: format!("{parent:?}/{reason:?}"),
            });
        }
    };
    Ok(InstrumentRecord {
        id,
        kind,
        symbol,
        title,
        currencies,
        lineage,
    })
}
