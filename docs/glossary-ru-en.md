# Доменный словарь: русский → английский

Проект переводится на английский (бид `iaam-cyed`). Словарь фиксируется
**до** перевода, иначе один термин уедет тремя вариантами и код перестанет
искаться грепом.

Правило: если термина здесь нет, а он доменный — впиши сюда прежде, чем
использовать. Придуманный на месте синоним хуже неудачного, но общего.

## Деньги и стоимость

| Русский | Английский | Замечание |
|---|---|---|
| номинал (первоначальный) | face value | `initial_principal` в графике — уже прижилось, не переименовывать |
| номинал (непогашенный остаток) | principal | остаток = `remaining_principal` |
| возврат номинала | principal return | |
| доля возврата | returned share | тип `ReturnedShare` |
| доля разнесения | basis allocation | тип `BasisAllocation` |
| налоговая стоимость | cost basis | |
| историческая стоимость приобретения | acquisition basis | |
| освобождённая стоимость | released basis | |
| реализованный результат | realised result | британское `-ised`, как уже в коде |
| денежный поток | cashflow | |
| НКД | accrued interest | |
| купон | coupon | |
| выплата (плановая) | posting | `ExpectedPosting`, `PostingKind` |
| компенсация | compensation | |
| минимальная единица валюты | minor unit | |

## Бумаги и позиция

| Русский | Английский | Замечание |
|---|---|---|
| бумага, инструмент | instrument | «бумага» в прозе → security |
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
| оферта | offer | вид права — `offer right` |
| погашение | redemption | частичное — partial redemption |
| амортизация | amortisation | британское `-isation`, как уже в коде |
| замещение | conversion | |

## Журнал и проекция

| Русский | Английский | Замечание |
|---|---|---|
| журнал | journal | append-only журнал событий |
| событие | event | |
| факт | fact | |
| приёмка | ingest | |
| нормализация | normalisation | |
| вердикт | verdict | |
| проекция | projection | |
| снимок | snapshot | |
| срез (журнала) | slice | |
| отпечаток | digest / fingerprint | `prefix_digest`, `fingerprint` — не смешивать |
| дельта | delta | |
| порядок (внутри дня) | effective order | |

## Даты

| Русский | Английский | Замечание |
|---|---|---|
| дата сделки | trade date | |
| дата расчётов | settlement date | |
| дата зачисления денег | cash posted date | |
| дата фиксации реестра | record date | |
| дата вступления в силу | effective date | |
| координата знания | knowledge coordinate | |
| на дату | as of | |

## Отказы и качество

| Русский | Английский | Замечание |
|---|---|---|
| отказ | refusal | отказ вычислить; не `error`, если это не ошибка |
| невычислимо | not computable | тип `NotComputable` |
| неизвестно | unknown | никогда не переводить как ноль |
| разрыв | gap | `BasisGap`, `AllocationGap` |
| сверка | reconciliation | |
| расхождение | discrepancy | |
| материальная проблема | material issue | |
| качество данных | data quality | |
| недоказуемо | unverifiable | |
| заслон | guard | скрипты в `scripts/` |
| инвариант | invariant | |
| порог | threshold | |

## Чего словарь не касается

- **Значения из источников.** `"Оферта"` в `iaam-market` — значение из
  ответа MOEX ISS, а не наш текст. Перевод ломает разбор.
- **Документы владельца.** `.internal/specs/`, `.internal/plans/`
  остаются русскими.
