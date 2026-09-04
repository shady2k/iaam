# Domain glossary: Russian → English

The codebase is English (see `CLAUDE.md`). This table is what the Russian
domain vocabulary was translated into, and it stays as the reference for
anything written from now on: one term must not travel under three names,
or the code stops being greppable.

If a domain term is missing here, add it before using it. A synonym
invented on the spot is worse than an awkward but shared one.

The Russian column is kept on purpose: the existing spec and plans under
`.internal/` are in Russian and are not being retranslated, so this table
is how you match a term in them to the code.

## Money and value

| Russian | English | Note |
|---|---|---|
| номинал (первоначальный) | face value | `initial_principal` in the schedule is established; do not rename |
| номинал (непогашенный остаток) | principal | the remainder is `remaining_principal` |
| возврат номинала | principal return | |
| доля возврата | returned share | the `ReturnedShare` type |
| доля разнесения | basis allocation | the `BasisAllocation` type |
| налоговая стоимость | cost basis | |
| историческая стоимость приобретения | acquisition basis | |
| освобождённая стоимость | released basis | |
| реализованный результат | realised result | British `-ised`, as already in the code |
| денежный поток | cashflow | |
| НКД | accrued interest | |
| купон | coupon | |
| выплата (плановая) | posting | `ExpectedPosting`, `PostingKind` |
| компенсация | compensation | |
| минимальная единица валюты | minor unit | |
| внесение средств | contribution | `FlowClass::ExternalIn`; money crossing into the contour |
| вывод средств | withdrawal | `FlowClass::ExternalOut` |
| доходность | return | never “yield”; `xirr_pre_tax` is the pre-tax return |
| ключевая ставка | key rate | `KeyRateObservation` |

## Securities and position

| Russian | English | Note |
|---|---|---|
| бумага, инструмент | instrument | in prose, prefer “security” |
| выпуск | issue | `issue_terms` |
| партия, лот | lot | |
| позиция | position | |
| количество | quantity | |
| депозитарий | custody | |
| счёт | account | |
| владелец | owner | |
| контур | contour | |
| периметр | perimeter | |
| график выплат | schedule | `BondSchedule` |
| оферта | offer | the right itself is `offer right` |
| погашение | redemption | partial one is a partial redemption |
| амортизация | amortisation | British `-isation`, as already in the code |
| замещение | conversion | `LineageReason::Conversion`; the instrument itself is a replacement bond |
| внешний код | external code | ISIN, ticker, MOEX `SECID`, FIGI, broker code; `AliasNamespace` |
| справочник инструментов | instrument catalogue | `crates/iaam-core/src/instrument.rs`; shared across all owners |
| справочник владельца | directory | `Directory`; the owner's own names for accounts and custody |
| род инструмента | instrument kind | `InstrumentKind`; may be unset, and that is not an error |
| валюта обязательства | denomination currency | |
| валюта расчётов | settlement currency | |
| валюта котировки | quote currency | |
| валюта отчёта | report currency | a property of the report, not of the instrument |

## Journal and projection

| Russian | English | Note |
|---|---|---|
| журнал | journal | the append-only event journal |
| событие | event | |
| факт | fact | |
| приёмка | ingest | |
| нормализация | normalisation | |
| вердикт | verdict | |
| проекция | projection | |
| снимок | snapshot | |
| срез (журнала) | slice | |
| отпечаток | digest / fingerprint | `prefix_digest` and `fingerprint` are different things; do not mix |
| дельта | delta | |
| порядок (внутри дня) | effective order | |
| состояние поручения | order state | `ChannelOrderState`; the broker channel's own state, not ours |
| ключ идемпотентности | idempotency key | `idempotency_key` |
| утверждение (владельца) | assertion | `assertions`; what the owner states — never a second source |
| восстановленное начало | reconstructed opening | a position that existed before the journal began |

## Dates

| Russian | English | Note |
|---|---|---|
| дата сделки | trade date | |
| дата расчётов | settlement date | |
| дата зачисления денег | cash posted date | |
| дата фиксации реестра | record date | |
| дата вступления в силу | effective date | |
| координата знания | knowledge coordinate | |
| на дату | as of | |
| дата приобретения | acquisition date | `acquisition_date` |
| граница владения | ownership boundary | drawn from the acquisition date; without it a posting is unverifiable |
| момент наблюдения | observation moment | `observed_at` |

## Input and import

| Russian | English | Note |
|---|---|---|
| выгрузка (банка) | bank export | the file or page the institution hands the owner |
| канал | channel | `SourceChannel`; how a fact reached us, not who stated it |
| объявленный источник | declared source | `SourceId::declared(owner, account, channel)`; what deduplication is scoped by |
| объявленный импорт | declared import | `ImportId::declared(owner, account, channel, label)`; one submission, and what an import correction is keyed on |
| метка импорта | import label | the caller's name for one import within an account and channel |
| маппинг колонок | column mapping | which export column feeds which field |
| ключ строки | row key | the stable identity that makes a re-import idempotent |
| нога (перевода) | leg | one of the two rows an internal transfer produces |
| нога движения | cash leg | `CashLeg`; one side of a cash movement offered to the pairing matcher, which may or may not turn out to be a transfer leg |
| скилл импорта | import skill | per-institution knowledge held as an agent skill, never as code |

## Refusals and quality

| Russian | English | Note |
|---|---|---|
| отказ | refusal | a refusal to compute; not `error` unless it really is one |
| невычислимо | not computable | the `NotComputable` type |
| неизвестно | unknown | never render as zero |
| разрыв | gap | `BasisGap`, `AllocationGap` |
| разрыв покрытия | coverage gap | `ImportCoverageGap`; refused rows leave named dimensions unconfirmed |
| сверка | reconciliation | |
| измерение (сверки) | dimension | `dimension`; the axis a reconciliation covers — cash, positions, income, tax basis. A verdict names one; it does not reconcile it |
| основание котировки | quotation basis | `QuotationBasis`; effective, recorded and proof status are three different things |
| граница полноты | completeness boundary | `complete_through` |
| выведенная (граница) | inferred | `Boundary::InferredAcrossNonTradingDays`; not “derived” for this |
| расхождение | discrepancy | |
| материальная проблема | material issue | |
| качество данных | data quality | |
| недоказуемо | unverifiable | |
| заслон | guard | the scripts under `scripts/` |
| инвариант | invariant | |
| порог | threshold | |
| карантин | quarantine | the `Quarantined` row: neither a fact nor lost |

## Transport and infrastructure

| Russian | English | Note |
|---|---|---|
| эталон | reference implementation | an independent implementation used for parity checks |
| списание лотов | lot disposal | |
| якорь доверия | trust anchor | |
| узел | endpoint | an external HTTP destination |
| шлюз | gateway | |
| среда | environment | prod or sandbox |
| перешифровка | re-encryption | |
| ротация | rotation | |

## What this glossary does not cover

- **Values that come from a source.** `"Оферта"` in `iaam-market` is a
  MOEX ISS value, not our text; translating it breaks parsing. The same
  goes for broker report sheet and column names.
- **Existing Russian documents.** `docs/`, `README.md` and `.internal/`
  are left as they are. New documents are written in English.
