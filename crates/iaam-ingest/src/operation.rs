//! Нормализованная операция и её превращение в событие журнала.

use iaam_core::dates::{
    CashPostedDate, EffectiveOrder, EventDates, PaidDate, SettledDate, TradeDate,
};
use iaam_core::event::kind::{EventKind, FeeOrigin, TradeSide};
use iaam_core::event::leg::Leg;
use iaam_core::event::provenance::{ParserVersion, Provenance, RawHash};
use iaam_core::event::{Confidence, Event, Relation, SCHEMA_VERSION};
use iaam_core::ids::{AccountId, CustodyId, EventId, InstrumentId, OwnerId, SourceId};
use iaam_core::money::{CurrencyCode, Money, PostedMinor, Quantity};
use iaam_core::numeric::decimal::Dec;
use iaam_core::valuation::PriceQuality;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::verdict::Rejection;

/// Версия разбора. Пишется в provenance: без неё нельзя отличить ошибку
/// источника от ошибки разбора, исправленной позже (§4.1).
pub const PARSER_VERSION: &str = "ingest/manual/1";

/// Даты операции. Все необязательны, кроме той, что делает операцию
/// датированной: событие без единой даты не попадает ни в один период.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct OperationDates {
    pub trade: Option<time::Date>,
    pub settled: Option<time::Date>,
    pub cash_posted: Option<time::Date>,
    pub paid: Option<time::Date>,
}

impl OperationDates {
    fn to_event_dates(self) -> EventDates {
        EventDates {
            trade: self.trade.map(TradeDate),
            settled: self.settled.map(SettledDate),
            cash_posted: self.cash_posted.map(CashPostedDate),
            entitlement: None,
            paid: self.paid.map(PaidDate),
            tax_period_override: None,
        }
    }
}

/// Что произошло. Величины **положительные**: знак определяет вид
/// операции, а не клиент.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum OperationKind {
    Deposit {
        amount_minor: i64,
        currency: CurrencyCode,
    },
    Withdrawal {
        amount_minor: i64,
        currency: CurrencyCode,
    },
    Transfer {
        to: AccountId,
        amount_minor: i64,
        currency: CurrencyCode,
    },
    Buy {
        instrument: InstrumentId,
        custody: CustodyId,
        quantity: Dec,
        gross_minor: i64,
        fee_minor: Option<i64>,
        accrued_interest_minor: Option<i64>,
        currency: CurrencyCode,
    },
    Sell {
        instrument: InstrumentId,
        custody: CustodyId,
        quantity: Dec,
        gross_minor: i64,
        fee_minor: Option<i64>,
        accrued_interest_minor: Option<i64>,
        currency: CurrencyCode,
    },
    Income {
        instrument: Option<InstrumentId>,
        gross_minor: i64,
        currency: CurrencyCode,
    },
    Fee {
        amount_minor: i64,
        currency: CurrencyCode,
        origin: FeeOrigin,
    },
    OpeningCash {
        amount_minor: i64,
        currency: CurrencyCode,
    },
    OpeningPosition {
        instrument: InstrumentId,
        custody: CustodyId,
        quantity: Dec,
        cost_basis_minor: Option<i64>,
        currency: CurrencyCode,
    },
    Valuation {
        instrument: InstrumentId,
        price: Dec,
        currency: CurrencyCode,
        quality: PriceQuality,
    },
}

/// Операция, пришедшая через API или из файла.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SubmittedOperation {
    pub account: AccountId,
    pub kind: OperationKind,
    pub dates: OperationDates,
    /// Ключ идемпотентности клиента (§10.6).
    pub idempotency_key: Option<String>,
    /// Идентификатор операции в источнике, если он есть.
    pub source_operation_id: Option<String>,
}

/// Готовое к записи событие плюс отпечаток сырой записи.
#[derive(Debug, Clone, PartialEq)]
pub struct Normalized {
    pub event: Event,
}

/// Контекст нормализации: кто владелец и из какого источника пришло.
///
/// Порядкового номера здесь нет намеренно: его назначает хранилище
/// в той же транзакции, что и вставку. Приёмка ставит номер `1`
/// как заведомо временный — хранилище его перезапишет (§4.8).
#[derive(Debug, Clone, Copy)]
pub struct NormalizationContext {
    pub owner: OwnerId,
    pub source: SourceId,
}

