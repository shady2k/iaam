//! Среда брокера: боевая и песочница (§14).
//!
//! Т-Инвестиции зовут это контуром, но слово «контур» в этой системе
//! занято составом контура учёта (§11), и второе значение читалось бы
//! как первое. Здесь — среда.
//!
//! Среды различаются не только адресом. Токены у них **разные**: боевой
//! токен песочный шлюз встречает `401` «Authentication token is missing
//! or invalid», а песочный на боевых методах даёт отказ. Поэтому среда —
//! свойство заведённого доступа, а не параметр отдельного обращения:
//! среда, выбранная вызывающим по ошибке, — это поход не туда,
//! замеченный по чужому ответу.
//!
//! Адрес шлюза выводится из среды, а не хранится рядом с доступом:
//! адрес — свойство среды, и запись, приносящая свой собственный,
//! означала бы, что заведённый доступ умеет увести программу куда угодно.

/// Среда брокерского канала.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Environment {
    /// Боевая: настоящие деньги и настоящая история сделок.
    Prod,
    /// Песочница: эмуляция торгов. Брокерского отчёта в ней нет.
    Sandbox,
}

impl Environment {
    /// Код для хранилища. Хранилище среду не толкует — оно хранит.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Prod => "prod",
            Self::Sandbox => "sandbox",
        }
    }

    /// Разбор кода.
    ///
    /// Не `trim` и не приведение регистра: значение пишет система,
    /// а не человек, и «почти то же самое» здесь означает, что запись
    /// изменил кто-то другой.
    #[must_use]
    pub fn parse(code: &str) -> Option<Self> {
        match code {
            "prod" => Some(Self::Prod),
            "sandbox" => Some(Self::Sandbox),
            _ => None,
        }
    }

    /// Адрес шлюза.
    #[must_use]
    pub const fn base_url(self) -> &'static str {
        match self {
            Self::Prod => "https://invest-public-api.tbank.ru/rest",
            Self::Sandbox => "https://sandbox-invest-public-api.tbank.ru/rest",
        }
    }

    /// Бывает ли метод в этой среде.
    ///
    /// Двух методов в песочнице нет вовсе — брокерского отчёта и справки
    /// о доходах за пределами РФ. Отказ выдаётся здесь, до похода: шлюз
    /// на такой вызов отвечает пустым ответом, а пустой отчёт неотличим
    /// от отчёта, в котором ничего не было. Это худший вид ошибки — тихая.
    #[must_use]
    pub fn serves(self, method: Method) -> bool {
        match self {
            Self::Prod => true,
            Self::Sandbox => match method {
                Method::BrokerReport | Method::DividendsForeignIssuer => false,
                Method::Accounts | Method::Operations | Method::Portfolio => true,
            },
        }
    }
}

/// Метод шлюза в той мере, в какой среда о нём знает.
///
/// Перечисление, а не строка с полным именем метода: строка отвечает
/// на «что послать», а здесь нужен ответ на «бывает ли такое здесь»,
/// и опечатка в строке дала бы молчаливое «бывает».
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Method {
    Accounts,
    Operations,
    Portfolio,
    BrokerReport,
    DividendsForeignIssuer,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_environment_survives_the_round_trip_through_its_code() {
        for environment in [Environment::Prod, Environment::Sandbox] {
            assert_eq!(Environment::parse(environment.code()), Some(environment));
        }
    }

    #[test]
    fn nothing_else_parses_as_an_environment() {
        // «Почти то же самое» — это чужая запись, а не наша.
        for code in ["", "PROD", " prod", "prod ", "песочница", "test"] {
            assert_eq!(Environment::parse(code), None, "{code}");
        }
    }

    #[test]
    fn the_environments_never_share_an_address() {
        // Один адрес на две среды означал бы боевые сделки
        // из проверочного прогона.
        assert_ne!(
            Environment::Prod.base_url(),
            Environment::Sandbox.base_url()
        );
        assert!(Environment::Sandbox.base_url().contains("sandbox"));
        assert!(!Environment::Prod.base_url().contains("sandbox"));
    }

    #[test]
    fn the_report_is_refused_in_the_sandbox_and_served_in_prod() {
        assert!(!Environment::Sandbox.serves(Method::BrokerReport));
        assert!(!Environment::Sandbox.serves(Method::DividendsForeignIssuer));
        assert!(Environment::Prod.serves(Method::BrokerReport));
        assert!(Environment::Prod.serves(Method::DividendsForeignIssuer));
    }

    #[test]
    fn what_the_sandbox_does_have_is_not_refused() {
        for method in [Method::Accounts, Method::Operations, Method::Portfolio] {
            assert!(Environment::Sandbox.serves(method), "{method:?}");
        }
    }
}
