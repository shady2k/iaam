//! Происхождение факта (§4.1).
//!
//! Восстановить эти данные позже невозможно, поэтому они обязательны
//! с первого коммита (§16.1).

use serde::{Deserialize, Serialize};

use crate::ids::SourceId;

/// Хеш сырой записи источника. Шестнадцатеричная строка SHA-256.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RawHash(String);

impl RawHash {
    /// Принимает только корректный шестнадцатеричный SHA-256.
    ///
    /// Логика проверки живёт здесь, а не в конструкторе с именем `new`:
    /// `cargo-mutants` молча пропускает функции с этим именем, и проверка
    /// формы хеша осталась бы невидимой мутационному заслону.
    #[must_use]
    pub fn parse(hex: &str) -> Option<Self> {
        let ok = hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit());
        ok.then(|| Self(hex.to_ascii_lowercase()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Версия парсера, породившего факт. Без неё нельзя отличить ошибку
/// источника от ошибки разбора, исправленной в более поздней версии.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ParserVersion(pub String);

/// Указание на конкретную строку исходного документа.
///
/// Документ назван хешом, а не именем файла: имя не является
/// тождеством — тот же отчёт, сохранённый под другим именем, перестал
/// бы дедуплицироваться (§10.6, уровень 4). Человеческое имя документа
/// хранится рядом с сырьём и разрешается по этому хешу.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RowLocator {
    pub document: RawHash,
    pub sheet: Option<String>,
    pub row: u64,
}

/// Происхождение. Сконструировать без хеша сырья и версии парсера нельзя.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Provenance {
    source: SourceId,
    raw_hash: RawHash,
    parser_version: ParserVersion,
    source_operation_id: Option<String>,
    row: Option<RowLocator>,
}

impl Provenance {
    /// Тривиальная упаковка полей: проверять при сборке нечего, обязательность
    /// хеша и версии парсера обеспечивает сама сигнатура, а не тело. Логики,
    /// которую стоило бы вынести из-под слепоты `cargo-mutants` к имени `new`,
    /// здесь нет (ср. [`crate::money::Money::new`]).
    #[must_use]
    pub fn new(source: SourceId, raw_hash: RawHash, parser_version: ParserVersion) -> Self {
        Self {
            source,
            raw_hash,
            parser_version,
            source_operation_id: None,
            row: None,
        }
    }

    #[must_use]
    pub fn with_source_operation_id(mut self, id: impl Into<String>) -> Self {
        self.source_operation_id = Some(id.into());
        self
    }

    #[must_use]
    pub fn with_row(mut self, row: RowLocator) -> Self {
        self.row = Some(row);
        self
    }

    #[must_use]
    pub const fn source(&self) -> SourceId {
        self.source
    }

    #[must_use]
    pub const fn raw_hash(&self) -> &RawHash {
        &self.raw_hash
    }

    /// Версия парсера. Читаема наравне с хешом: происхождение, из которого
    /// нельзя достать версию разбора, не отвечает на вопрос «чем это разобрано».
    #[must_use]
    pub const fn parser_version(&self) -> &ParserVersion {
        &self.parser_version
    }

    #[must_use]
    pub fn source_operation_id(&self) -> Option<&str> {
        self.source_operation_id.as_deref()
    }

    #[must_use]
    pub const fn row(&self) -> Option<&RowLocator> {
        self.row.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hash(seed: &str) -> RawHash {
        RawHash::parse(&seed.repeat(64)).unwrap()
    }

    #[test]
    fn raw_hash_rejects_malformed_input() {
        assert!(RawHash::parse("не хеш").is_none());
        assert!(RawHash::parse("abc").is_none());
        assert!(RawHash::parse(&"a".repeat(64)).is_some());
    }

    #[test]
    fn raw_hash_rejects_the_right_length_with_a_wrong_character() {
        // 64 символа, но не шестнадцатеричные: длины одной мало.
        let mut s = "a".repeat(63);
        s.push('z');
        assert_eq!(s.len(), 64);
        assert!(RawHash::parse(&s).is_none());
    }

    #[test]
    fn raw_hash_rejects_a_hash_that_is_one_character_too_long() {
        assert!(RawHash::parse(&"a".repeat(65)).is_none());
    }

    #[test]
    fn raw_hash_is_normalised_to_lowercase() {
        let h = RawHash::parse(&"A".repeat(64)).unwrap();
        assert_eq!(h.as_str(), "a".repeat(64));
    }

    #[test]
    fn provenance_keeps_the_source_hash_and_parser_version() {
        let source = SourceId::new_random();
        let p = Provenance::new(
            source,
            hash("a"),
            ParserVersion("tinkoff-xlsx/3".to_owned()),
        );
        assert_eq!(p.source(), source);
        assert_eq!(p.raw_hash(), &hash("a"));
        assert_eq!(
            p.parser_version(),
            &ParserVersion("tinkoff-xlsx/3".to_owned())
        );
    }

    #[test]
    fn optional_provenance_details_are_absent_until_set() {
        // Неизвестное — None, а не пустая строка (§4.9).
        let p = Provenance::new(
            SourceId::new_random(),
            hash("b"),
            ParserVersion("manual/1".to_owned()),
        );
        assert_eq!(p.source_operation_id(), None);
        assert_eq!(p.row(), None);
    }

    #[test]
    fn source_operation_id_is_recorded_when_given() {
        let p = Provenance::new(
            SourceId::new_random(),
            hash("c"),
            ParserVersion("manual/1".to_owned()),
        )
        .with_source_operation_id("OP-4417");
        assert_eq!(p.source_operation_id(), Some("OP-4417"));
    }

    #[test]
    fn row_locator_points_at_the_exact_line_of_the_document() {
        let row = RowLocator {
            document: hash("e"),
            sheet: Some("Сделки".to_owned()),
            row: 118,
        };
        let p = Provenance::new(
            SourceId::new_random(),
            hash("d"),
            ParserVersion("manual/1".to_owned()),
        )
        .with_row(row.clone());
        assert_eq!(p.row(), Some(&row));
    }
}
