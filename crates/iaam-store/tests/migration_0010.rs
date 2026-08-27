//! Миграция 0010: снимки графика выплат облигаций.

use rusqlite::Connection;

// База версии 9 собирается применением прежних миграций к пустому
// соединению — тот же приём, что в `migration_0008.rs`. Публичного
// конструктора из готового `Connection` у `SqliteStore` нет, и заводить
// его ради теста значило бы расширять API под тест.
fn database_at_version_nine() -> Connection {
    let conn = Connection::open_in_memory().expect("база в памяти");
    conn.execute_batch(
        "CREATE TABLE instruments (
             id       TEXT PRIMARY KEY,
             symbol   TEXT NOT NULL,
             title    TEXT NOT NULL,
             currency TEXT NOT NULL
         ) STRICT;
         INSERT INTO instruments (id, symbol, title, currency)
         VALUES ('instrument-1', 'SU46020RMFS2', 'ОФЗ 46020', 'RUB');
         PRAGMA user_version = 9;",
    )
    .expect("схема версии 9");
    conn
}

#[test]
fn a_snapshot_row_cannot_be_rewritten() {
    // Снимок — наблюдение: исправление источника ложится новым снимком,
    // а не правкой старого. Иначе воспроизводимость отчёта на прошлую
    // координату знания теряется безвозвратно.
    let conn = database_at_version_nine();
    iaam_store::schema::migrate(&conn).expect("миграция до 10");
    conn.execute_batch(
        "INSERT INTO schedule_snapshots
             (id, instrument_id, source_id, observed_at, content_hash, recorded_at)
         VALUES ('snap-1', 'instrument-1', 'moex-iss',
                 '2026-08-27T12:00:00Z', 'hash-1', '2026-08-27T12:00:00Z');",
    )
    .expect("снимок записан");

    let rewritten = conn.execute(
        "UPDATE schedule_snapshots SET content_hash = 'hash-2' WHERE id = 'snap-1'",
        [],
    );
    assert!(rewritten.is_err(), "правка снимка обязана быть запрещена");

    let deleted = conn.execute("DELETE FROM schedule_snapshots WHERE id = 'snap-1'", []);
    assert!(deleted.is_err(), "удаление снимка обязано быть запрещено");
}

#[test]
fn a_coupon_row_belongs_to_a_snapshot_and_has_no_own_knowledge_axis() {
    // Ось знания принадлежит снимку. Колонка observed_at в строке вернула
    // бы построчную модель, которая не умеет выразить исчезновение строки.
    let conn = database_at_version_nine();
    iaam_store::schema::migrate(&conn).expect("миграция до 10");
    let mut statement = conn
        .prepare("SELECT name FROM pragma_table_info('schedule_coupon_periods')")
        .expect("описание таблицы");
    let columns: Vec<String> = statement
        .query_map([], |row| row.get(0))
        .expect("чтение колонок")
        .collect::<Result<_, _>>()
        .expect("колонки");
    assert!(columns.iter().any(|c| c == "snapshot_id"));
    assert!(
        !columns.iter().any(|c| c == "observed_at"),
        "у строки графика своей оси знания быть не должно: {columns:?}"
    );
}
