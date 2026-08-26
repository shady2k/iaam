# E3.2 часть 1 — транспорт: план реализации

> **For agentic workers:** REQUIRED SUB-SKILL: Use beads-superpowers:subagent-driven-development (recommended) or beads-superpowers:executing-plans to implement this plan task-by-task. Each Task becomes a bead (`bd create -t task --parent <epic-id>`). Steps within tasks use checkbox (`- [ ]`) syntax for human readability.

**Goal:** Исходящий HTTP, политика доверия и устойчивость живут в одной крейте `iaam-http`; ни `iaam-broker`, ни будущий `iaam-market` не зависят от `reqwest`, и это проверяется заслоном, а не договорённостью.

**Architecture:** `iaam-http` отдаёт конкретный `HttpClient` поверх собственных типов `HttpRequest`/`HttpResponse` — объектобезопасного трейта здесь быть не может, правило 10 `check-architecture.sh` разрешает `async_trait` только в `iaam-app`. Крейты источников строят описание запроса и разбирают тело ответа; обе эти операции чисты и проверяются без сети. Политика доверия задаётся одной таблицей назначений: шлюз Т-Инвестиций — на вшитом корне Минцифры, публичные узлы — на веб-корнях.

**Tech Stack:** Rust 1.98.0 (закреплён `rust-toolchain.toml`), `reqwest` 0.13 с `rustls` и `default-features = false`, `thiserror`, `tokio` в dev. Окружение — `nix develop`.

**Спецификация:** `.internal/specs/2026-08-26-e3-2-market-data-design.md`, разделы 2.1–2.4, 6.1, 9.1
**Спека проекта:** `.internal/specs/2026-08-22-investment-tracker-design.md`, §3.1, §3.2, §14

## Global Constraints

- **Все команды идут через `nix develop -c`.** Снаружи окружения соберётся не тот тулчейн.
- **Воркеры не запускают тяжёлые прогоны.** Разрешено: `cargo check -p <крейт>`, `cargo test -p <крейт> <фильтр>`, `cargo fmt --all`. Запрещено: `cargo mutants`, `cargo llvm-cov`, полный `make check` — они идут один раз в конце эпика у оркестратора.
- **Проза русская, имена тестов английские.** Так написан весь существующий код: doc-комментарии по-русски со ссылками на параграфы спеки, тесты вида `a_pinned_anchor_is_used_only_for_the_gateway_that_needs_it`.
- **`rustfmt.toml` задаёт `fn_call_width = 60` при `max_width = 100`.** Перед каждым коммитом обязателен `nix develop -c cargo fmt --all`.
- **`async_trait` — только в `iaam-app`.** Правило 10 `scripts/check-architecture.sh`. В `iaam-http` его быть не может; используется конкретный тип.
- **`[lints] workspace = true` обязателен в манифесте каждой крейты.** Правило 7 `check-architecture.sh`: крейта без этой секции молча выпадает из-под запрета `unsafe`.
- **Проверка подлинности TLS не отключается нигде.** `danger_accept_invalid_certs` и любые послабления запрещены. Меняется только то, откуда берётся якорь.
- **Секрет не попадает ни в `Debug`, ни в текст ошибки.** Образец — `TinkoffError` в `crates/iaam-broker/src/tinkoff/client.rs:13`: варианты намеренно не несут токен.
- **Правки файлов политики разрешены владельцем 2026-08-26** для этого эпика: `Cargo.toml` (members), `scripts/check-architecture.sh` (новое правило). PR получает метку `policy-change`, обоснование — в биде задачи. Прочие файлы политики не трогать.
- **Ослабление теста ради прохождения запрещено (§15.7).** Расхождение исправляется в пользу компилятора; если тест приходится ослабить — остановка и эскалация.
- **Поведение брокера меняться не должно.** Существующие тесты `iaam-broker` проходят без правок их утверждений. Если тест приходится изменить — остановка и эскалация: это признак того, что переселение поменяло смысл, а не адрес.

---

## Карта файлов

| Файл | Ответственность | Задача |
|---|---|---|
| `crates/iaam-http/Cargo.toml` | манифест новой крейты | 1 |
| `crates/iaam-http/src/lib.rs` | объявляет все модули; после задачи 1 не правится | 1 |
| `crates/iaam-http/src/destination.rs` | `Destination` — перечень внешних узлов и их базы | 1 |
| `crates/iaam-http/src/request.rs` | `HttpRequest`, `HttpMethod`, сборка URL и строки запроса | 1 |
| `crates/iaam-http/src/response.rs` | `HttpResponse`, `HttpError` | 1 |
| `Cargo.toml` | `members` воркспейса | 1 |
| `crates/iaam-http/certs/russian-trusted-root-ca.pem` | вшитый корень Минцифры (переезд) | 2 |
| `crates/iaam-http/certs/README.md` | происхождение файла и порядок замены (переезд) | 2 |
| `crates/iaam-http/src/trust.rs` | таблица назначений и якорей, сборка клиентов | 2 |
| `crates/iaam-http/src/client.rs` | `HttpClient::send`, таймауты | 3 |
| `crates/iaam-http/src/resilience.rs` | решение о повторе, backoff, ограничение частоты | 4 |
| `crates/iaam-broker/src/tinkoff/client.rs` | переселён на `iaam-http` | 5 |
| `crates/iaam-broker/src/finam/client.rs` | переселён на `iaam-http` | 6 |
| `crates/iaam-broker/src/trust.rs` | **удаляется** | 6 |
| `crates/iaam-broker/Cargo.toml` | `reqwest` удаляется, добавляется `iaam-http` | 6 |
| `scripts/check-architecture.sh` | правило 11: `reqwest` только в `iaam-http` | 7 |
| `.internal/specs/2026-08-22-investment-tracker-design.md` | граф §3.2: `iaam-http` и пропавший `iaam-broker` | 7 |

**Порядок и параллельность.** Задача 1 первая. Задачи 2, 3, 4 стоят на ней и между собой независимы — идут параллельно. Задачи 5 и 6 стоят на 2–4 и независимы между собой — идут параллельно. Задача 7 последняя: заслон, добавленный раньше, покраснеет на ещё не переселённом брокере.

---

### Task 1: Крейта `iaam-http` — типы запроса и ответа

**Files:**
- Create: `crates/iaam-http/Cargo.toml`
- Create: `crates/iaam-http/src/lib.rs`
- Create: `crates/iaam-http/src/destination.rs`
- Create: `crates/iaam-http/src/request.rs`
- Create: `crates/iaam-http/src/response.rs`
- Modify: `Cargo.toml:3` (members)

**Interfaces:**
- Produces: `Destination::{TinkoffProd, TinkoffSandbox, FinamApi, MoexIss, CbrScripts, CbrDailyInfo}`; `Destination::base_url(self) -> &'static str`; `Destination::ALL: [Self; 6]`; `HttpMethod::{Get, Post}`; `HttpRequest::{get, post, with_query, with_bearer, with_soap_action, url, destination, method, body, bearer, soap_action}`; `RequestBody::{Json(String), Xml(String)}` с `content_type()` и `payload()`; `Secret::{new, expose}`; `HttpResponse { status: u16, body: Vec<u8> }` с `text_utf8()`; `HttpError::{Network, Timeout, ClientNotBuilt(String), TrustAnchorNotParsed(String)}`.
- Якорь доверия (`Anchors`, `Destination::anchors`) здесь **не объявляется** — он в задаче 2. Разделение намеренное: база узла нужна для сборки URL, якорь — только для сборки клиента.

