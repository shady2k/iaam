//! Existing tax facts survive the optional instrument migration unchanged.
use iaam_store::schema::{MIGRATIONS, migrate};
use rusqlite::Connection;

#[test]
fn version_twelve_tax_history_keeps_its_origin_and_unknown_instrument() {
    let conn = Connection::open_in_memory().unwrap();
    for (version, sql) in MIGRATIONS {
        if version <= 12 {
            conn.execute_batch(sql).unwrap();
        }
    }
    conn.execute_batch(
        "PRAGMA user_version = 12;
         PRAGMA foreign_keys = ON;
         INSERT INTO accounts (id, owner, title, created_at)
         VALUES ('a', 'o', 'Account', '2026-01-01T00:00:00Z');
         INSERT INTO instruments (id, kind, symbol, title, denomination_currency, settlement_currency, quote_currency, created_at)
         VALUES ('i', 'bond', 'BOND', 'Bond', 'RUB', 'RUB', 'RUB', '2026-01-01T00:00:00Z');
         INSERT INTO events (id, owner, account, kind, effective_date, sequence, confidence, relation_kind, recorded_at, source, raw_hash, parser_version)
         VALUES ('e', 'o', 'a', 'tax', '2026-01-02', 1, 'known', 'none', '2026-01-02T00:00:00Z', 'file', 'hash', 'v1');
         INSERT INTO event_tax (event, origin) VALUES ('e', 'withheld_at_source');"
    ).unwrap();
    let indexes_before: Vec<String> = conn
        .prepare("SELECT name FROM sqlite_master WHERE type = 'index' ORDER BY name")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();

    migrate(&conn).expect("migrate populated version twelve");

    let tax: (String, String, Option<String>) = conn
        .query_row(
            "SELECT event, origin, instrument FROM event_tax",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(tax, ("e".into(), "withheld_at_source".into(), None));
    let indexes_after: Vec<String> = conn
        .prepare("SELECT name FROM sqlite_master WHERE type = 'index' ORDER BY name")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    // Later detail tables add their own primary-key indexes; the existing
    // indexes, including every index on the historical tax table, survive.
    assert!(
        indexes_before
            .iter()
            .all(|name| indexes_after.contains(name))
    );
    conn.execute(
        "UPDATE event_tax SET instrument = 'i' WHERE event = 'e'",
        [],
    )
    .unwrap();
    assert!(
        conn.execute("UPDATE event_tax SET instrument = 'missing'", [])
            .is_err()
    );
    assert!(
        conn.execute("UPDATE event_tax SET origin = 'unknown'", [])
            .is_err()
    );
    assert!(
        conn.execute("DELETE FROM events WHERE id = 'e'", [])
            .is_err()
    );
    let violations: i64 = conn
        .query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(violations, 0);
}

#[test]
fn version_twelve_broker_mappings_preserve_contract_and_owner_decisions() {
    use rusqlite::types::Value;

    fn mappings(conn: &Connection) -> Vec<Vec<Value>> {
        conn.prepare("SELECT * FROM broker_operation_kinds ORDER BY broker, source_kind")
            .unwrap()
            .query_map([], |row| (0..6).map(|column| row.get(column)).collect())
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }

    let conn = Connection::open_in_memory().unwrap();
    for (version, sql) in MIGRATIONS {
        if version <= 12 {
            conn.execute_batch(sql).unwrap();
        }
    }
    conn.execute_batch(
        "PRAGMA user_version = 12;
         INSERT INTO broker_operation_kinds
             (broker, source_kind, kind, origin, dictionary, recorded_at)
         VALUES
             ('tinkoff', 'coupon', 'coupon', 'contract', 'seed-v1', '2026-01-01T00:00:00Z'),
             ('tinkoff', 'redemption', 'deposit', 'owner', NULL, '2026-01-02T00:00:00Z');",
    )
    .unwrap();
    let before = mappings(&conn);
    assert!(conn.execute(
        "INSERT INTO broker_operation_kinds VALUES ('tinkoff', 'tax', 'tax', 'contract', 'seed-v2', '2026-01-03T00:00:00Z')",
        [],
    ).is_err());

    migrate(&conn).expect("migrate broker mappings");

    assert_eq!(mappings(&conn), before);
    conn.execute(
        "INSERT INTO broker_operation_kinds VALUES ('tinkoff', 'tax', 'tax', 'contract', 'seed-v2', '2026-01-03T00:00:00Z')",
        [],
    ).expect("tax mapping accepted");
    for sql in [
        "UPDATE broker_operation_kinds SET kind = 'unknown' WHERE source_kind = 'tax'",
        "UPDATE broker_operation_kinds SET origin = 'unknown' WHERE source_kind = 'tax'",
        "INSERT INTO broker_operation_kinds SELECT * FROM broker_operation_kinds WHERE source_kind = 'tax'",
    ] {
        assert!(conn.execute(sql, []).is_err());
    }
}