/// Превращение операции в событие журнала.
///
/// Возвращает отказ, а не паникует и не подставляет умолчания: строка
/// с непонятой операцией получает вердикт, а документ продолжает
/// разбираться (§10.1).
pub fn normalize(
    operation: &SubmittedOperation,
    context: NormalizationContext,
) -> Result<Normalized, Rejection> {
    let dates = operation.dates.to_event_dates();
    let day = dates.effective_date().ok_or_else(|| Rejection {
        field: "dates".into(),
        expected: "хотя бы одна дата: trade, settled, cash_posted или paid".into(),
        actual: "ни одной".into(),
    })?;

    let (kind, legs) = build(operation, &operation.kind)?;
    let raw_hash = fingerprint(operation);

    Ok(Normalized {
        event: Event {
            id: EventId::new_random(),
            schema_version: SCHEMA_VERSION,
            owner: context.owner,
            account: operation.account,
            kind,
            dates,
            // Временный номер: окончательный ставит хранилище.
            order: EffectiveOrder::new(day, 1),
            legs,
            provenance: {
                let base = Provenance::new(
                    context.source,
                    raw_hash,
                    ParserVersion(PARSER_VERSION.to_owned()),
                );
                match operation.source_operation_id.as_deref() {
                    Some(id) => base.with_source_operation_id(id),
                    None => base,
                }
            },
            relation: Relation::None,
            // `Confidence` описывает **значение**, а не сверку (§4.9):
            // владелец, вводящий пополнение вручную, знает его сумму.
            // Отсутствие независимого подтверждения — это утверждение
            // о счёте и интервале (§10.3), оно появится в E2 отдельной
            // сущностью и полем события не является.
            confidence: Confidence::Known,
            idempotency_key: operation.idempotency_key.clone(),
        },
    })
}

/// Отпечаток нормализованной записи (§10.6, ключ третьей силы).
fn fingerprint(operation: &SubmittedOperation) -> RawHash {
    let mut hasher = Sha256::new();
    hasher.update(operation.account.inner().as_bytes());
    hasher.update(format!("{:?}", operation.kind).as_bytes());
    hasher.update(format!("{:?}", operation.dates).as_bytes());
    let digest = hasher.finalize();
    let hex: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    // Длина и алфавит гарантированы SHA-256, поэтому разбор не может
    // отказать; но подставлять заглушку в случае отказа нельзя —
    // provenance без хеша не должно существовать.
    RawHash::parse(&hex).unwrap_or_else(|| {
        unreachable_hash();
    })
}

/// Отдельная функция вместо `unwrap`: `unwrap` на `Option` в этом месте
/// читался бы как «а вдруг», хотя вариант невозможен по построению.
fn unreachable_hash() -> ! {
    panic!("SHA-256 всегда даёт 64 шестнадцатеричных знака")
}

/// Перевод десятичной суммы в минимальные единицы **без округления**.
///
/// Сумма с большей точностью, чем минимальная единица валюты, — это
/// не «почти правильная» сумма, а неверные входные данные: округлив её,
/// система запишет факт, которого не было (§3.4).
pub fn to_minor_units(
    value: rust_decimal::Decimal,
    currency: CurrencyCode,
    field: &str,
) -> Result<i64, Rejection> {
    let scale = currency.minor_units();
    if value.scale() > scale {
        return Err(Rejection {
            field: field.to_owned(),
            expected: format!(
                "не более {scale} знаков после запятой для {}",
                currency.code()
            ),
            actual: value.to_string(),
        });
    }
    let factor = rust_decimal::Decimal::from(10_i64.pow(scale));
    let scaled = value
        .checked_mul(factor)
        .ok_or_else(|| Rejection {
            field: field.to_owned(),
            expected: "представимая сумма".into(),
            actual: value.to_string(),
        })?
        .normalize();
    i64::try_from(scaled.mantissa())
        .ok()
        .filter(|_| scaled.scale() == 0)
        .ok_or_else(|| Rejection {
            field: field.to_owned(),
            expected: "целое число минимальных единиц".into(),
            actual: scaled.to_string(),
        })
}

fn money(minor: i64, currency: CurrencyCode) -> Money {
    Money::new(PostedMinor::new(minor), currency)
}

/// Величина обязана быть положительной.
///
/// Имя поля и величина в отказе — те же, что прислал клиент: `amount`,
/// а не `amount_minor`, и `-5.00`, а не `-500`. Отказ, называющий
/// внутреннее имя и внутренние единицы, отправляет клиента чинить поле,
/// которого он не отправлял (§10.4).
fn positive(value: i64, field: &str, currency: CurrencyCode) -> Result<i64, Rejection> {
    if value > 0 {
        Ok(value)
    } else {
        Err(Rejection {
            field: field.to_owned(),
            expected: "положительная величина".into(),
            actual: money(value, currency).to_calc_dec().inner().to_string(),
        })
    }
}

