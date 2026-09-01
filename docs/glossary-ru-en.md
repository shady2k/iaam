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
| замещение | conversion | |

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

## Input and import

| Russian | English | Note |
|---|---|---|
| выгрузка (банка) | bank export | the file or page the institution hands the owner |
| канал | channel | `SourceChannel`; how a fact reached us, not who stated it |
| объявленный источник | declared source | `SourceId::declared(owner, account, channel)` |
| маппинг колонок | column mapping | which export column feeds which field |
| ключ строки | row key | the stable identity that makes a re-import idempotent |
| нога (перевода) | leg | one of the two rows an internal transfer produces |
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