**Acceptance Criteria:**
- Крейта собирается и входит в воркспейс; `[lints] workspace = true` присутствует.
- `HttpRequest::url()` склеивает базу назначения, путь и строку запроса без двойных и потерянных слэшей.
- Пустая строка запроса не даёт висящего `?`.
- Значения строки запроса экранируются: пробел и кириллица не уезжают в URL сырыми.
- Ни `HttpRequest`, ни `HttpError` не печатают предъявленный секрет в `Debug`.
- Песочница и бой Т-Инвестиций — **разные назначения**: у них разные хосты, а не разные пути.

- [ ] **Step 1: Завести манифест и место в воркспейсе**

`crates/iaam-http/Cargo.toml`:

```toml
[package]
name = "iaam-http"
version = "0.1.0"
edition.workspace = true
license.workspace = true
rust-version.workspace = true

[dependencies]
# Единственное место в дереве, где объявлен HTTP-клиент. rustls без
# встроенных корней там, где якорь задан явно; веб-корни там, где узел
# публичный. Выбор делает таблица назначений, а не клиент.
reqwest = { version = "0.13", default-features = false, features = ["json", "rustls"] }
thiserror = "2"
# Экранирование значений строки запроса. Своя реализация процентного
# кодирования — лишнее место для ошибки в том, что уже решено.
percent-encoding = "2"
# Секрет зануляется при уничтожении: копия токена, оставшаяся
# в освобождённой памяти, — это тот же токен.
zeroize = { version = "1", features = ["alloc"] }

[dev-dependencies]
tokio = { version = "1", features = ["rt-multi-thread", "macros"] }
sha2 = "0.11"

[lints]
workspace = true
```

В корневом `Cargo.toml` строка 3 — добавить `"crates/iaam-http"` первым элементом (крейта ни от кого не зависит):

```toml
members = ["crates/iaam-http", "crates/iaam-core", "crates/iaam-oracle", "crates/iaam-store", "crates/iaam-ingest", "crates/iaam-broker", "crates/iaam-app", "crates/iaam-server", "crates/iaam-bootstrap"]
```

Это правка файла политики. Разрешение владельца получено 2026-08-26; обоснование записать в бид задачи.

- [ ] **Step 2: Объявить назначения**

`crates/iaam-http/src/destination.rs`:

```rust
//! Внешние узлы, к которым ходит программа.
//!
//! Перечисление исчерпаемо и **без** `#[non_exhaustive]` намеренно
//! (§15.1): новый источник обязан сломать сборку и здесь, и в таблице
//! якорей (`trust.rs`), чтобы про его доверие нельзя было забыть.

/// Внешний узел.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Destination {
    /// Боевой шлюз Т-Инвестиций.
    TinkoffProd,
    /// Песочница Т-Инвестиций. Отдельное назначение, а не отдельный путь:
    /// у песочницы **другой хост**, и подставить её, обрезав базу боевого
    /// адреса, нельзя.
    TinkoffSandbox,
    FinamApi,
    MoexIss,
    /// Простые XML-скрипты ЦБ: курсы на дату и за период.
    CbrScripts,
    /// SOAP-сервис ЦБ: ключевая ставка и прочие датированные ряды.
    CbrDailyInfo,
}

impl Destination {
    /// Все назначения. Существует ради тестов, проходящих по таблице
    /// целиком: тест, перечисляющий варианты вручную, устаревает молча.
    pub const ALL: [Self; 6] = [
        Self::TinkoffProd,
        Self::TinkoffSandbox,
        Self::FinamApi,
        Self::MoexIss,
        Self::CbrScripts,
        Self::CbrDailyInfo,
    ];

    /// База узла.
    ///
    /// Значения сверены с `crates/iaam-broker/src/environment.rs:53`
    /// и `crates/iaam-broker/src/finam/client.rs`. Домен шлюза —
    /// `tbank.ru`, а не `tinkoff.ru`.
    #[must_use]
    pub const fn base_url(self) -> &'static str {
        match self {
            Self::TinkoffProd => "https://invest-public-api.tbank.ru/rest",
            Self::TinkoffSandbox => "https://sandbox-invest-public-api.tbank.ru/rest",
            Self::FinamApi => "https://api.finam.ru",
            Self::MoexIss => "https://iss.moex.com",
            Self::CbrScripts | Self::CbrDailyInfo => "https://www.cbr.ru",
        }
    }
}
```

- [ ] **Step 3: Написать падающий тест на сборку URL**

`crates/iaam-http/src/request.rs`, в конец файла:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_url_joins_base_and_path_without_doubling_the_slash() {
        let request = HttpRequest::get(Destination::MoexIss, "/iss/history.json");
        assert_eq!(request.url(), "https://iss.moex.com/iss/history.json");
    }

    #[test]
    fn an_empty_query_leaves_no_dangling_question_mark() {
        let request = HttpRequest::get(Destination::MoexIss, "/iss/history.json");
        assert!(!request.url().contains('?'));
    }

    #[test]
    fn query_values_are_percent_encoded() {
        let request = HttpRequest::get(Destination::CbrScripts, "/scripts/XML_daily.asp")
            .with_query("name", "Австралийский доллар")
            .with_query("range", "a b");
        let url = request.url();
        assert!(!url.contains(' '), "пробел обязан быть экранирован: {url}");
        assert!(
            !url.contains('Д'),
            "кириллица обязана быть экранирована: {url}"
        );
        assert!(url.contains("range=a%20b"), "{url}");
    }

    #[test]
    fn a_bearer_secret_never_appears_in_debug_output() {
        let request = HttpRequest::post(
            Destination::TinkoffProd,
            "/OperationsService/GetOperationsByCursor",
            RequestBody::Json("{}".to_owned()),
        )
        .with_bearer("t.SUPER-SECRET-VALUE");
        let printed = format!("{request:?}");
        assert!(
            !printed.contains("SUPER-SECRET-VALUE"),
            "секрет утёк в Debug: {printed}"
        );
    }

    #[test]
    fn the_sandbox_is_a_different_host_not_a_different_path() {
        assert_ne!(
            Destination::TinkoffProd.base_url(),
            Destination::TinkoffSandbox.base_url()
        );
        assert!(Destination::TinkoffSandbox.base_url().contains("sandbox"));
        assert!(!Destination::TinkoffProd.base_url().contains("sandbox"));
    }

    #[test]
    fn every_destination_serves_https() {
        for destination in Destination::ALL {
            assert!(
                destination.base_url().starts_with("https://"),
                "{destination:?} ходит не по HTTPS"
            );
        }
    }
}
```

- [ ] **Step 4: Убедиться, что тест не собирается**

Run: `nix develop -c cargo test -p iaam-http --lib`
Expected: FAIL, `cannot find type HttpRequest in this scope`.

- [ ] **Step 5: Реализовать типы запроса**

`crates/iaam-http/src/request.rs`:

