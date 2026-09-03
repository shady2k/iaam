//! Reference data: accounts, instruments, and environment versions.

use iaam_core::contour::{ContourDefinition, ContourId, ContourVersion};
use iaam_core::ids::{AccountId, CustodyId, InstrumentId, OwnerId, SourceId};
use iaam_core::instrument::{
    AliasInterval, AliasNamespace, CurrencyRoles, InstrumentKind, Lineage, LineageReason,
};
use iaam_core::money::CurrencyCode;
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
        transaction.execute(
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
        transaction.execute(
            "DELETE FROM account_transfer_partners WHERE owner = ?1 AND account = ?2",
            params![owner.inner().to_string(), account.inner().to_string()],
        )?;
        for partner in partners {
            transaction.execute(
                "INSERT OR IGNORE INTO account_transfer_partners (owner, account, partner)
                 VALUES (?1, ?2, ?3)",
                params![
                    owner.inner().to_string(),
                    account.inner().to_string(),
                    partner.inner().to_string(),
                ],
            )?;
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
