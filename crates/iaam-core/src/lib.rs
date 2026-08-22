//! Ядро учёта инвестиций.
//!
//! Чистые синхронные функции над загруженным срезом данных.
//! Ни ввода-вывода, ни `async`, ни `Mutex`, ни зависимостей на другие крейты воркспейса.
//! См. §3.1 спецификации.

#[cfg(test)]
mod tests {
    #[test]
    fn workspace_builds() {
        assert_eq!(2 + 2, 4);
    }

    #[test]
    fn fixture_manifest_is_wired() {
        let raw = include_str!("../../../tests/fixtures/smoke.json");
        assert!(raw.contains("\"value\": 42"));
    }
}