```rust
//! Описание исходящего запроса (§3.1).
//!
//! Описание запроса — данные, а не действие: оно строится и проверяется
//! без сети, и именно поэтому крейты источников могут не знать транспорта
//! вовсе. Отправкой занимается `HttpClient`.

use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
use zeroize::Zeroizing;

use crate::destination::Destination;

/// Метод запроса. Расширяется по потребности: варианты, которых ни один
/// источник не использует, — это непроверенный код.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HttpMethod {
    Get,
    Post,
}

/// Тело запроса вместе с типом содержимого: тип нельзя забыть выставить,
/// потому что он не отделён от тела.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RequestBody {
    Json(String),
    /// Конверт SOAP тоже приходит сюда: отдельного варианта он не требует,
    /// от XML его отличает лишь заголовок `SOAPAction` (см. `soap_action`).
    Xml(String),
}

impl RequestBody {
    #[must_use]
    pub const fn content_type(&self) -> &'static str {
        match self {
            Self::Json(_) => "application/json",
            Self::Xml(_) => "text/xml; charset=utf-8",
        }
    }

    #[must_use]
    pub fn payload(&self) -> &str {
        match self {
            Self::Json(body) | Self::Xml(body) => body,
        }
    }
}

/// Предъявляемый секрет.
///
/// `Debug` написан вручную и печатает заглушку. Производный `Debug`
/// напечатал бы токен в первом же логе отказа, а `Zeroizing` затирает
/// копию при уничтожении.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(Zeroizing<String>);

impl Secret {
    #[must_use]
    pub fn new(value: &str) -> Self {
        Self(Zeroizing::new(value.to_owned()))
    }

    /// Единственная точка, где секрет обращается в строку. Названа так,
    /// чтобы вызов был заметен на ревью.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl core::fmt::Debug for Secret {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("Secret(<скрыт>)")
    }
}

/// Полное описание исходящего запроса.
#[derive(Debug, Clone)]
pub struct HttpRequest {
    destination: Destination,
    method: HttpMethod,
    path: String,
    query: Vec<(String, String)>,
    body: Option<RequestBody>,
    bearer: Option<Secret>,
    soap_action: Option<String>,
}

impl HttpRequest {
    #[must_use]
    pub fn get(destination: Destination, path: &str) -> Self {
        Self::new(destination, HttpMethod::Get, path, None)
    }

    #[must_use]
    pub fn post(destination: Destination, path: &str, body: RequestBody) -> Self {
        Self::new(destination, HttpMethod::Post, path, Some(body))
    }

    fn new(
        destination: Destination,
        method: HttpMethod,
        path: &str,
        body: Option<RequestBody>,
    ) -> Self {
        Self {
            destination,
            method,
            path: path.to_owned(),
            query: Vec::new(),
            body,
            bearer: None,
            soap_action: None,
        }
    }

    #[must_use]
    pub fn with_query(mut self, key: &str, value: &str) -> Self {
        self.query.push((key.to_owned(), value.to_owned()));
        self
    }

    #[must_use]
    pub fn with_bearer(mut self, token: &str) -> Self {
        self.bearer = Some(Secret::new(token));
        self
    }

    /// Заголовок `SOAPAction`. Нужен ЦБ РФ: без него сервис отвечает
    /// отказом, а не ошибкой разбора, и причина неочевидна.
    #[must_use]
    pub fn with_soap_action(mut self, action: &str) -> Self {
        self.soap_action = Some(action.to_owned());
        self
    }

    #[must_use]
    pub const fn destination(&self) -> Destination {
        self.destination
    }

    #[must_use]
    pub const fn method(&self) -> HttpMethod {
        self.method
    }

    #[must_use]
    pub const fn body(&self) -> Option<&RequestBody> {
        self.body.as_ref()
    }

    #[must_use]
    pub const fn bearer(&self) -> Option<&Secret> {
        self.bearer.as_ref()
    }

    #[must_use]
    pub fn soap_action(&self) -> Option<&str> {
        self.soap_action.as_deref()
    }

    /// Полный URL запроса.
    #[must_use]
    pub fn url(&self) -> String {
        let base = self.destination.base_url().trim_end_matches('/');
        let path = self.path.trim_start_matches('/');
        let mut url = format!("{base}/{path}");
        if !self.query.is_empty() {
            url.push('?');
            let encoded: Vec<String> = self
                .query
                .iter()
                .map(|(key, value)| {
                    format!(
                        "{}={}",
                        utf8_percent_encode(key, NON_ALPHANUMERIC),
                        utf8_percent_encode(value, NON_ALPHANUMERIC)
                    )
                })
                .collect();
            url.push_str(&encoded.join("&"));
        }
        url
    }
}
```

`crates/iaam-http/src/response.rs`:

```rust
//! Ответ и отказы транспорта.

use thiserror::Error;

/// Ответ узла: код и тело как есть.
///
/// Тело не разбирается и не перекодируется здесь: ЦБ отвечает
/// в `windows-1251`, MOEX — в UTF-8, и знание об этом принадлежит
/// крейте источника, а не транспорту.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpResponse {
    pub status: u16,
    pub body: Vec<u8>,
}

impl HttpResponse {
    /// Тело как строка UTF-8. Источники с иной кодировкой этим методом
    /// не пользуются — они берут `body` и перекодируют сами.
    #[must_use]
    pub fn text_utf8(&self) -> Option<&str> {
        core::str::from_utf8(&self.body).ok()
    }
}

/// Отказ транспорта.
///
/// Варианты не несут ни тела ответа, ни предъявленного секрета:
/// классификация ответа по смыслу принадлежит источнику, а отказ
/// транспорта не должен превращаться в утечку.
#[derive(Debug, Error)]
pub enum HttpError {
    #[error("сетевой отказ")]
    Network,
    #[error("истекло время ожидания ответа")]
    Timeout,
    #[error("клиент не собран: {0}")]
    ClientNotBuilt(String),
    #[error("вшитый корень доверия не разобран: {0}")]
    TrustAnchorNotParsed(String),
}
```

`crates/iaam-http/src/lib.rs`:

```rust
//! Исходящий HTTP: транспорт, доверие, устойчивость.
//!
//! Единственная крейта в дереве, объявляющая HTTP-клиент. Крейты
//! источников (`iaam-broker`, `iaam-market`) описывают запрос и разбирают
//! ответ; ни та, ни другая операция сети не касается, и потому обе
//! проверяются на замороженных образцах.
//!
//! Правило проверяется заслоном: `scripts/check-architecture.sh`
//! запрещает `reqwest` во всех крейтах, кроме этой.

pub mod client;
pub mod destination;
pub mod request;
pub mod resilience;
pub mod response;
pub mod trust;

pub use destination::Destination;
pub use request::{HttpMethod, HttpRequest, RequestBody, Secret};
pub use response::{HttpError, HttpResponse};
```

**`lib.rs` объявляет все шесть модулей сразу и больше не правится ни одной
задачей.** Это сделано намеренно: задачи 2, 3 и 4 идут параллельно, и общий
файл превратился бы в гонку правок. Поэтому задача 1 создаёт заглушки
`trust.rs`, `client.rs` и `resilience.rs` — каждая содержит только
doc-комментарий, чтобы крейта собиралась:

```rust
//! Заглушка. Наполняется задачей 2 (доверие).
```

```rust
//! Заглушка. Наполняется задачей 3 (отправка запроса).
```

```rust
//! Заглушка. Наполняется задачей 4 (устойчивость).
```

Реэкспортов из этих трёх модулей в `lib.rs` **нет**: потребители пишут полный
путь (`iaam_http::trust::Anchors`, `iaam_http::client::HttpClient`,
`iaam_http::resilience::RetryPolicy`). Чуть длиннее в вызове — зато ни одна
параллельная задача не возвращается в общий файл.

