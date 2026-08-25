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

use iaam_broker::environment::Environment;
use iaam_broker::trust::tinkoff_client;

// Обычный метод по адресу песочницы — рекомендуемый Т-Инвестициями
// способ. Метод песочницы по этому же адресу даёт `40003`, то есть
// жалобу на токен вместо жалобы на маршрут.
const METHOD: &str = "tinkoff.public.invest.api.contract.v1.UsersService/GetAccounts";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = tinkoff_client()?;
    let mut request = client
        .post(format!("{}/{METHOD}", Environment::Sandbox.base_url()))
        .header("Content-Type", "application/json")
        .body("{}");

    match env::var("IAAM_TINKOFF_SANDBOX_TOKEN_FILE") {
        Ok(path) => {
            let token = fs::read_to_string(&path)?;
            println!("токен прочитан из {path}");
            request = request.bearer_auth(token.trim());
        }
        Err(_) => {
            println!("токена нет: ожидается 401, и он подтвердит только то, что цепочка проверена")
        }
    }

    let response = request.send().await?;
    println!("HTTP {}", response.status());
    Ok(())
}
