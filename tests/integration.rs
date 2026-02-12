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
        nws_forecast_high: None,
        bias_correction: None,
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

// ═══════════════════════════════════════
// Phase 7: Dashboard
// ═══════════════════════════════════════

#[tokio::test]
async fn test_dashboard_status_with_data() {
    use axum::body::Body;
    use axum::http::Request;
    use polymarket_agent::dashboard::AppState;
    use polymarket_agent::websocket::new_event_channel;
    use std::sync::{Arc, Mutex};
    use tower::ServiceExt;

    let db = Database::open_in_memory().unwrap();
    db.ensure_bankroll_seeded(50.0).unwrap();
    db.conn
        .execute(
            "INSERT INTO cycle_log (cycle_number, markets_scanned, markets_filtered, trades_placed, api_cost_usd, bankroll_before, bankroll_after) VALUES (1, 50, 10, 2, 0.15, 50.0, 49.85)",
            [],
        )
        .unwrap();
    db.conn
        .execute(
            "INSERT INTO markets (condition_id, question, active) VALUES ('0xdash', 'Dashboard test?', 1)",
            [],
        )
        .unwrap();
    db.insert_trade("dt1", "0xdash", "tok1", "YES", 0.60, 5.0, "filled", true)
        .unwrap();

    let state = AppState {
        db: Arc::new(Mutex::new(db)),
        event_tx: new_event_channel(),
        trading_mode: "paper".to_string(),
    };

    let app = axum::Router::new()
        .route(
            "/api/status",
            axum::routing::get(
                |axum::extract::State(s): axum::extract::State<AppState>| async move {
                    let db = s.db.lock().unwrap();
                    let bankroll = db.get_current_bankroll().unwrap_or(0.0);
                    let total_trades = db.get_total_trades_count().unwrap_or(0);
                    let next_cycle = db.get_next_cycle_number().unwrap_or(1);
                    axum::Json(serde_json::json!({
                        "trading_mode": s.trading_mode,
                        "bankroll": bankroll,
                        "total_trades": total_trades,
                        "next_cycle": next_cycle,
                    }))
                },
            ),
        )
        .with_state(state);

    let resp = app
        .oneshot(Request::get("/api/status").body(Body::empty()).unwrap())
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let body = axum::body::to_bytes(resp.into_body(), 1_000_000)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["bankroll"], 50.0);
    assert_eq!(json["total_trades"], 1);
    assert_eq!(json["next_cycle"], 2);
    assert_eq!(json["trading_mode"], "paper");
}

#[tokio::test]
async fn test_dashboard_positions_with_data() {
    use axum::body::Body;
    use axum::http::Request;
    use polymarket_agent::dashboard::AppState;
    use polymarket_agent::websocket::new_event_channel;
    use std::sync::{Arc, Mutex};
    use tower::ServiceExt;

    let db = Database::open_in_memory().unwrap();
    db.ensure_bankroll_seeded(50.0).unwrap();
    db.conn
        .execute(
            "INSERT INTO markets (condition_id, question, active) VALUES ('0xpos', 'Position test?', 1)",
            [],
        )
        .unwrap();
    db.upsert_position_with_estimate("0xpos", "tok1", "YES", 0.60, 10.0, Some(0.75))
        .unwrap();

    let state = AppState {
        db: Arc::new(Mutex::new(db)),
        event_tx: new_event_channel(),
        trading_mode: "paper".to_string(),
    };

    let app = axum::Router::new()
        .route(
            "/api/positions",
            axum::routing::get(
                |axum::extract::State(s): axum::extract::State<AppState>| async move {
                    let db = s.db.lock().unwrap();
                    let positions = db.get_open_positions_with_market().unwrap_or_default();
                    let resp: Vec<serde_json::Value> = positions
                        .into_iter()
                        .map(|p| {
                            serde_json::json!({
                                "market_condition_id": p.market_condition_id,
                                "side": p.side,
                                "entry_price": p.entry_price,
                                "size": p.size,
                                "question": p.question,
                            })
                        })
                        .collect();
                    axum::Json(resp)
                },
            ),
        )
        .with_state(state);

    let resp = app
        .oneshot(Request::get("/api/positions").body(Body::empty()).unwrap())
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let body = axum::body::to_bytes(resp.into_body(), 1_000_000)
        .await
        .unwrap();
    let json: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();
    assert_eq!(json.len(), 1);
    assert_eq!(json[0]["market_condition_id"], "0xpos");
    assert_eq!(json[0]["side"], "YES");
}