- [ ] **Step 6: Прогнать тесты**

Run: `nix develop -c cargo test -p iaam-http --lib`
Expected: PASS, шесть тестов в `request::tests`.

- [ ] **Step 7: Формат и коммит**

```bash
nix develop -c cargo fmt --all
git add crates/iaam-http Cargo.toml
git commit -m "feat(http): крейта исходящего HTTP — описание запроса и ответ"
```

---

### Task 2: Таблица назначений и якорей доверия

**Files:**
- Create: `crates/iaam-http/src/trust.rs`
- Move: `crates/iaam-broker/certs/russian-trusted-root-ca.pem` → `crates/iaam-http/certs/russian-trusted-root-ca.pem`
- Move: `crates/iaam-broker/certs/README.md` → `crates/iaam-http/certs/README.md`
- Move: `crates/iaam-broker/tests/trust.rs` → `crates/iaam-http/tests/trust.rs`

**Interfaces:**
- Consumes: `Destination` (задача 1), `HttpError::TrustAnchorNotParsed`, `HttpError::ClientNotBuilt` (задача 1).
- Produces: `Anchors::{WebRoots, Pinned(&'static str)}`; `Destination::anchors(self) -> Anchors` — дополнительный `impl` в `trust.rs`, это законно, крейта та же; `pub const RUSSIAN_TRUSTED_ROOT_CA_PEM: &str`; `pub fn certificate_count() -> usize`; `pub(crate) fn client_for(Destination) -> Result<Client, HttpError>`.

**Acceptance Criteria:**
- У каждого назначения якорь объявлен явно; добавление варианта без якоря не собирается.
- Вшитый корень применяется **только** к шлюзу Т-Инвестиций; для публичных узлов якорь — веб-корни.
- Вшитая связка содержит ровно один сертификат.
- Отпечаток вшитого файла совпадает с зафиксированным: подмена корня в дереве роняет тест.

- [ ] **Step 1: Перенести сертификат и его описание**

```bash
mkdir -p crates/iaam-http/certs crates/iaam-http/tests
git mv crates/iaam-broker/certs/russian-trusted-root-ca.pem crates/iaam-http/certs/
git mv crates/iaam-broker/certs/README.md crates/iaam-http/certs/
git mv crates/iaam-broker/tests/trust.rs crates/iaam-http/tests/trust.rs
```

Тест отпечатка **переносится, а не переписывается**: он уже существует и уже содержит правильное значение. Переписать его заново значит рискнуть тем, что новое значение возьмут из нового файла — и тест перестанет ловить подмену, ради которой заведён.

В `crates/iaam-http/certs/README.md` дописать абзац о переезде: файл обслуживает не одного потребителя, а таблицу назначений, и применяется только к тем из них, у кого якорь объявлен закреплённым.

- [ ] **Step 2: Поправить перенесённый тест и дополнить его**

В `crates/iaam-http/tests/trust.rs` заменить импорт на `iaam_http::trust::...` и `iaam_http::Destination`. Константа отпечатка остаётся прежней:

```rust
const FROZEN_ROOT_SHA256: &str = "936a43fea6e8e525bcc0f81acd9c3d21b4fc4b9b68acea7906d698005afc6504";
```

Дописать в тот же файл проверку самой таблицы:

```rust
#[test]
fn a_pinned_anchor_is_used_only_for_the_gateways_that_need_it() {
    for pinned in [Destination::TinkoffProd, Destination::TinkoffSandbox] {
        assert!(
            matches!(pinned.anchors(), Anchors::Pinned(_)),
            "{pinned:?} обязан ходить на вшитом корне: Минцифры нет в общедоступных хранилищах"
        );
    }
    for public in [
        Destination::FinamApi,
        Destination::MoexIss,
        Destination::CbrScripts,
        Destination::CbrDailyInfo,
    ] {
        assert!(
            matches!(public.anchors(), Anchors::WebRoots),
            "{public:?} не должен ходить на вшитом корне: он подписан публичным центром"
        );
    }
}

#[test]
fn the_pinned_bundle_holds_exactly_one_certificate() {
    assert_eq!(certificate_count(), 1);
}
```

Соответственно дополнить импорт: `use iaam_http::trust::{Anchors, RUSSIAN_TRUSTED_ROOT_CA_PEM, certificate_count};` и `use iaam_http::Destination;`.

- [ ] **Step 3: Убедиться, что тест не собирается**

Run: `nix develop -c cargo test -p iaam-http --test trust`
Expected: FAIL, `unresolved import iaam_http::trust::Destination`.

- [ ] **Step 4: Реализовать таблицу**

`crates/iaam-http/src/trust.rs`:

```rust
//! Якорь доверия задаётся здесь и только здесь (§14).
//!
//! Политика доверия объявлена **одной таблицей назначений**, а не
//! рассыпана по крейтам источников. «Глобально» здесь означает единство
//! управления, а не слияние якорей: вшитый корень применяется ровно
//! к тому узлу, ради которого он вшит.
//!
//! Причина, по которой у Т-Инвестиций якорь свой: корень Минцифры
//! отсутствует в общедоступных хранилищах, и пиннинг был единственным
//! способом соединиться. У MOEX (ZeroSSL) и ЦБ (HARICA) сертификаты
//! публичных центров — вшивать там нечего, а пиннинг публичного
//! DV-центра ломался бы при смене выпускающего и не покупал бы ничего.
//!
//! Проверка подлинности не отключается ни для одного назначения.
//! Меняется только то, откуда берётся якорь.

use reqwest::{Certificate, Client};

use crate::destination::Destination;
use crate::response::HttpError;

/// Корневой сертификат Минцифры.
///
/// `include_str!`, а не чтение файла при запуске: файл на диске рядом
/// с программой подменить проще, чем содержимое двоичного файла,
/// а якорь доверия — ровно то, что подменяют в первую очередь.
pub const RUSSIAN_TRUSTED_ROOT_CA_PEM: &str = include_str!("../certs/russian-trusted-root-ca.pem");

/// Сколько сертификатов лежит в вшитой связке.
///
/// Ровно один: промежуточный сертификат сервер присылает сам, а лишний
/// якорь — это лишнее доверие и вторая дата истечения.
#[must_use]
pub fn certificate_count() -> usize {
    RUSSIAN_TRUSTED_ROOT_CA_PEM
        .matches("BEGIN CERTIFICATE")
        .count()
}

/// Откуда берётся якорь для назначения.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Anchors {
    /// Общедоступные корни. Узел подписан публичным центром.
    WebRoots,
    /// Ровно один вшитый корень; веб-корни выключены.
    Pinned(&'static str),
}

/// Якорь доверия назначения.
///
/// `impl` живёт здесь, а не рядом с объявлением `Destination`: база узла
/// нужна для сборки URL и не имеет отношения к доверию, а якорь нужен
/// только при сборке клиента. Разные вопросы — разные модули; крейта
/// та же, так что дополнительный `impl` законен.
impl Destination {
    #[must_use]
    pub const fn anchors(self) -> Anchors {
        match self {
            // Обе среды шлюза — один удостоверяющий центр.
            Self::TinkoffProd | Self::TinkoffSandbox => Anchors::Pinned(RUSSIAN_TRUSTED_ROOT_CA_PEM),
            Self::FinamApi | Self::MoexIss | Self::CbrScripts | Self::CbrDailyInfo => {
                Anchors::WebRoots
            }
        }
    }
}

/// Собирает клиента под якорь назначения.
pub(crate) fn client_for(destination: Destination) -> Result<Client, HttpError> {
    let builder = Client::builder().tls_backend_rustls();
    let builder = match destination.anchors() {
        Anchors::WebRoots => builder,
        Anchors::Pinned(pem) => {
            let root = Certificate::from_pem(pem.as_bytes())
                .map_err(|error| HttpError::TrustAnchorNotParsed(error.to_string()))?;
            // Именно `only`, а не `merge`: `merge` добавил бы наш корень
            // к веб-корням, и клиент продолжил бы доверять всему
            // публичному интернету ради узла, которому это не нужно.
            builder.tls_certs_only([root])
        }
    };
    builder
        .build()
        .map_err(|error| HttpError::ClientNotBuilt(error.to_string()))
}
```

