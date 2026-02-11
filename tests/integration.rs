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

#[test]
fn test_position_sizer_kelly_basic() {
    use polymarket_agent::edge_detector::{EdgeOpportunity, TradeSide};
    use polymarket_agent::position_sizer::PositionSizer;

    let sizer = PositionSizer::new(0.5, 0.06, 0.40);
    let opp = EdgeOpportunity {
        market_id: "0xtest".to_string(),
        question: "Test?".to_string(),
        side: TradeSide::Yes,
        estimated_probability: 0.75,
        market_price: 0.55,
        edge: 0.20,
        confidence: 0.85,
        data_quality: "high".to_string(),
        reasoning: "Test".to_string(),
        analysis_cost: 0.01,
    };

    let result = sizer.size_position(&opp, 50.0, 0.0);
    assert!(!result.is_rejected());
    assert!(result.position_usd > 0.0);
    assert!(result.position_usd <= 3.0); // max 6% of 50
}

#[tokio::test]
async fn test_paper_trade_end_to_end() {
    use polymarket_agent::config::TradingMode;
    use polymarket_agent::edge_detector::{EdgeOpportunity, TradeSide};
    use polymarket_agent::executor::{Executor, TradeIntent};
    use polymarket_agent::position_sizer::PositionSizer;

    let db = Database::open_in_memory().unwrap();
    // Insert market for FK
    db.conn
        .execute(
            "INSERT INTO markets (condition_id, question, active) VALUES ('0xe2e', 'E2E test?', 1)",
            [],
        )
        .unwrap();
    db.ensure_bankroll_seeded(50.0).unwrap();

    let sizer = PositionSizer::new(0.5, 0.06, 0.40);
    let opp = EdgeOpportunity {
        market_id: "0xe2e".to_string(),
        question: "E2E test?".to_string(),
        side: TradeSide::Yes,
        estimated_probability: 0.75,
        market_price: 0.55,
        edge: 0.20,
        confidence: 0.85,
        data_quality: "high".to_string(),
        reasoning: "Test".to_string(),
        analysis_cost: 0.01,
    };

    let sizing = sizer.size_position(&opp, 50.0, 0.0);
    assert!(!sizing.is_rejected());

    let executor =
        Executor::new("http://unused:9999", TradingMode::Paper, 5).unwrap();
    let intent = TradeIntent {
        opportunity: opp,
        token_id: "tok_yes_e2e".to_string(),
        sizing,
    };
    let result = executor.execute(&intent, &db).await.unwrap();
    assert!(result.paper);
    assert_eq!(result.status, "filled");

    // Verify bankroll decreased
    let bankroll = db.get_current_bankroll().unwrap();
    assert!(bankroll < 50.0);

    // Verify position created
    let positions = db.get_open_positions().unwrap();
    assert_eq!(positions.len(), 1);
}
