# Контракты T-Invest API

Официальные protobuf-контракты Т-Инвестиций, положенные сюда как
**справочник**, а не как исходник сборки.

## Происхождение

Репозиторий `RussianInvestments/investAPI`, каталог
`src/docs/contracts`, коммит `3eaf23a25f598fe483c913184acdbd9132bc68d2`
от 7 ноября 2025 года.

```
BASE=https://raw.githubusercontent.com/RussianInvestments/investAPI/main/src/docs/contracts
for f in common.proto instruments.proto marketdata.proto operations.proto \
         orders.proto sandbox.proto signals.proto stoporders.proto users.proto; do
  curl -sSfL "$BASE/$f" -o "docs/api/tinkoff-invest/$f"
done
```

## Почему справочник, а не генерация кода

План E2 отказывается вносить gRPC-стек с генерацией protobuf в сборку:
HTTP/JSON-шлюз покрывает нужные методы, а лишний генератор кода — это
лишний способ разойтись с заслонами. Клиент разбирает JSON шлюза
вручную, а контракты нужны, чтобы имена полей, типы и значения
перечислений брались из документа, а не из памяти.

## Как читать вместе с JSON-шлюзом

Шлюз отдаёт те же сообщения в JSON по правилам protobuf JSON mapping:

| В контракте | В JSON шлюза |
|---|---|
| `account_id` | `accountId` — нижнее подчёркивание становится верблюжьим регистром |
| `int64` | строка (`"1000"`), а не число: JSON-число не хранит 64 бита точно |
| `google.protobuf.Timestamp` | строка RFC 3339 (`"2026-01-31T23:59:59Z"`) |
| `enum` | имя значения строкой (`"OPERATION_TYPE_BUY"`) |
| `MoneyValue` | `{"currency":"rub","units":"1000","nano":500000000}` |

`MoneyValue` и `Quotation` — целые: `units` плюс `nano` (миллиардные
доли). Двоичной плавающей точки в денежных величинах шлюза нет, и
появляться ей при разборе неоткуда — это ровно тот случай, ради
которого §6.6 запрещает `f64`.

**Дробная часть не округляется.** `nano`, не делящийся нацело на
минимальную единицу валюты, — это не «почти та же сумма», а величина,
которую проведённой суммой представить нельзя: такая строка получает
отказ (§3.4).

## Базовые адреса

| Контур | Адрес |
|---|---|
| песочница | `https://sandbox-invest-public-api.tbank.ru/rest` |
| боевой | `https://invest-public-api.tbank.ru/rest` |

Метод вызывается как `POST {база}/{полное имя сервиса}/{метод}`,
токен — заголовком `Authorization: Bearer …`.

Оба шлюза предъявляют сертификат удостоверяющего центра Минцифры;
корень доверия вшит в `crates/iaam-broker/certs` (см. README там же).

## Финам

Контракты Финама лягут рядом, в `docs/api/finam/`, когда дойдёт очередь
до задачи 19.