- [ ] **Step 5: Прогнать тесты**

Run: `nix develop -c cargo test -p iaam-http --test trust`
Expected: PASS, включая перенесённый `якорь доверия подменён` и два новых.

`lib.rs` не трогать: модуль `trust` уже объявлен задачей 1, заглушка
заменяется содержимым. Реэкспорта нет намеренно — общий файл в параллельной
волне это гонка правок.

- [ ] **Step 6: Формат и коммит**

```bash
nix develop -c cargo fmt --all
git add -A crates/iaam-http crates/iaam-broker
git commit -m "feat(http): таблица якорей доверия, вшитый корень переехал из брокера"
```

---

### Task 3: Отправка запроса

**Files:**
- Create: `crates/iaam-http/src/client.rs`
- `crates/iaam-http/src/lib.rs` — **не трогать**, модуль объявлен задачей 1
- Test: внутри `client.rs`

**Interfaces:**
- Consumes: `HttpRequest`, `HttpResponse`, `HttpError`, `trust::client_for` (задачи 1, 2).
- Produces: `HttpClient::new() -> Self`; `HttpClient::send(&self, request: &HttpRequest) -> Result<HttpResponse, HttpError>` (async).

**Acceptance Criteria:**
- Клиент на назначение собирается один раз и переиспользуется: повторный запрос к тому же узлу не строит второй клиент.
- Таймаут задан явно; его отсутствие означало бы вечно висящий фоновый прогон.
- Отказ по времени отличим от прочих сетевых отказов.
- Заголовки `Authorization`, `Content-Type` и `SOAPAction` выставляются из описания запроса, а не вызывающей стороной.

- [ ] **Step 1: Написать падающий тест**

`crates/iaam-http/src/client.rs`, в конец файла:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::request::RequestBody;

    #[test]
    fn a_client_is_built_once_per_destination() {
        let client = HttpClient::new();
        let first = client.pool_len();
        let _ = client.client_for(Destination::MoexIss).expect("клиент");
        let _ = client.client_for(Destination::MoexIss).expect("клиент");
        assert_eq!(first, 0);
        assert_eq!(client.pool_len(), 1, "второй запрос собрал второй клиент");
    }

    #[test]
    fn distinct_destinations_get_distinct_clients() {
        let client = HttpClient::new();
        let _ = client.client_for(Destination::MoexIss).expect("клиент");
        let _ = client.client_for(Destination::TinkoffProd).expect("клиент");
        assert_eq!(client.pool_len(), 2);
    }

    #[test]
    fn the_two_gateway_environments_do_not_share_a_client() {
        let client = HttpClient::new();
        let _ = client.client_for(Destination::TinkoffProd).expect("клиент");
        let _ = client
            .client_for(Destination::TinkoffSandbox)
            .expect("клиент");
        assert_eq!(
            client.pool_len(),
            2,
            "песочница и бой — разные хосты, общий клиент увёл бы запрос не туда"
        );
    }

    #[test]
    fn a_soap_request_carries_its_action_header() {
        let request = HttpRequest::post(
            Destination::CbrDailyInfo,
            "/DailyInfoWebServ/DailyInfo.asmx",
            RequestBody::Xml("<soap:Envelope/>".to_owned()),
        )
        .with_soap_action("http://web.cbr.ru/KeyRateXML");
        assert_eq!(request.soap_action(), Some("http://web.cbr.ru/KeyRateXML"));
        assert_eq!(
            request.body().map(RequestBody::content_type),
            Some("text/xml; charset=utf-8")
        );
    }
}
```

- [ ] **Step 2: Убедиться, что тест не собирается**

Run: `nix develop -c cargo test -p iaam-http --lib client`
Expected: FAIL, `cannot find type HttpClient in this scope`.

- [ ] **Step 3: Реализовать клиента**

`crates/iaam-http/src/client.rs`:

```rust
//! Отправка запроса.
//!
//! Клиент на назначение собирается один раз: сборка клиента `reqwest`
//! поднимает пул соединений и разбирает якорь доверия, и делать это
//! на каждый запрос значит терять и то, и другое.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use reqwest::Client;

use crate::request::{HttpMethod, HttpRequest, RequestBody};
use crate::response::{HttpError, HttpResponse};
use crate::trust::{Destination, client_for};

/// Предел ожидания ответа.
///
/// Задан явно: у `reqwest` таймаута по умолчанию нет, и его отсутствие
/// превратило бы зависший узел в вечно висящее фоновое задание.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Клиент исходящих запросов.
pub struct HttpClient {
    pool: Mutex<HashMap<Destination, Client>>,
}

impl Default for HttpClient {
    fn default() -> Self {
        Self::new()
    }
}

impl HttpClient {
    #[must_use]
    pub fn new() -> Self {
        Self {
            pool: Mutex::new(HashMap::new()),
        }
    }

    pub(crate) fn client_for(&self, destination: Destination) -> Result<Client, HttpError> {
        let mut pool = self
            .pool
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(existing) = pool.get(&destination) {
            return Ok(existing.clone());
        }
        let built = client_for(destination)?;
        pool.insert(destination, built.clone());
        Ok(built)
    }

    #[cfg(test)]
    pub(crate) fn pool_len(&self) -> usize {
        self.pool
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len()
    }

    /// Отправляет запрос и возвращает код с телом.
    ///
    /// Код ответа **не классифицируется** здесь: смысл 401 у шлюза
    /// брокера и у биржи разный, и трактовка принадлежит источнику.
    pub async fn send(&self, request: &HttpRequest) -> Result<HttpResponse, HttpError> {
        let client = self.client_for(request.destination())?;
        let mut builder = match request.method() {
            HttpMethod::Get => client.get(request.url()),
            HttpMethod::Post => client.post(request.url()),
        };
        builder = builder.timeout(REQUEST_TIMEOUT);
        if let Some(secret) = request.bearer() {
            builder = builder.bearer_auth(secret.expose());
        }
        if let Some(action) = request.soap_action() {
            builder = builder.header("SOAPAction", format!("\"{action}\""));
        }
        if let Some(body) = request.body() {
            builder = builder
                .header("Content-Type", body.content_type())
                .body(body.payload().to_owned());
        }
        let response = builder.send().await.map_err(classify_transport_error)?;
        let status = response.status().as_u16();
        let body = response
            .bytes()
            .await
            .map_err(classify_transport_error)?
            .to_vec();
        Ok(HttpResponse { status, body })
    }
}

