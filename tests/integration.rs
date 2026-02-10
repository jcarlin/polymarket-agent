use polymarket_agent::config::{Config, TradingMode};
use polymarket_agent::db::Database;

#[test]
fn test_database_tables_created() {
    let db = Database::open_in_memory().unwrap();

    let tables: Vec<String> = db
        .conn
        .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .filter_map(|r| r.ok())
        .collect();

    assert!(
        tables.contains(&"markets".to_string()),
        "markets table missing"
    );
    assert!(
        tables.contains(&"trades".to_string()),
        "trades table missing"
    );
    assert!(
        tables.contains(&"positions".to_string()),
        "positions table missing"
    );
    assert!(
        tables.contains(&"bankroll_log".to_string()),
        "bankroll_log table missing"
    );
    assert!(
        tables.contains(&"cycle_log".to_string()),
        "cycle_log table missing"
    );
}

#[test]
fn test_config_loads_with_defaults() {
    let config = Config::from_env().unwrap();
    assert_eq!(config.trading_mode, TradingMode::Paper);
    assert_eq!(config.sidecar_port, 9090);
    assert_eq!(config.scanner_page_size, 50);
}
