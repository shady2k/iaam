//! Контрактные тесты против порождённой спеки (§17.1).
//!
//! `utoipa` порождает спеку из типов и потому устраняет расхождение
//! **схемы данных**. Поведение — коды ответов, требования аутентификации,
//! фактическая сериализация — остаётся вне генерации, и проверяется
//! только вызовом поднятого сервера. Для контракта, которым пользуется
//! внешний агент, синтаксически верная, но поведенчески неверная спека
//! означает, что агент будет чиниться по неверной подсказке.

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use iaam_app::AppServices;
use iaam_app::adapters::sqlite::SqliteAdapter;
use iaam_app::ports::Clock;
use iaam_core::ids::{AccountId, CustodyId, InstrumentId, OwnerId};
use iaam_server::auth::hash_token;
use iaam_server::rate_limit::RateLimiter;
use iaam_server::{ServerState, build};
use iaam_store::SqliteStore;
use iaam_store::reference::AccountRecord;
use iaam_store::tokens::{TokenRecord, TokenScope};
use serde_json::{Value, json};
use std::time::Duration;
use time::Date;
use time::macros::date;
use tower::ServiceExt;
use uuid::Uuid;

/// Часы с зафиксированной датой: отчёт «на сегодня» иначе
/// невоспроизводим в тесте.
struct FixedClock(Date);

impl Clock for FixedClock {
    fn today(&self) -> Date {
        self.0
    }
}

struct Harness {
    router: Router,
    api: utoipa::openapi::OpenApi,
    owner_token: String,
    agent_token: String,
    readonly_token: String,
    account: AccountId,
    instrument: InstrumentId,
    custody: CustodyId,
}

fn harness() -> Harness {
    harness_with(SqliteStore::open_in_memory().expect("база в памяти"))
}

/// Тот же стенд, но на базе файлом: тесты, проверяющие, что запись
/// действительно легла в таблицу, обязаны иметь второе соединение
/// к той же базе. Через `open_in_memory` второго соединения не бывает.
fn harness_on_disk() -> (Harness, std::path::PathBuf) {
    let path = std::env::temp_dir().join(format!("iaam-contract-{}.db", Uuid::new_v4()));
    let store = SqliteStore::open(&path).expect("база файлом");
    (harness_with(store), path)
}

fn harness_with(store: SqliteStore) -> Harness {
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();

    store
        .upsert_account(&AccountRecord {
            id: account,
            owner,
            title: "Брокерский".into(),
            institution: None,
        })
        .expect("счёт");

    let owner_token = "owner-secret-token";
    store
        .insert_token(
            &TokenRecord {
                id: Uuid::new_v4(),
                owner,
                label: "владелец".into(),
                scope: TokenScope::Owner,
                revoked: false,
            },
            &hash_token(owner_token),
        )
        .expect("токен владельца");

    let agent_token = "agent-secret-token";
    store
        .insert_token(
            &TokenRecord {
                id: Uuid::new_v4(),
                owner,
                label: "агент".into(),
                scope: TokenScope::Agent,
                revoked: false,
            },
            &hash_token(agent_token),
        )
        .expect("токен агента");

    let readonly_token = "read-only-token";
    store
        .insert_token(
            &TokenRecord {
                id: Uuid::new_v4(),
                owner,
                label: "чтение".into(),
                scope: TokenScope::ReadOnly,
                revoked: false,
            },
            &hash_token(readonly_token),
        )
        .expect("токен чтения");

    let services = Arc::new(AppServices::new(
        Arc::new(SqliteAdapter::new(store)),
        Arc::new(FixedClock(date!(2026 - 01 - 01))),
    ));
    let state = ServerState::new(
        services,
        Arc::new(RateLimiter::new(1_000, Duration::from_secs(60))),
    );
    let (router, api) = build(state);

    Harness {
        router,
        api,
        owner_token: owner_token.to_owned(),
        agent_token: agent_token.to_owned(),
        readonly_token: readonly_token.to_owned(),
        account,
        instrument: InstrumentId::new_random(),
        custody: CustodyId::new_random(),
    }
}

