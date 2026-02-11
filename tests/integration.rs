use polymarket_agent::config::Config;
use polymarket_agent::db::Database;

#[test]
fn test_config_loads_with_defaults() {
    let config = Config::from_env().unwrap();
    assert_eq!(
        config.trading_mode,
        polymarket_agent::config::TradingMode::Paper
    );
}

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

    assert!(tables.contains(&"markets".to_string()));
    assert!(tables.contains(&"trades".to_string()));
    assert!(tables.contains(&"positions".to_string()));
    assert!(tables.contains(&"bankroll_log".to_string()));
    assert!(tables.contains(&"cycle_log".to_string()));
    assert!(tables.contains(&"api_cost_log".to_string()));
}

#[test]
fn test_edge_detector_basic() {
    use polymarket_agent::edge_detector::EdgeDetector;
    use polymarket_agent::estimator::{AnalysisResult, FairValueEstimate};

    let detector = EdgeDetector::new(0.08);
    let result = AnalysisResult {
        market_id: "0xtest".to_string(),
        question: "Test?".to_string(),
        estimate: FairValueEstimate {
            probability: 0.75,
            confidence: 0.85,
            reasoning: "Test".to_string(),
            data_quality: "high".to_string(),
        },
        market_yes_price: 0.55,
        total_cost: 0.01,
        api_calls: vec![],
    };

    let opp = detector.detect(&result);
    assert!(opp.is_some());
    assert!((opp.unwrap().edge - 0.20).abs() < 0.001);
}
