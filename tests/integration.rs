use polymarket_agent::accounting::Accountant;
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

    let executor = Executor::new("http://unused:9999", TradingMode::Paper, 5).unwrap();
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

#[test]
fn test_accounting_api_cost_deduction() {
    let db = Database::open_in_memory().unwrap();
    db.ensure_bankroll_seeded(50.0).unwrap();

    // Log some API costs for cycle 1
    db.log_api_cost(1, None, "haiku", 500, 50, 0.05, "triage")
        .unwrap();
    db.log_api_cost(1, None, "sonnet", 2000, 200, 0.15, "analysis")
        .unwrap();

    let accountant = Accountant::new(200.0);
    let result = accountant.close_cycle(&db, 1).unwrap();

    assert!((result.bankroll_before - 50.0).abs() < f64::EPSILON);
    assert!((result.api_cost - 0.20).abs() < 1e-10);
    assert!((result.bankroll_after - 49.80).abs() < 1e-10);
    assert!(result.is_alive);

    // Verify bankroll was actually updated in DB
    let bankroll = db.get_current_bankroll().unwrap();
    assert!((bankroll - 49.80).abs() < 1e-10);
}

#[test]
fn test_accounting_death_condition() {
    let db = Database::open_in_memory().unwrap();
    db.ensure_bankroll_seeded(0.01).unwrap();

    // Log API cost exceeding bankroll
    db.log_api_cost(1, None, "sonnet", 2000, 200, 0.50, "analysis")
        .unwrap();

    let accountant = Accountant::new(200.0);
    let result = accountant.close_cycle(&db, 1).unwrap();

    assert!(!result.is_alive);
    assert!(result.bankroll_after < 0.0);
}

#[test]
fn test_cycle_number_persistence() {
    let db = Database::open_in_memory().unwrap();

    // Empty DB starts at cycle 1
    assert_eq!(db.get_next_cycle_number().unwrap(), 1);

    // Insert some cycle entries
    db.conn
        .execute(
            "INSERT INTO cycle_log (cycle_number, markets_scanned, trades_placed, api_cost_usd) VALUES (1, 10, 0, 0.01)",
            [],
        )
        .unwrap();
    db.conn
        .execute(
            "INSERT INTO cycle_log (cycle_number, markets_scanned, trades_placed, api_cost_usd) VALUES (2, 15, 1, 0.02)",
            [],
        )
        .unwrap();
    db.conn
        .execute(
            "INSERT INTO cycle_log (cycle_number, markets_scanned, trades_placed, api_cost_usd) VALUES (3, 12, 0, 0.03)",
            [],
        )
        .unwrap();

    // Should resume at cycle 4
    assert_eq!(db.get_next_cycle_number().unwrap(), 4);
}

#[test]
fn test_weather_market_parsing_and_bucket_lookup() {
    use polymarket_agent::weather_client::{
        get_weather_model_probability, parse_weather_market, BucketProbability,
        WeatherProbabilities,
    };

    let q =
        "Will the high temperature in New York City on February 20, 2026 be between 74°F and 76°F?";
    let info = parse_weather_market(q).unwrap();
    assert_eq!(info.city, "NYC");
    assert_eq!(info.date, "2026-02-20");
    assert_eq!(info.bucket_lower, 74.0);
    assert_eq!(info.bucket_upper, 76.0);

    let probs = WeatherProbabilities {
        city: "NYC".to_string(),
        station_icao: "KLGA".to_string(),
        forecast_date: "2026-02-20".to_string(),
        buckets: vec![
            BucketProbability {
                bucket_label: "72-74".to_string(),
                lower: 72.0,
                upper: 74.0,
                probability: 0.15,
            },
            BucketProbability {
                bucket_label: "74-76".to_string(),
                lower: 74.0,
                upper: 76.0,
                probability: 0.35,
            },
            BucketProbability {
                bucket_label: "76-78".to_string(),
                lower: 76.0,
                upper: 78.0,
                probability: 0.30,
            },
        ],
        ensemble_mean: 75.0,
        ensemble_std: 2.0,
        gefs_count: 31,
        ecmwf_count: 51,
    };

    let prob = get_weather_model_probability(&info, &probs).unwrap();
    assert!((prob - 0.35).abs() < 0.01);
}

#[tokio::test]
async fn test_weather_client_deserialization() {
    use polymarket_agent::weather_client::WeatherClient;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;

    let response_json = serde_json::json!({
        "city": "CHI",
        "station_icao": "KORD",
        "forecast_date": "2026-03-15",
        "buckets": [
            {"bucket_label": "40-42", "lower": 40.0, "upper": 42.0, "probability": 0.25},
            {"bucket_label": "42-44", "lower": 42.0, "upper": 44.0, "probability": 0.40},
            {"bucket_label": "44-46", "lower": 44.0, "upper": 46.0, "probability": 0.20},
        ],
        "ensemble_mean": 42.8,
        "ensemble_std": 2.1,
        "gefs_count": 31,
        "ecmwf_count": 51
    });

    Mock::given(method("GET"))
        .and(path("/weather/probabilities"))
        .respond_with(ResponseTemplate::new(200).set_body_json(response_json))
        .mount(&server)
        .await;

    let client = WeatherClient::new(&server.uri(), 5, 1).unwrap();
    let result = client.get_probabilities("CHI", "2026-03-15").await.unwrap();

    assert_eq!(result.city, "CHI");
    assert_eq!(result.station_icao, "KORD");
    assert_eq!(result.buckets.len(), 3);
    assert!((result.ensemble_mean - 42.8).abs() < 0.01);
    assert_eq!(result.gefs_count, 31);
    assert_eq!(result.ecmwf_count, 51);
}

#[test]
fn test_weather_non_weather_market_returns_none() {
    use polymarket_agent::weather_client::parse_weather_market;

    assert!(parse_weather_market("Will Bitcoin reach $100k?").is_none());
    assert!(parse_weather_market("Will the election happen?").is_none());
    assert!(parse_weather_market("").is_none());
}