async fn call(router: &Router, request: Request<Body>) -> (StatusCode, Value) {
    let response = router
        .clone()
        .oneshot(request)
        .await
        .expect("обработчик ответил");
    let status = response.status();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("тело ответа")
        .to_bytes();
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, value)
}

fn get(path: &str, token: Option<&str>) -> Request<Body> {
    let mut builder = Request::builder().uri(path).method("GET");
    if let Some(token) = token {
        builder = builder.header("Authorization", format!("Bearer {token}"));
    }
    builder.body(Body::empty()).expect("запрос")
}

fn post(path: &str, token: &str, body: &Value) -> Request<Body> {
    Request::builder()
        .uri(path)
        .method("POST")
        .header("Authorization", format!("Bearer {token}"))
        .header("Content-Type", "application/json")
        .body(Body::from(body.to_string()))
        .expect("запрос")
}

#[tokio::test]
async fn health_is_public_and_reports_versions() {
    let harness = harness();
    let (status, body) = call(&harness.router, get("/v1/health", None)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "ok");
    // Версия 2: вариант события Valuation добавлен после заморозки v1,
    // и одна версия не может обозначать две схемы (§4.1).
    assert_eq!(body["schema_version"], 2);
    assert_eq!(body["projection_version"], 1);
}

#[tokio::test]
async fn every_documented_path_answers_something_other_than_404() {
    // Спека, описывающая несуществующий маршрут, — это инструкция
    // внешнему агенту чинить себя по неверной подсказке.
    let harness = harness();
    for (path, item) in harness.api.paths.paths.clone() {
        // `PathItem` в utoipa 5 хранит операции отдельными полями,
        // а не картой: перечисляем ровно те методы, которые использует API.
        let methods = [
            ("GET", item.get.is_some()),
            ("POST", item.post.is_some()),
            ("PUT", item.put.is_some()),
            ("DELETE", item.delete.is_some()),
            ("PATCH", item.patch.is_some()),
        ];
        for (verb, present) in methods {
            if !present {
                continue;
            }
            let request = Request::builder()
                .uri(path.replace("{id}", &Uuid::new_v4().to_string()))
                .method(verb)
                .header("Authorization", format!("Bearer {}", harness.owner_token))
                .header("Content-Type", "application/json")
                .body(Body::from("{}"))
                .expect("запрос");
            let (status, _) = call(&harness.router, request).await;
            assert_ne!(
                status,
                StatusCode::NOT_FOUND,
                "маршрут {path} {verb} описан в спеке, но не существует"
            );
            assert_ne!(
                status,
                StatusCode::METHOD_NOT_ALLOWED,
                "метод {verb} для {path} описан в спеке, но не поддерживается"
            );
        }
    }
}

#[tokio::test]
async fn a_request_without_a_token_is_rejected() {
    // Аутентификация с первого дня (§14).
    let harness = harness();
    let (status, body) = call(&harness.router, get("/v1/accounts", None)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["code"], "unauthorized");
}