#[tokio::test]
async fn test_dashboard_history_endpoint() {
    use axum::body::Body;
    use axum::http::Request;
    use polymarket_agent::dashboard::AppState;
    use polymarket_agent::websocket::new_event_channel;
    use std::sync::{Arc, Mutex};
    use tower::ServiceExt;

    let db = Database::open_in_memory().unwrap();
    db.ensure_bankroll_seeded(50.0).unwrap();
    for i in 1..=5 {
        db.conn
            .execute(
                "INSERT INTO cycle_log (cycle_number, markets_scanned, markets_filtered, trades_placed, api_cost_usd, bankroll_before, bankroll_after) VALUES (?1, 20, 5, 1, 0.10, ?2, ?3)",
                rusqlite::params![i, 50.0 - (i as f64 - 1.0) * 0.10, 50.0 - i as f64 * 0.10],
            )
            .unwrap();
    }

    let state = AppState {
        db: Arc::new(Mutex::new(db)),
        event_tx: new_event_channel(),
        trading_mode: "paper".to_string(),
    };

    let app = axum::Router::new()
        .route(
            "/api/history",
            axum::routing::get(
                |axum::extract::State(s): axum::extract::State<AppState>| async move {
                    let db = s.db.lock().unwrap();
                    let mut stmt = db.conn.prepare(
                "SELECT cycle_number, bankroll_after FROM cycle_log ORDER BY cycle_number"
            ).unwrap();
                    let rows: Vec<serde_json::Value> = stmt
                        .query_map([], |row| {
                            Ok(serde_json::json!({
                                "cycle_number": row.get::<_, i64>(0)?,
                                "bankroll_after": row.get::<_, f64>(1)?,
                            }))
                        })
                        .unwrap()
                        .filter_map(|r| r.ok())
                        .collect();
                    axum::Json(rows)
                },
            ),
        )
        .with_state(state);

    let resp = app
        .oneshot(Request::get("/api/history").body(Body::empty()).unwrap())
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let body = axum::body::to_bytes(resp.into_body(), 1_000_000)
        .await
        .unwrap();
    let json: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();
    assert_eq!(json.len(), 5);
    // Verify ordered by cycle_number
    assert_eq!(json[0]["cycle_number"], 1);
    assert_eq!(json[4]["cycle_number"], 5);
}

// ═══════════════════════════════════════
// Phase 6: Position Management & Risk
// ═══════════════════════════════════════

#[test]
fn test_stop_loss_end_to_end() {
    use polymarket_agent::db::PositionRow;
    use polymarket_agent::position_manager::{PositionAction, PositionManager};

    let db = Database::open_in_memory().unwrap();

    // Create a market and position
    db.conn
        .execute(
            "INSERT INTO markets (condition_id, question, active) VALUES ('0xsl', 'Stop-loss test?', 1)",
            [],
        )
        .unwrap();
    db.ensure_bankroll_seeded(50.0).unwrap();
    db.upsert_position("0xsl", "tok_yes_sl", "YES", 0.60, 10.0)
        .unwrap();

    // Simulate price drop beyond stop-loss threshold
    let pos = PositionRow {
        market_condition_id: "0xsl".to_string(),
        token_id: "tok_yes_sl".to_string(),
        side: "YES".to_string(),
        entry_price: 0.60,
        size: 10.0,
        status: "open".to_string(),
        current_price: Some(0.50),
        unrealized_pnl: -1.0,
        estimated_probability: None,
        question: Some("Stop-loss test?".to_string()),
    };

    let mgr = PositionManager::new(0.15, 0.90, 0.02, 3.0, 5000.0, 0.15, 0.25);
    // Price at 0.50 = 16.7% loss > 15% threshold
    let action = mgr.evaluate_position(&pos, 0.50);
    assert!(matches!(action, PositionAction::Exit { .. }));

    // Simulate exit by closing position in DB
    let pnl = db.close_position("0xsl", "YES", 0.50).unwrap();
    assert!((pnl - (-1.0)).abs() < 0.01); // (0.50 - 0.60) * 10 = -1.0

    // Position should now be closed
    assert!(db.get_open_positions().unwrap().is_empty());
}