fn classify_transport_error(error: reqwest::Error) -> HttpError {
    if error.is_timeout() {
        HttpError::Timeout
    } else {
        HttpError::Network
    }
}
```

`lib.rs` не трогать: модуль `client` уже объявлен задачей 1.

- [ ] **Step 4: Прогнать тесты**

Run: `nix develop -c cargo test -p iaam-http --lib client`
Expected: PASS, три теста.

- [ ] **Step 5: Формат и коммит**

```bash
nix develop -c cargo fmt --all
git add crates/iaam-http
git commit -m "feat(http): отправка запроса с таймаутом и клиентом на назначение"
```

---

### Task 4: Устойчивость — повтор, задержка, ограничение частоты

**Files:**
- Create: `crates/iaam-http/src/resilience.rs`
- `crates/iaam-http/src/lib.rs` — **не трогать**, модуль объявлен задачей 1
- Test: внутри `resilience.rs`

**Interfaces:**
- Consumes: `HttpError`, `HttpResponse` (задача 1).
- Produces: `RetryPolicy::{ new(attempts: u32, base: Duration), decide(&self, attempt: u32, outcome: &Outcome) -> Retry }`; `Outcome::{Transport(HttpError), Status(u16)}`; `Retry::{Give_up, After(Duration)}` (в коде — `Retry::GiveUp` и `Retry::After`); `RateLimiter::new(min_interval: Duration)`.

**Acceptance Criteria:**
- Решение о повторе — **чистая функция** от номера попытки и исхода: проверяется без сети и без сна.
- Повторяются отказ сети, таймаут и коды 429, 502, 503, 504. Не повторяются 4xx, кроме 429: повтор отказа в правах — это трата попыток на заведомо тот же ответ.
- Задержка растёт экспоненциально и **ограничена сверху**: без потолка шестая попытка ушла бы за пределы окна синхронизации.
- Исчерпание попыток даёт `GiveUp`, а не бесконечный цикл.

- [ ] **Step 1: Написать падающий тест**

`crates/iaam-http/src/resilience.rs`, в конец файла:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn policy() -> RetryPolicy {
        RetryPolicy::new(4, Duration::from_millis(100))
    }

    #[test]
    fn a_network_failure_is_retried() {
        assert!(matches!(
            policy().decide(1, &Outcome::Transport(HttpError::Network)),
            Retry::After(_)
        ));
    }

    #[test]
    fn a_timeout_is_retried() {
        assert!(matches!(
            policy().decide(1, &Outcome::Transport(HttpError::Timeout)),
            Retry::After(_)
        ));
    }

    #[test]
    fn rate_limiting_and_gateway_failures_are_retried() {
        for status in [429, 502, 503, 504] {
            assert!(
                matches!(policy().decide(1, &Outcome::Status(status)), Retry::After(_)),
                "код {status} обязан повторяться"
            );
        }
    }

    #[test]
    fn a_rejection_is_not_retried() {
        for status in [400, 401, 403, 404, 422] {
            assert!(
                matches!(policy().decide(1, &Outcome::Status(status)), Retry::GiveUp),
                "код {status} повторять бессмысленно: ответ будет тот же"
            );
        }
    }

    #[test]
    fn a_success_is_not_retried() {
        assert!(matches!(
            policy().decide(1, &Outcome::Status(200)),
            Retry::GiveUp
        ));
    }

    #[test]
    fn the_delay_grows_and_stays_bounded() {
        let policy = policy();
        let first = match policy.decide(1, &Outcome::Status(503)) {
            Retry::After(delay) => delay,
            Retry::GiveUp => panic!("должен был повториться"),
        };
        let third = match policy.decide(3, &Outcome::Status(503)) {
            Retry::After(delay) => delay,
            Retry::GiveUp => panic!("должен был повториться"),
        };
        assert!(third > first, "задержка обязана расти: {first:?} → {third:?}");
        assert!(third <= MAX_BACKOFF, "задержка вышла за потолок: {third:?}");
    }

    #[test]
    fn attempts_are_exhausted_rather_than_looping_forever() {
        assert!(matches!(
            policy().decide(4, &Outcome::Status(503)),
            Retry::GiveUp
        ));
    }
}
```

- [ ] **Step 2: Убедиться, что тест не собирается**

Run: `nix develop -c cargo test -p iaam-http --lib resilience`
Expected: FAIL, `cannot find type RetryPolicy in this scope`.

- [ ] **Step 3: Реализовать политику**

`crates/iaam-http/src/resilience.rs`:

```rust
//! Устойчивость: когда повторять, через сколько и как часто ходить (§12).
//!
//! Решение о повторе — **чистая функция**. Так оно проверяется без сети
//! и без сна: тест на политику повторов, который спит, проверяет ещё
//! и планировщик потоков, а падает загадочно.

use std::time::{Duration, Instant};

use crate::response::HttpError;

/// Потолок задержки.
///
/// Без него экспонента на шестой попытке ушла бы за пределы окна
/// суточной синхронизации, и задание висело бы вместо того, чтобы
/// честно отчитаться о частичном отказе.
pub const MAX_BACKOFF: Duration = Duration::from_secs(60);

/// Чем закончилась попытка.
#[derive(Debug)]
pub enum Outcome {
    /// Транспорт не довёл запрос.
    Transport(HttpError),
    /// Узел ответил кодом.
    Status(u16),
}

/// Что делать после попытки.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Retry {
    /// Повторить через указанную задержку.
    After(Duration),
    /// Не повторять: попытки исчерпаны либо повтор бессмыслен.
    GiveUp,
}

/// Политика повторов.
#[derive(Debug, Clone, Copy)]
pub struct RetryPolicy {
    attempts: u32,
    base: Duration,
}

impl RetryPolicy {
    #[must_use]
    pub const fn new(attempts: u32, base: Duration) -> Self {
        Self { attempts, base }
    }

    /// Решение по номеру попытки (с единицы) и её исходу.
    #[must_use]
    pub fn decide(&self, attempt: u32, outcome: &Outcome) -> Retry {
        if attempt >= self.attempts || !is_transient(outcome) {
            return Retry::GiveUp;
        }
        Retry::After(self.backoff(attempt))
    }

    fn backoff(&self, attempt: u32) -> Duration {
        let factor = 1_u32 << (attempt.min(16) - 1);
        self.base.saturating_mul(factor).min(MAX_BACKOFF)
    }
}

/// Отказ временный, то есть повтор имеет шанс дать другой ответ.
///
/// 4xx, кроме 429, сюда не входят намеренно: отказ в правах или
/// неверный запрос повторятся ровно тем же, и попытки будут потрачены
/// на заведомо известный ответ.
fn is_transient(outcome: &Outcome) -> bool {
    match outcome {
        Outcome::Transport(HttpError::Network | HttpError::Timeout) => true,
        Outcome::Transport(HttpError::ClientNotBuilt(_) | HttpError::TrustAnchorNotParsed(_)) => {
            false
        }
        Outcome::Status(status) => matches!(status, 429 | 502 | 503 | 504),
    }
}

/// Ограничение частоты: не чаще одного запроса в заданный интервал.
///
/// Существует, чтобы первичная загрузка истории не выглядела для MOEX
/// как поток запросов: получить 429 и уйти в повторы дороже, чем
/// подождать.
pub struct RateLimiter {
    min_interval: Duration,
    last: std::sync::Mutex<Option<Instant>>,
}

impl RateLimiter {
    #[must_use]
    pub fn new(min_interval: Duration) -> Self {
        Self {
            min_interval,
            last: std::sync::Mutex::new(None),
        }
    }

    /// Сколько ждать до следующего запроса. Ноль — можно сразу.
    #[must_use]
    pub fn delay_before_next(&self, now: Instant) -> Duration {
        let mut last = self
            .last
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let wait = match *last {
            Some(previous) => self
                .min_interval
                .checked_sub(now.saturating_duration_since(previous))
                .unwrap_or_default(),
            None => Duration::ZERO,
        };
        *last = Some(now + wait);
        wait
    }
}
```