/// Построение типа события и ног.
///
/// Диспетчер исчерпывающий: новый вид операции обязан сломать сборку.
fn build(
    operation: &SubmittedOperation,
    kind: &OperationKind,
) -> Result<(EventKind, Vec<Leg>), Rejection> {
    let account = operation.account;
    match kind {
        OperationKind::Deposit {
            amount_minor,
            currency,
        } => {
            let amount = money(positive(*amount_minor, "amount", *currency)?, *currency);
            Ok((
                EventKind::CashIn { amount },
                vec![Leg::cash(account, amount)],
            ))
        }
        OperationKind::Withdrawal {
            amount_minor,
            currency,
        } => {
            let amount = money(-positive(*amount_minor, "amount", *currency)?, *currency);
            Ok((
                EventKind::CashOut { amount },
                vec![Leg::cash(account, amount)],
            ))
        }
        OperationKind::Transfer {
            to,
            amount_minor,
            currency,
        } => {
            if *to == account {
                return Err(Rejection {
                    field: "to".into(),
                    expected: "счёт, отличный от счёта операции".into(),
                    actual: to.inner().to_string(),
                });
            }
            let amount = money(positive(*amount_minor, "amount", *currency)?, *currency);
            let outgoing = amount.checked_negate().map_err(|error| Rejection {
                field: "amount".into(),
                expected: "представимая сумма".into(),
                actual: error.to_string(),
            })?;
            Ok((
                EventKind::CashTransfer {
                    transfer_id: iaam_core::ids::TransferId::new_random(),
                    from: account,
                    to: *to,
                    amount,
                },
                vec![Leg::cash(account, outgoing), Leg::cash(*to, amount)],
            ))
        }
        OperationKind::Buy {
            instrument,
            custody,
            quantity,
            gross_minor,
            fee_minor,
            accrued_interest_minor,
            currency,
        } => {
            let gross = money(positive(*gross_minor, "amount", *currency)?, *currency);
            let fee = fee_money(*fee_minor, *currency)?;
            let accrued = fee_money(*accrued_interest_minor, *currency)?;
            let mut settlement = gross.amount().raw();
            settlement += accrued.map_or(0, |value| value.amount().raw());
            settlement += fee.map_or(0, |value| value.amount().raw());
            Ok((
                EventKind::Trade {
                    side: TradeSide::Buy,
                    instrument: *instrument,
                    quantity: Quantity(*quantity),
                    gross,
                    fee,
                    accrued_interest: accrued,
                },
                vec![
                    Leg::cash(account, money(-settlement, *currency)),
                    Leg::security(account, *custody, *instrument, Quantity(*quantity)),
                ],
            ))
        }
        OperationKind::Sell {
            instrument,
            custody,
            quantity,
            gross_minor,
            fee_minor,
            accrued_interest_minor,
            currency,
        } => {
            let gross = money(positive(*gross_minor, "amount", *currency)?, *currency);
            let fee = fee_money(*fee_minor, *currency)?;
            let accrued = fee_money(*accrued_interest_minor, *currency)?;
            let mut settlement = gross.amount().raw();
            settlement += accrued.map_or(0, |value| value.amount().raw());
            settlement -= fee.map_or(0, |value| value.amount().raw());
            let sold = quantity.checked_neg().map_err(|error| Rejection {
                field: "quantity".into(),
                expected: "представимое количество".into(),
                actual: error.to_string(),
            })?;
            Ok((
                EventKind::Trade {
                    side: TradeSide::Sell,
                    instrument: *instrument,
                    quantity: Quantity(*quantity),
                    gross,
                    fee,
                    accrued_interest: accrued,
                },
                vec![
                    Leg::cash(account, money(settlement, *currency)),
                    Leg::security(account, *custody, *instrument, Quantity(sold)),
                ],
            ))
        }
        OperationKind::Income {
            instrument,
            gross_minor,
            currency,
        } => {
            let gross = money(positive(*gross_minor, "amount", *currency)?, *currency);
            Ok((
                EventKind::Income {
                    instrument: *instrument,
                    gross,
                },
                vec![Leg::cash(account, gross)],
            ))
        }
        OperationKind::Fee {
            amount_minor,
            currency,
            origin,
        } => {
            let amount = money(-positive(*amount_minor, "amount", *currency)?, *currency);
            Ok((
                EventKind::Fee {
                    amount,
                    origin: *origin,
                },
                vec![Leg::fee(account, amount)],
            ))
        }
        OperationKind::OpeningCash {
            amount_minor,
            currency,
        } => {
            // Восстановленный остаток может быть отрицательным (§15.9),
            // поэтому нуля здесь не требуется, а знак берётся как есть.
            let amount = money(*amount_minor, *currency);
            Ok((
                EventKind::OpeningCash { amount },
                vec![Leg::cash(account, amount)],
            ))
        }
        OperationKind::OpeningPosition {
            instrument,
            custody,
            quantity,
            cost_basis_minor,
            currency,
        } => {
            let cost_basis = match cost_basis_minor {
                Some(value) => Some(money(positive(*value, "cost_basis", *currency)?, *currency)),
                None => None,
            };
            Ok((
                EventKind::OpeningPosition {
                    instrument: *instrument,
                    quantity: Quantity(*quantity),
                    cost_basis,
                },
                vec![Leg::security(
                    account,
                    *custody,
                    *instrument,
                    Quantity(*quantity),
                )],
            ))
        }
        OperationKind::Valuation {
            instrument,
            price,
            currency,
            quality,
        } => Ok((
            EventKind::Valuation {
                instrument: *instrument,
                price: *price,
                currency: *currency,
                quality: *quality,
            },
            vec![],
        )),
    }
}

/// Комиссия и НКД приходят положительными: знак задаёт `trade_settlement`
/// ядра, и дублировать это решение в приёмке нельзя.
fn fee_money(value: Option<i64>, currency: CurrencyCode) -> Result<Option<Money>, Rejection> {
    match value {
        None => Ok(None),
        Some(minor) => Ok(Some(money(positive(minor, "fee", currency)?, currency))),
    }
}