#[test]
fn test_drawdown_reduces_sizing() {
    use polymarket_agent::position_manager::PositionManager;
    use polymarket_agent::position_sizer::PositionSizer;

    let db = Database::open_in_memory().unwrap();
    db.ensure_bankroll_seeded(100.0).unwrap();

    // Set peak to 100, then drop to 65 (35% drawdown > 30% threshold)
    db.update_peak_bankroll(100.0).unwrap();
    let state = PositionManager::check_drawdown(&db, 65.0, 0.30).unwrap();
    assert!(state.is_circuit_breaker_active);

    // Normal sizer: kelly_fraction = 0.5
    let normal_sizer = PositionSizer::new(0.5, 0.06, 0.40);

    // Drawdown sizer: kelly_fraction = 0.5 * 0.5 = 0.25
    let drawdown_sizer = PositionSizer::new(0.25, 0.06, 0.40);

    let opp = polymarket_agent::edge_detector::EdgeOpportunity {
        market_id: "0xdd".to_string(),
        question: "Drawdown test?".to_string(),
        side: polymarket_agent::edge_detector::TradeSide::Yes,
        estimated_probability: 0.80,
        market_price: 0.50,
        edge: 0.30,
        confidence: 0.85,
        data_quality: "high".to_string(),
        reasoning: "Test".to_string(),
        analysis_cost: 0.01,
    };

    let normal_sizing = normal_sizer.size_position(&opp, 65.0, 0.0);
    let drawdown_sizing = drawdown_sizer.size_position(&opp, 65.0, 0.0);

    // Both should succeed, but drawdown sizing should be smaller
    assert!(!normal_sizing.is_rejected());
    assert!(!drawdown_sizing.is_rejected());
    assert!(drawdown_sizing.position_usd <= normal_sizing.position_usd);
}

#[test]
fn test_correlated_exposure_blocks_trade() {
    use polymarket_agent::db::PositionRow;
    use polymarket_agent::position_manager::PositionManager;

    let mgr = PositionManager::new(0.15, 0.90, 0.02, 3.0, 5000.0, 0.15, 0.25);

    // Create positions in the Northeast group (NYC + BOS)
    let positions = vec![
        PositionRow {
            market_condition_id: "0xnyc".to_string(),
            token_id: "tok_nyc".to_string(),
            side: "YES".to_string(),
            entry_price: 0.50,
            size: 20.0,
            status: "open".to_string(),
            current_price: None,
            unrealized_pnl: 0.0,
            estimated_probability: None,
            question: Some(
                "Will the high temperature in New York City on February 20, 2026 be between 40°F and 42°F?"
                    .to_string(),
            ),
        },
        PositionRow {
            market_condition_id: "0xbos".to_string(),
            token_id: "tok_bos".to_string(),
            side: "YES".to_string(),
            entry_price: 0.50,
            size: 20.0,
            status: "open".to_string(),
            current_price: None,
            unrealized_pnl: 0.0,
            estimated_probability: None,
            question: Some(
                "Will the high temperature in Boston on February 20, 2026 be between 38°F and 40°F?"
                    .to_string(),
            ),
        },
    ];

    // NE exposure = 10 + 10 = 20.0, limit = 0.15 * 100 = 15.0
    // Philadelphia is also in NE group — should be blocked
    let phl_question =
        "Will the high temperature in Philadelphia on February 20, 2026 be between 42°F and 44°F?";
    assert!(mgr.is_correlated_group_over_limit(phl_question, &positions, 100.0));

    // Chicago is in Midwest — should NOT be blocked
    let chi_question =
        "Will the high temperature in Chicago on February 20, 2026 be between 30°F and 32°F?";
    assert!(!mgr.is_correlated_group_over_limit(chi_question, &positions, 100.0));
}

#[test]
fn test_position_management_db_roundtrip() {
    // Test the full DB lifecycle: create position → update price → check alert → close
    let db = Database::open_in_memory().unwrap();
    db.conn
        .execute(
            "INSERT INTO markets (condition_id, question, active) VALUES ('0xrt', 'Roundtrip test?', 1)",
            [],
        )
        .unwrap();
    db.ensure_bankroll_seeded(50.0).unwrap();

    // Create position with estimated probability
    db.upsert_position_with_estimate("0xrt", "tok_yes_rt", "YES", 0.60, 10.0, Some(0.75))
        .unwrap();

    // Update price
    db.update_position_price("0xrt", "YES", 0.70).unwrap();

    // Verify current price and unrealized P&L
    let positions = db.get_open_positions().unwrap();
    assert_eq!(positions.len(), 1);
    assert!((positions[0].current_price.unwrap() - 0.70).abs() < f64::EPSILON);
    assert!((positions[0].unrealized_pnl - 1.0).abs() < 0.01); // (0.70 - 0.60) * 10

    // Log an alert
    db.log_position_alert(
        "0xrt",
        "stop_loss",
        "Price approaching threshold",
        "monitoring",
        1,
    )
    .unwrap();

    // Close position
    let pnl = db.close_position("0xrt", "YES", 0.70).unwrap();
    assert!((pnl - 1.0).abs() < 0.01);
    assert!(db.get_open_positions().unwrap().is_empty());

    // Bankroll log should still have seed entry
    let bankroll = db.get_current_bankroll().unwrap();
    assert!((bankroll - 50.0).abs() < f64::EPSILON);
}
