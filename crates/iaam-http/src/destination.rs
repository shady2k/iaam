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
    /// Опубликованный контракт T-Invest API.
    ///
    /// Не шлюз брокера, а исходный текст контракта: по нему сверяется
    /// словарь видов операций. Отдельное назначение, потому что это
    /// другой хост и другой якорь доверия — вшитый корень шлюза здесь
    /// не при чём, а ходить в чужой репозиторий с ним значило бы
    /// утверждать, что это тот же узел.
    ///
    /// Читается только на чтение и только текст: ответ не влияет
    /// ни на одну сумму — он лишь называет коды, о которых стоит
    /// спросить владельца.
    TinvestContract,
}

impl Destination {
    /// Все назначения. Существует ради тестов, проходящих по таблице
    /// целиком: тест, перечисляющий варианты вручную, устаревает молча.
    pub const ALL: [Self; 7] = [
        Self::TinkoffProd,
        Self::TinkoffSandbox,
        Self::FinamApi,
        Self::MoexIss,
        Self::CbrScripts,
        Self::CbrDailyInfo,
        Self::TinvestContract,
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
            Self::TinvestContract => "https://raw.githubusercontent.com",
        }
    }
}