`lib.rs` не трогать: модуль `resilience` уже объявлен задачей 1.

- [ ] **Step 4: Прогнать тесты**

Run: `nix develop -c cargo test -p iaam-http --lib resilience`
Expected: PASS, семь тестов.

- [ ] **Step 5: Формат и коммит**

```bash
nix develop -c cargo fmt --all
git add crates/iaam-http
git commit -m "feat(http): политика повторов, задержка с потолком и ограничение частоты"
```

---

### Task 5: Переселение клиента Т-Инвестиций

**Files:**
- Modify: `crates/iaam-broker/src/tinkoff/client.rs` (поля структуры, метод `post`, вариант ошибки)
- Modify: `crates/iaam-broker/Cargo.toml` (добавить `iaam-http`)

**Interfaces:**
- Consumes: `HttpClient::send`, `HttpRequest::post`, `RequestBody::Json`, `Destination::{TinkoffProd, TinkoffSandbox}`, `HttpError` (задачи 1–3).
- Produces: `TinkoffClient` с прежним публичным API. Сигнатуры `get_portfolio`, `get_operations_by_cursor` и прочих методов **не меняются**.

**Acceptance Criteria:**
- `reqwest` в `tinkoff/client.rs` не упоминается.
- Все существующие тесты `iaam-broker` проходят **без правки их утверждений**.
- Вариант `TinkoffError::Trust` заменён на вариант, несущий `HttpError`, и по-прежнему не содержит токена.
- `Environment` продолжает решать, песочница это или бой, но **выбирает назначение**, а не склеивает URL: базы у сред разные по хосту.
- `Environment::base_url()` и `Destination::base_url()` не расходятся: значения сверены и совпадают дословно.

- [ ] **Step 1: Добавить зависимость**

В `crates/iaam-broker/Cargo.toml`, в `[dependencies]`:

```toml
# Исходящий HTTP, доверие и устойчивость живут в одной крейте:
# описание запроса и разбор ответа транспорта не знают (§3.1).
iaam-http = { path = "../iaam-http", version = "0.1.0" }
```

`reqwest` пока **не удалять** — его снимает задача 6, когда переселится и второй клиент.

- [ ] **Step 2: Заменить вариант ошибки**

В `crates/iaam-broker/src/tinkoff/client.rs` заменить

```rust
    /// Не удалось собрать HTTP-клиент с закреплённым корнем доверия.
    #[error(transparent)]
    Trust(#[from] TrustError),
```

на

```rust
    /// Транспорт не довёл запрос: сеть, время ожидания или сборка клиента.
    ///
    /// Токена не несёт: `HttpError` его не содержит по построению.
    #[error(transparent)]
    Transport(#[from] iaam_http::HttpError),
```

и импорт `use crate::trust::{TrustError, tinkoff_client};` — на `use iaam_http::{Destination, HttpClient, HttpRequest, RequestBody};`.

- [ ] **Step 3: Переписать транспортный метод**

Поле структуры `http: reqwest::Client` заменить на `http: HttpClient`, а метод `post` — на:

```rust
    async fn post(&self, method: Method, path: &str, body: Value) -> Result<String, TinkoffError> {
        ensure_method_available(self.environment, method)?;
        // База среды берётся у `Environment`, а не у `Destination`:
        // песочница и бой — это разные адреса одного назначения,
        // и якорь доверия у них общий.
        let request = HttpRequest::post(
            destination_for(self.environment),
            path,
            RequestBody::Json(
                serde_json::to_string(&body).map_err(|_| TinkoffError::RequestSerialization)?,
            ),
        )
        .with_bearer(self.token.expose());
        let response = self.http.send(&request).await?;
        let body = String::from_utf8(response.body).map_err(|_| TinkoffError::MalformedResponse)?;
        classify_response_with_token(response.status, &body, self.token.expose())?;
        Ok(body)
    }
```

`method_url` удаляется целиком: склейка базы с путём переехала в `HttpRequest::url()`. Вместо неё — отображение среды в назначение:

```rust
/// Среда выбирает назначение, а не приписку к URL.
///
/// У песочницы и боя **разные хосты** (`sandbox-invest-public-api.tbank.ru`
/// против `invest-public-api.tbank.ru`), поэтому подставить одну вместо
/// другой обрезкой базы нельзя — запрос ушёл бы не туда и получил бы
/// правдоподобный ответ из другой среды.
const fn destination_for(environment: Environment) -> Destination {
    match environment {
        Environment::Prod => Destination::TinkoffProd,
        Environment::Sandbox => Destination::TinkoffSandbox,
    }
}
```

Существующий тест `crates/iaam-broker/src/environment.rs:116` проверяет, что базы сред различаются и что в песочнице есть `sandbox`. Он остаётся и продолжает охранять `Environment`; парная проверка на стороне `Destination` добавлена задачей 1.

**Сверка баз обязательна.** Значения в `Destination::base_url()` и в `Environment::base_url()` продублированы намеренно (разные крейты, разные обязанности), и расхождение между ними было бы тихой ошибкой. Проверить дословно:

```bash
grep -n 'tbank.ru' crates/iaam-broker/src/environment.rs crates/iaam-http/src/destination.rs
```

Все четыре строки обязаны совпасть попарно. Если нет — остановка и эскалация.

- [ ] **Step 4: Прогнать тесты брокера**

Run: `nix develop -c cargo test -p iaam-broker --lib tinkoff`
Expected: PASS, все существующие тесты `tinkoff::client::tests` — включая `error_text_never_contains_the_token` и `preserves_unexpected_status_body_without_treating_success_as_error`.

Если хоть одно утверждение пришлось изменить — остановка и эскалация.

- [ ] **Step 5: Формат и коммит**

```bash
nix develop -c cargo fmt --all
git add crates/iaam-broker
git commit -m "refactor(broker): клиент Т-Инвестиций ходит через iaam-http"
```

---

### Task 6: Переселение клиента Finam и снятие `reqwest`

**Files:**
- Modify: `crates/iaam-broker/src/finam/client.rs`
- Delete: `crates/iaam-broker/src/trust.rs`
- Modify: `crates/iaam-broker/src/lib.rs` (снять `pub mod trust;`)
- Modify: `crates/iaam-broker/Cargo.toml` (удалить `reqwest`)

**Interfaces:**
- Consumes: те же, что задача 5, плюс `HttpRequest::get`, `Destination::FinamApi`.
- Produces: `FinamClient` с прежним публичным API.

**Acceptance Criteria:**
- `reqwest` отсутствует в `crates/iaam-broker/Cargo.toml` и во всём `crates/iaam-broker/src`.
- `crates/iaam-broker/src/trust.rs` удалён; тест отпечатка корня живёт теперь в `iaam-http` (задача 2) и не потерян.
- Все существующие тесты `iaam-broker` проходят без правки утверждений.
- `BASE_URL` из `finam/client.rs` удалён: база принадлежит таблице назначений.