#[tokio::test]
async fn an_unknown_token_is_rejected() {
    let harness = harness();
    let (status, _) = call(&harness.router, get("/v1/accounts", Some("чужой"))).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn a_read_only_token_may_not_submit_operations() {
    let harness = harness();
    let body = json!({
        "source_label": "тест",
        "operations": [{
            "account": harness.account.inner(),
            "type": "deposit",
            "amount": "1000.00",
            "currency": "RUB",
            "dates": { "cash_posted": "2025-01-01" }
        }]
    });
    let (status, response) = call(
        &harness.router,
        post("/v1/ingest/operations", &harness.readonly_token, &body),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(response["code"], "forbidden");
}

#[tokio::test]
async fn an_invalid_amount_is_reported_as_422_with_field_expected_actual() {
    let harness = harness();
    let body = json!({
        "source_label": "тест",
        "operations": [{
            "account": harness.account.inner(),
            "type": "deposit",
            "amount": "1000.005",
            "currency": "RUB",
            "dates": { "cash_posted": "2025-01-01" }
        }]
    });
    let (status, response) = call(
        &harness.router,
        post("/v1/ingest/operations", &harness.owner_token, &body),
    )
    .await;
    // Вердикт на строку, а не отказ всего документа (§10.1).
    assert_eq!(status, StatusCode::OK);
    assert_eq!(response[0]["verdict"], "rejected");
    assert_eq!(response[0]["field"], "amount");
    assert_eq!(response[0]["actual"], "1000.005");
}

#[tokio::test]
async fn the_stage_one_question_is_answered_end_to_end() {
    // Приёмочный критерий эпика через API: сколько внесено, сколько
    // выведено, какова доходность до налога.
    let harness = harness();

    let contour = json!({
        "title": "Мой портфель",
        "accounts": [harness.account.inner()],
    });
    let (status, contour_response) = call(
        &harness.router,
        post("/v1/contours", &harness.owner_token, &contour),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{contour_response}");
    let contour_id = contour_response["contour"]
        .as_str()
        .expect("контур")
        .to_owned();

    let operations = json!({
        "source_label": "ручной ввод",
        "operations": [
            {
                "account": harness.account.inner(),
                "type": "deposit",
                "amount": "100000.00",
                "currency": "RUB",
                "dates": { "cash_posted": "2025-01-01" },
                "idempotency_key": "dep-1"
            },
            {
                "account": harness.account.inner(),
                "type": "buy",
                "instrument": harness.instrument.inner(),
                "custody": harness.custody.inner(),
                "quantity": "100",
                "amount": "90000.00",
                "fee": "100.00",
                "currency": "RUB",
                "dates": { "trade": "2025-01-15", "cash_posted": "2025-01-15" }
            },
            {
                "account": harness.account.inner(),
                "type": "income",
                "instrument": harness.instrument.inner(),
                "amount": "3000.00",
                "currency": "RUB",
                "dates": { "cash_posted": "2025-07-01" }
            },
            {
                "account": harness.account.inner(),
                "type": "withdrawal",
                "amount": "10000.00",
                "currency": "RUB",
                "dates": { "cash_posted": "2025-09-01" }
            },
            {
                "account": harness.account.inner(),
                "type": "valuation",
                "instrument": harness.instrument.inner(),
                "price": "1000",
                "currency": "RUB",
                "quality": "previous_close",
                "dates": { "cash_posted": "2026-01-01" }
            }
        ]
    });
    let (status, verdicts) = call(
        &harness.router,
        post("/v1/ingest/operations", &harness.owner_token, &operations),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{verdicts}");
    for verdict in verdicts.as_array().expect("массив вердиктов") {
        assert_eq!(verdict["verdict"], "provisional", "{verdict}");
    }

    let (status, report) = call(
        &harness.router,
        get(
            &format!("/v1/reports/returns?contour={contour_id}&currency=RUB&as_of=2026-01-01"),
            Some(&harness.owner_token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{report}");
    // Масштаб сохраняется: рубль имеет две минимальные единицы, и сумма,
    // переведённая из проведённой в расчётную, остаётся с двумя знаками.
    assert_eq!(report["contributed"]["value"], "100000.00");
    assert_eq!(report["withdrawn"]["value"], "10000.00");
    // 2 900,00 рубля денег плюс 100 бумаг по 1 000 = 102 900,00.
    assert_eq!(report["terminal_value"]["value"], "102900.00");
    assert_eq!(report["history_starts"], "2025-01-01");
    assert_eq!(report["applied_rules"]["fx_source"], "owner_supplied");
    assert_eq!(report["applied_rules"]["day_count"], "act/365");

    // Ставка получена независимым эталоном (scripts/gen-xirr-fixtures.py),
    // а не выводом проверяемой программы (§15.5).
    let rate: f64 = report["xirr_pre_tax"]["value"]
        .as_str()
        .expect("ставка")
        .parse()
        .expect("число");
    assert!(
        (rate - 0.133_270_341_032).abs() < 1e-7,
        "ставка {rate} не совпадает с эталонной"
    );
    assert_eq!(report["data_quality"]["unconfirmed_share"], "1");
}

#[tokio::test]
async fn repeating_an_idempotent_operation_returns_the_same_event() {
    let harness = harness();
    let body = json!({
        "source_label": "тест",
        "operations": [{
            "account": harness.account.inner(),
            "type": "deposit",
            "amount": "1000.00",
            "currency": "RUB",
            "dates": { "cash_posted": "2025-01-01" },
            "idempotency_key": "one"
        }]
    });
    let (_, first) = call(
        &harness.router,
        post("/v1/ingest/operations", &harness.owner_token, &body),
    )
    .await;
    let (_, second) = call(
        &harness.router,
        post("/v1/ingest/operations", &harness.owner_token, &body),
    )
    .await;

    assert_eq!(first[0]["verdict"], "provisional");
    assert_eq!(second[0]["verdict"], "duplicate");
    assert_eq!(first[0]["event_id"], second[0]["event_id"]);
}

#[tokio::test]
async fn the_openapi_document_declares_bearer_security() {
    let harness = harness();
    let (status, spec) = call(&harness.router, get("/v1/openapi.json", None)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        spec["components"]["securitySchemes"]["bearer"].is_object(),
        "спека обязана описывать схему аутентификации"
    );
}

#[tokio::test]
async fn the_report_shape_is_frozen_by_a_snapshot() {
    // Поштучные проверки полей ловят неверное значение, но не ловят
    // исчезнувшее поле и не ловят появление лишнего. Снапшот ловит
    // форму целиком (§15.8).
    let harness = harness();
    let contour = json!({
        "title": "Снапшот",
        "accounts": [harness.account.inner()],
    });
    let (_, contour_response) = call(
        &harness.router,
        post("/v1/contours", &harness.owner_token, &contour),
    )
    .await;
    let contour_id = contour_response["contour"]
        .as_str()
        .expect("контур")
        .to_owned();

    let operations = json!({
        "source_label": "снапшот",
        "operations": [{
            "account": harness.account.inner(),
            "type": "deposit",
            "amount": "50000.00",
            "currency": "RUB",
            "dates": { "cash_posted": "2025-01-01" }
        }]
    });
    call(
        &harness.router,
        post("/v1/ingest/operations", &harness.owner_token, &operations),
    )
    .await;

    let (status, report) = call(
        &harness.router,
        get(
            &format!("/v1/reports/returns?contour={contour_id}&currency=RUB&as_of=2026-01-01"),
            Some(&harness.owner_token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    insta::assert_json_snapshot!(report, {
        ".applied_rules.contour" => "[contour]",
    });
}
#[tokio::test]
async fn an_agent_may_submit_but_may_not_administer() {
    // Область действия — заслон, а не подсказка. Агент отправляет
    // операции, но не заводит счета и не меняет состав контура: иначе
    // внешний агент, которому доверили ввод данных, получает право
    // переопределить границу контура и тем самым переписать доходность.
    let harness = harness();

    let (status, body) = call(
        &harness.router,
        post(
            "/v1/accounts",
            &harness.agent_token,
            &json!({ "title": "Чужой счёт" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(body["code"], "forbidden");

    let (status, body) = call(
        &harness.router,
        post(
            "/v1/contours",
            &harness.agent_token,
            &json!({ "title": "Свой контур", "accounts": [harness.account.inner()] }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");

    // Но отправлять операции — может.
    let (status, body) = call(
        &harness.router,
        post(
            "/v1/ingest/operations",
            &harness.agent_token,
            &json!({
                "source_label": "агент",
                "operations": [{
                    "account": harness.account.inner(),
                    "type": "deposit",
                    "amount": "1000.00",
                    "currency": "RUB",
                    "dates": { "cash_posted": "2025-01-01" },
                    "idempotency_key": "agent-1",
                }],
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body[0]["verdict"], "provisional");
}

#[tokio::test]
async fn a_created_account_appears_in_the_list_and_a_readonly_token_can_read_it() {
    // Счёт, который завели, обязан читаться обратно: пустой список
    // выглядит как «счетов нет», а не как «список сломан».
    let harness = harness();
    let (status, created) = call(
        &harness.router,
        post(
            "/v1/accounts",
            &harness.owner_token,
            &json!({ "title": "Второй брокерский", "institution": "Банк" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");

    let (status, list) = call(
        &harness.router,
        get("/v1/accounts", Some(&harness.readonly_token)),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let titles: Vec<&str> = list
        .as_array()
        .expect("список счетов")
        .iter()
        .map(|account| account["title"].as_str().expect("название"))
        .collect();
    assert!(
        titles.contains(&"Второй брокерский"),
        "заведённый счёт обязан быть в списке: {titles:?}"
    );
    assert!(titles.contains(&"Брокерский"), "и прежний тоже: {titles:?}");
}

#[tokio::test]
async fn each_verdict_names_the_row_it_belongs_to() {
    // Вердикты приходят по строке на операцию, и агент чинит именно ту,
    // которую ему назвали. Сбитая нумерация отправляет его править
    // здоровую строку, а больную оставляет как есть.
    let harness = harness();
    let (status, body) = call(
        &harness.router,
        post(
            "/v1/ingest/operations",
            &harness.owner_token,
            &json!({
                "source_label": "ручной ввод",
                "operations": [
                    {
                        "account": harness.account.inner(),
                        "type": "deposit",
                        "amount": "1000.00",
                        "currency": "RUB",
                        "dates": { "cash_posted": "2025-01-01" },
                        "idempotency_key": "row-1",
                    },
                    {
                        "account": harness.account.inner(),
                        "type": "deposit",
                        "amount": "-5.00",
                        "currency": "RUB",
                        "dates": { "cash_posted": "2025-01-02" },
                        "idempotency_key": "row-2",
                    },
                    {
                        "account": harness.account.inner(),
                        "type": "deposit",
                        "amount": "не число",
                        "currency": "RUB",
                        "dates": { "cash_posted": "2025-01-03" },
                        "idempotency_key": "row-3",
                    },
                    {
                        "account": harness.account.inner(),
                        "type": "deposit",
                        "amount": "2000.00",
                        "currency": "RUB",
                        "dates": { "cash_posted": "2025-01-04" },
                        "idempotency_key": "row-4",
                    },
                ],
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let rows: Vec<u64> = body
        .as_array()
        .expect("вердикты")
        .iter()
        .map(|verdict| verdict["row"].as_u64().expect("номер строки"))
        .collect();
    assert_eq!(
        rows,
        vec![1, 2, 3, 4],
        "нумерация начинается с единицы подряд"
    );
    assert_eq!(body[0]["verdict"], "provisional");
    // Вторая строка отклонена приёмкой: величина разобралась, но
    // отрицательной быть не может.
    assert_eq!(body[1]["verdict"], "rejected");
    // Третья — отклонена ещё на разборе тела запроса. Обе дороги к
    // вердикту нумеруют строки, и обе обязаны нумеровать одинаково.
    assert_eq!(body[2]["verdict"], "rejected");
    assert_eq!(body[3]["verdict"], "provisional");
}

#[tokio::test]
async fn a_csv_document_resolves_account_names_and_numbers_its_rows() {
    // Справочник имён строится из счетов владельца. Пустой справочник
    // отклонил бы весь документ по полю account, и «не завели счёт»
    // стало бы неотличимо от «сломался справочник».
    let harness = harness();
    let document = "date,type,account,counterparty_account,instrument,custody,quantity,amount,fee,accrued_interest,currency,idempotency_key\n\
        2025-01-01,deposit,Брокерский,,,,,1000.00,,,RUB,csv-1\n\
        2025-01-02,deposit,Нет такого счёта,,,,,1000.00,,,RUB,csv-2\n\
        2025-01-03,withdrawal,Брокерский,,,,,500.00,,,RUB,csv-3\n";
    let request = Request::builder()
        .uri("/v1/ingest/csv")
        .method("POST")
        .header("Authorization", format!("Bearer {}", harness.owner_token))
        .header("Content-Type", "text/csv")
        .body(Body::from(document))
        .expect("запрос");
    let (status, body) = call(&harness.router, request).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let verdicts = body.as_array().expect("вердикты");
    assert_eq!(verdicts.len(), 3);
    let rows: Vec<u64> = verdicts
        .iter()
        .map(|verdict| verdict["row"].as_u64().expect("номер строки"))
        .collect();
    assert_eq!(rows, vec![1, 2, 3]);
    assert_eq!(verdicts[0]["verdict"], "provisional");
    assert_eq!(verdicts[1]["verdict"], "rejected");
    assert_eq!(verdicts[1]["field"], "account");
    assert_eq!(verdicts[2]["verdict"], "provisional");
}

#[tokio::test]
async fn an_unparsable_report_date_is_refused_and_a_valid_one_is_honoured() {
    // Молчаливое умолчание «сегодня» вместо непонятой даты выдало бы
    // отчёт не на ту дату — с виду нормальный, но про другой период.
    let harness = harness();
    let (status, contour) = call(
        &harness.router,
        post(
            "/v1/contours",
            &harness.owner_token,
            &json!({ "title": "Портфель", "accounts": [harness.account.inner()] }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{contour}");
    let contour_id = contour["contour"].as_str().expect("контур").to_owned();

    let (status, _) = call(
        &harness.router,
        post(
            "/v1/ingest/operations",
            &harness.owner_token,
            &json!({
                "source_label": "ручной ввод",
                "operations": [{
                    "account": harness.account.inner(),
                    "type": "deposit",
                    "amount": "1000.00",
                    "currency": "RUB",
                    "dates": { "cash_posted": "2025-06-01" },
                    "idempotency_key": "as-of-1",
                }],
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, body) = call(
        &harness.router,
        get(
            &format!("/v1/reports/returns?contour={contour_id}&currency=RUB&as_of=вчера"),
            Some(&harness.owner_token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["field"], "as_of");

    // Дата раньше операции: отчёт на неё обязан отличаться от отчёта
    // на сегодня, иначе параметр ни на что не влияет.
    let (status, early) = call(
        &harness.router,
        get(
            &format!("/v1/reports/returns?contour={contour_id}&currency=RUB&as_of=2025-01-01"),
            Some(&harness.owner_token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{early}");
    assert_eq!(early["as_of"], "2025-01-01");

    let (status, today) = call(
        &harness.router,
        get(
            &format!("/v1/reports/returns?contour={contour_id}&currency=RUB"),
            Some(&harness.owner_token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{today}");
    assert_eq!(today["as_of"], "2026-01-01", "умолчание — дата часов");
    assert_ne!(
        early["contributed"], today["contributed"],
        "отчёт до первой операции обязан отличаться от отчёта после неё"
    );
}

#[tokio::test]
async fn a_report_for_today_leaves_a_snapshot_and_a_report_for_a_past_date_does_not() {
    // Ключ снимка — контур, его версия и версия правила; даты в ключе
    // нет. Снимок, построенный по срезу на прошлую дату, лёг бы под тем
    // же ключом и молча подменил бы состояние следующему запросу.
    // Проверяется прямым запросом к базе: снаружи подмена выглядит
    // как обычный ответ, просто с неверными числами.
    let (harness, path) = harness_on_disk();
    let (status, contour) = call(
        &harness.router,
        post(
            "/v1/contours",
            &harness.owner_token,
            &json!({ "title": "Портфель", "accounts": [harness.account.inner()] }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{contour}");
    let contour_id = contour["contour"].as_str().expect("контур").to_owned();

    let (status, _) = call(
        &harness.router,
        post(
            "/v1/ingest/operations",
            &harness.owner_token,
            &json!({
                "source_label": "ручной ввод",
                "operations": [{
                    "account": harness.account.inner(),
                    "type": "deposit",
                    "amount": "1000.00",
                    "currency": "RUB",
                    "dates": { "cash_posted": "2025-06-01" },
                    "idempotency_key": "snap-1",
                }],
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let snapshots = |path: &std::path::Path| -> u32 {
        let probe = SqliteStore::open(path).expect("второе соединение");
        probe
            .connection()
            .query_row("SELECT COUNT(*) FROM snapshots", [], |row| row.get(0))
            .expect("счёт снимков")
    };
    let usages = |path: &std::path::Path| -> u32 {
        let probe = SqliteStore::open(path).expect("второе соединение");
        probe
            .connection()
            .query_row("SELECT COUNT(*) FROM token_usage", [], |row| row.get(0))
            .expect("счёт обращений")
    };

    // Отчёт на прошлую дату снимка не оставляет.
    let (status, _) = call(
        &harness.router,
        get(
            &format!("/v1/reports/returns?contour={contour_id}&currency=RUB&as_of=2025-12-31"),
            Some(&harness.owner_token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        snapshots(&path),
        0,
        "снимок по срезу на прошлую дату сохраняться не должен"
    );

    // Отчёт на сегодня — оставляет, и он читается обратно: повторный
    // запрос обязан дать те же числа.
    let (status, first) = call(
        &harness.router,
        get(
            &format!("/v1/reports/returns?contour={contour_id}&currency=RUB"),
            Some(&harness.owner_token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(snapshots(&path), 1, "отчёт на сегодня оставляет снимок");

    let (status, second) = call(
        &harness.router,
        get(
            &format!("/v1/reports/returns?contour={contour_id}&currency=RUB"),
            Some(&harness.owner_token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        first, second,
        "отчёт, посчитанный со снимка, обязан совпасть с посчитанным без него"
    );
    assert_eq!(snapshots(&path), 1, "снимок заменяется, а не задваивается");

    // Каждое обращение с токеном попадает в журнал (§14).
    assert!(
        usages(&path) >= 4,
        "журнал обращений пуст: попытки с токеном обязаны быть видны"
    );

    drop(harness);
    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn an_event_added_behind_the_snapshot_boundary_forces_a_recompute_not_a_failure() {
    // Снимок — кэш, и его непригодность не является ошибкой работы.
    // Событие, пришедшее задним числом до границы снимка, меняет
    // отпечаток свёрнутого префикса: ядро отказывается продвигать
    // снимок, а оболочка обязана пересчитать журнал целиком и всё
    // равно ответить — причём ответить с учётом нового события.
    let harness = harness();
    let (status, contour) = call(
        &harness.router,
        post(
            "/v1/contours",
            &harness.owner_token,
            &json!({ "title": "Портфель", "accounts": [harness.account.inner()] }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{contour}");
    let contour_id = contour["contour"].as_str().expect("контур").to_owned();

    let deposit = |key: &str, day: &str, amount: &str| {
        json!({
            "source_label": "ручной ввод",
            "operations": [{
                "account": harness.account.inner(),
                "type": "deposit",
                "amount": amount,
                "currency": "RUB",
                "dates": { "cash_posted": day },
                "idempotency_key": key,
            }],
        })
    };

    let (status, _) = call(
        &harness.router,
        post(
            "/v1/ingest/operations",
            &harness.owner_token,
            &deposit("late", "2025-06-01", "1000.00"),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // Первый отчёт на сегодня оставляет снимок.
    let (status, before) = call(
        &harness.router,
        get(
            &format!("/v1/reports/returns?contour={contour_id}&currency=RUB"),
            Some(&harness.owner_token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{before}");
    assert_eq!(before["contributed"]["value"], "1000.00");

    // Событие задним числом — раньше уже свёрнутого.
    let (status, _) = call(
        &harness.router,
        post(
            "/v1/ingest/operations",
            &harness.owner_token,
            &deposit("early", "2025-01-01", "500.00"),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, after) = call(
        &harness.router,
        get(
            &format!("/v1/reports/returns?contour={contour_id}&currency=RUB"),
            Some(&harness.owner_token),
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "непригодный снимок — повод пересчитать, а не отказать: {after}"
    );
    assert_eq!(
        after["contributed"]["value"], "1500.00",
        "событие задним числом обязано войти в расчёт"
    );
    assert_eq!(
        after["history_starts"], "2025-01-01",
        "и сдвинуть начало истории"
    );
}
