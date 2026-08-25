//! Справочники: счета, инструменты, версии контуров.

use iaam_core::contour::{ContourDefinition, ContourId, ContourVersion};
use iaam_core::ids::{AccountId, CustodyId, InstrumentId, OwnerId, SourceId};
use iaam_core::instrument::{
    AliasInterval, AliasNamespace, CurrencyRoles, InstrumentKind, Lineage, LineageReason,
};
use iaam_core::money::CurrencyCode;
use rusqlite::{OptionalExtension, params};
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstrumentRecord {
    pub id: InstrumentId,
    /// `None` — род не установлен. Такой инструмент оценивается как
    /// неполный, а не как акция по умолчанию (§4.9, §5.4).
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

/// Смена внешнего кода инструмента.
///
/// Структура, а не шесть позиционных аргументов. Дело не только в пороге
/// clippy: `from` и `to` — два соседних `&str`, которые компилятор
/// переставленными местами не увидит, а результатом станет псевдоним,
/// заведённый задом наперёд (§15.1).
#[derive(Debug, Clone)]
pub struct AliasRename {
    pub namespace: AliasNamespace,
    /// Код, действующий до смены.
    pub from: String,
    /// Код, действующий с даты смены.
    pub to: String,
    /// Дата смены: старый интервал закрывается ею, новый ею же открывается.
    pub on: Date,
    pub instrument: InstrumentId,
    pub source: SourceId,
}

/// Дата в хранилище — ISO-8601, как и везде в схеме.
fn date_to_text(value: Date) -> String {
    value
        .format(&Iso8601::DATE)
        .expect("дата форматируется в ISO-8601")
}

fn text_to_date(value: &str) -> Result<Date, StoreError> {
    Date::parse(value, &Iso8601::DATE).map_err(|_| StoreError::NotFound {
        what: "дата псевдонима",
        id: value.to_owned(),
    })
}

impl SqliteStore {
    /// Создание или обновление счёта.
    ///
    /// Условие `WHERE accounts.owner = excluded.owner` обязательно:
    /// без него запрос с чужим (или угаданным) идентификатором
    /// переписывал бы название счёта другого владельца (§14).
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

    /// Создание или обновление места хранения.
    ///
    /// Условие `WHERE custody_places.owner = excluded.owner`
    /// обязательно по той же причине, что и у счетов: без него запрос
    /// с чужим идентификатором переписал бы место хранения другого
    /// владельца (§14).
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

    /// Смена внешнего кода: закрыть старый интервал, открыть новый.
    ///
    /// Одна транзакция обязательна: между двумя операциями старый код
    /// уже закрыт, а новый ещё не заведён, и параллельный резолвинг
    /// документа получил бы `Unknown` вместо инструмента.
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

    /// Инструмент по внешнему коду на дату.
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
                    .map_or_else(|| "открыт".to_owned(), date_to_text);
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
                    what: "пространство псевдонима",
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

    /// Новая версия состава контура.
    ///
    /// Версия неизменяема: изменение состава — новая строка, а не UPDATE.
    /// Это обеспечено триггером в схеме, а не только этим методом.
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
            // Внешний ключ на (owner, account) отклонит чужой счёт:
            // контур из чужих счетов — это доступ к чужим деньгам,
            // а не ошибка ввода.
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

    /// Состав контура на версии **для указанного владельца**.
    ///
    /// Владелец входит в запрос, а не проверяется после: идентификатор
    /// контура — это UUID, а UUID не является правом доступа (§14).
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

    /// Наибольшая версия контура. Отчёт без явно указанной версии
    /// считается по последней — и пишет её в применённые правила.
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
                what: "род инструмента",
                id: value,
            })
        })
        .transpose()?;
    let currencies = CurrencyRoles {
        denomination: CurrencyCode::from_code(&denomination).ok_or_else(|| {
            StoreError::NotFound {
                what: "валюта инструмента",
                id: denomination,
            }
        })?,
        settlement: CurrencyCode::from_code(&settlement).ok_or_else(|| StoreError::NotFound {
            what: "валюта инструмента",
            id: settlement,
        })?,
        quote: CurrencyCode::from_code(&quote).ok_or_else(|| StoreError::NotFound {
            what: "валюта инструмента",
            id: quote,
        })?,
    };
    let lineage = match (parent, reason) {
        (None, None) => None,
        (Some(parent), Some(reason)) => Some(Lineage {
            parent: InstrumentId(parse_uuid(&parent, "lineage_parent")?),
            reason: LineageReason::from_code(&reason).ok_or_else(|| StoreError::NotFound {
                what: "причина происхождения инструмента",
                id: reason,
            })?,
        }),
        (parent, reason) => {
            return Err(StoreError::NotFound {
                what: "полное происхождение инструмента",
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