- [ ] **Step 1: Переписать транспортный метод Finam**

Поле `http: reqwest::Client` → `http: HttpClient`; конструктор `reqwest::Client::new()` → `HttpClient::new()`; метод `get` — на:

```rust
    async fn get(&self, path: &str, query: &[(&str, String)]) -> Result<String, FinamError> {
        let mut request = HttpRequest::get(Destination::FinamApi, path)
            .with_bearer(self.token.expose());
        for (key, value) in query {
            request = request.with_query(key, value);
        }
        let response = self.http.send(&request).await.map_err(|_| FinamError::Network)?;
        let body = String::from_utf8(response.body).map_err(|_| FinamError::MalformedResponse)?;
        classify_response(response.status, &body)?;
        Ok(body)
    }
```

Константу `const BASE_URL: &str = "https://api.finam.ru";` удалить — она переехала в `Destination::FinamApi::base_url()`. Проверить, что значения совпадают дословно.

- [ ] **Step 2: Убрать модуль доверия из брокера**

```bash
git rm crates/iaam-broker/src/trust.rs
```

Из `crates/iaam-broker/src/lib.rs` снять строку `pub mod trust;`. Если на `trust::` ссылается что-то ещё — найти и переключить на `iaam_http`:

```bash
grep -rn "trust::" crates/ --include='*.rs'
```

- [ ] **Step 3: Снять `reqwest` с манифеста**

Из `crates/iaam-broker/Cargo.toml` удалить строку с `reqwest` вместе с её комментарием. Из `[dev-dependencies]` удалить `sha2`, если он оставался только ради теста отпечатка корня.

- [ ] **Step 4: Проверить, что транспорта в брокере не осталось**

Run:
```bash
grep -rn "reqwest" crates/iaam-broker/ && echo "ОСТАЛОСЬ" || echo "чисто"
```
Expected: `чисто`.

- [ ] **Step 5: Прогнать тесты брокера**

Run: `nix develop -c cargo test -p iaam-broker`
Expected: PASS, все тесты крейты без правки утверждений.

- [ ] **Step 6: Формат и коммит**

```bash
nix develop -c cargo fmt --all
git add crates/iaam-broker
git commit -m "refactor(broker): клиент Finam на iaam-http, reqwest снят с манифеста"
```

---

### Task 7: Заслон архитектуры и правка графа §3.2

**Files:**
- Modify: `scripts/check-architecture.sh` (правило 11, в конец, перед итоговой проверкой `fail`)
- Modify: `.internal/specs/2026-08-22-investment-tracker-design.md` (граф §3.2 и таблица под ним)

**Interfaces:**
- Consumes: результат задач 5 и 6 — брокер уже без `reqwest`. Заслон, добавленный раньше, покраснеет на непереселённом коде.
- Produces: правило, ловящее возврат транспорта в крейту источника.

**Acceptance Criteria:**
- Заслон падает, если `reqwest` объявлен в манифесте любой крейты, кроме `iaam-http`.
- Заслон проверяет **манифесты**, а не только исходники: зависимость, объявленная и неиспользованная, — это разрешение использовать её завтра.
- `nix develop -c ./scripts/check-architecture.sh` проходит на текущем дереве.
- Граф §3.2 содержит `iaam-http` и `iaam-broker`; таблица под графом описывает обе.

- [ ] **Step 1: Написать правило**

В `scripts/check-architecture.sh`, после правила 10 и **до** финальной проверки `if [ "$fail" -ne 0 ]`:

```bash
# --- 11. Транспорт живёт в одной крейте ---
# §3.1 и раздел 2.1 дизайна E3.2: крейты источников описывают запрос
# и разбирают ответ, но HTTP не знают. Проверяются МАНИФЕСТЫ, а не
# исходники: объявленная и пока неиспользованная зависимость —
# это разрешение воспользоваться ею завтра, без единой правки заслона.
for manifest in crates/*/Cargo.toml; do
  [ -f "$manifest" ] || continue
  case "$manifest" in
    crates/iaam-http/Cargo.toml) continue ;;
  esac
  hits=$(grep -n '^[[:space:]]*reqwest[[:space:]]*=' "$manifest" || true)
  if [ -n "$hits" ]; then
    err "$manifest объявляет reqwest: исходящий HTTP живёт только в iaam-http (§3.1)"
    echo "$hits" >&2
  fi
done
```

- [ ] **Step 2: Прогнать заслон**

Run: `nix develop -c ./scripts/check-architecture.sh`
Expected: `Архитектурные заслоны пройдены.`

- [ ] **Step 3: Проверить, что заслон действительно ловит**

Временно вернуть `reqwest` в манифест брокера и убедиться, что заслон краснеет:

```bash
printf 'reqwest = "0.13"\n' >> crates/iaam-broker/Cargo.toml
nix develop -c ./scripts/check-architecture.sh || echo "заслон сработал — это ожидаемо"
git checkout crates/iaam-broker/Cargo.toml
```

Expected: заслон падает с сообщением про `iaam-http`, затем правка откатывается.

Заслон, который не проверили на срабатывание, — это заслон, про который неизвестно, работает ли он.

- [ ] **Step 4: Поправить граф §3.2**

В `.internal/specs/2026-08-22-investment-tracker-design.md` заменить блок графа на:

```
                            iaam-core
                         ↑      ↑      ↑
                  iaam-store  iaam-market  iaam-ingest
                         ↖      ↑      ↗
                            iaam-app
                            ↑        ↑
                      iaam-server  iaam-cli

  iaam-http ← iaam-market,  iaam-http ← iaam-broker
```

и дополнить таблицу под ним двумя строками:

| Крейта | Зависит от | Ответственность |
|---|---|---|
| `iaam-http` | — | исходящий HTTP, якоря доверия, повторы и задержки, ограничение частоты |
| `iaam-broker` | core + `iaam-http` | описание запросов брокеров, разбор отчётов и выгрузок |

В строке `iaam-market` дописать зависимость от `iaam-http`.

Отдельно отметить в тексте: `iaam-broker` в графе отсутствовал — он появился в E2 и в документ не попал; это исправление старой дыры, а не следствие E3.2.

- [ ] **Step 5: Коммит**

```bash
nix develop -c cargo fmt --all
git add scripts/check-architecture.sh .internal/specs/2026-08-22-investment-tracker-design.md
git commit -m "chore(arch): заслон запрещает reqwest вне iaam-http, граф §3.2 дополнен"
```

---

## Приёмка части 1 (оркестратор, один раз в конце)

Воркеры этих команд не запускают.

```bash
nix develop -c make check
nix develop -c ./scripts/check-architecture.sh
nix develop -c cargo mutants -p iaam-http --no-times
grep -rn "reqwest" crates/ --include='Cargo.toml' | grep -v iaam-http && echo "ПРОВАЛ" || echo "транспорт заперт"
```

Приёмка пройдена, когда:

- `make check` зелёный, число тестов не уменьшилось;
- ни один тест `iaam-broker` не был ослаблен ради прохождения;
- `reqwest` объявлен ровно в одном манифесте;
- мутационный заслон на `iaam-http` без выживших либо выжившие разобраны поимённо;
- граф §3.2 описывает дерево, которое действительно собралось.
