//! Разовая живая проверка канала Т-Инвестиций.
//!
//! Не тест: тесты сети не касаются. Эта команда существует, чтобы
//! убедиться, что вшитый корень доверия действительно проверяет цепочку
//! шлюза, и чтобы снять образцы ответов для замороженных фикстур.
//!
//! ```text
//! nix develop -c cargo run -p iaam-broker --example tinkoff-probe
//! ```
//!
//! Без токена ожидается `401`: этого достаточно, чтобы увидеть, что
//! рукопожатие TLS состоялось. С токеном путь к файлу передаётся
//! переменной `IAAM_TINKOFF_SANDBOX_TOKEN_FILE` — сам токен в аргументы
//! командной строки не попадает: список процессов виден всей машине.

use std::env;
use std::fs;

use iaam_http::client::HttpClient;
use iaam_http::{Destination, HttpRequest, RequestBody};

// Обычный метод по адресу песочницы — рекомендуемый Т-Инвестициями
// способ. Метод песочницы по этому же адресу даёт `40003`, то есть
// жалобу на токен вместо жалобы на маршрут.
const METHOD: &str = "tinkoff.public.invest.api.contract.v1.UsersService/GetAccounts";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = HttpClient::new();
    let mut request = HttpRequest::post(
        Destination::TinkoffSandbox,
        METHOD,
        RequestBody::Json("{}".to_owned()),
    );

    match env::var("IAAM_TINKOFF_SANDBOX_TOKEN_FILE") {
        Ok(path) => {
            let token = fs::read_to_string(&path)?;
            println!("токен прочитан из {path}");
            request = request.with_bearer(token.trim());
        }
        Err(_) => {
            println!("токена нет: ожидается 401, и он подтвердит только то, что цепочка проверена")
        }
    }

    let response = client.send(&request).await?;
    println!("HTTP {}", response.status);
    Ok(())
}
