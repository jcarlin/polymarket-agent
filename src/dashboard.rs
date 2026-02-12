use anyhow::{Context, Result};
use axum::{
    extract::{Query, State},
    response::{Html, IntoResponse},
    routing::get,
    Json, Router,
};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};
use tower_http::validate_request::ValidateRequestHeaderLayer;
use tracing::info;

use crate::config::Config;
use crate::db::Database;
use crate::websocket::{ws_handler, EventSender};

/// Shared state for the dashboard server.
#[derive(Clone)]
pub struct AppState {
    pub db: Arc<Mutex<Database>>,
    pub event_tx: EventSender,
    pub trading_mode: String,
}

// ─── REST response types ───────────────────────────────

#[derive(Serialize)]
struct StatusResponse {
    trading_mode: String,
    bankroll: f64,
    peak_bankroll: f64,
    exposure: f64,
    total_trades: i64,
    next_cycle: i64,
    api_cost_24h: f64,
    total_trading_fees: f64,
}

#[derive(Serialize)]
struct PositionResponse {
    market_condition_id: String,
    side: String,
    entry_price: f64,
    current_price: Option<f64>,
    size: f64,
    unrealized_pnl: f64,
    estimated_probability: Option<f64>,
    question: Option<String>,
}

#[derive(Serialize)]
struct TradeResponse {
    trade_id: String,
    market_condition_id: String,
    side: String,
    price: f64,
    size: f64,
    status: String,
    paper: bool,
    created_at: String,
    question: Option<String>,
    realized_pnl: Option<f64>,
    unrealized_pnl: Option<f64>,
    position_status: Option<String>,
    entry_fee: f64,
}

#[derive(Serialize)]
struct CycleHistoryRow {
    cycle_number: i64,
    markets_scanned: i64,
    markets_filtered: i64,
    trades_placed: i64,
    api_cost_usd: f64,
    bankroll_before: Option<f64>,
    bankroll_after: Option<f64>,
    created_at: String,
}

#[derive(Serialize)]
struct AlertRow {
    id: i64,
    market_condition_id: String,
    alert_type: String,
    details: Option<String>,
    action_taken: Option<String>,
    cycle_number: Option<i64>,
    created_at: String,
}

#[derive(Serialize)]
struct WeatherResponse {
    city: String,
    forecast_date: String,
    ensemble_mean: f64,
    ensemble_std: f64,
    gefs_count: i32,
    ecmwf_count: i32,
    buckets: serde_json::Value,
    cycle_number: i64,
}

#[derive(Serialize)]
struct OpportunityResponse {
    question: String,
    side: String,
    market_price: f64,
    estimated_probability: f64,
    edge: f64,
    confidence: f64,
    status: String,
    reject_reason: Option<String>,
    cycle_number: i64,
    created_at: Option<String>,
}

#[derive(Deserialize)]
pub struct TradesQuery {
    limit: Option<i64>,
}

#[derive(Deserialize)]
pub struct OpportunitiesQuery {
    limit: Option<i64>,
}

// ─── Handlers ──────────────────────────────────────────

async fn api_status(State(state): State<AppState>) -> impl IntoResponse {
    let db = state.db.lock().unwrap();
    let bankroll = db.get_current_bankroll().unwrap_or(0.0);
    let peak_bankroll = db.get_peak_bankroll().unwrap_or(0.0);
    let exposure = db.get_total_exposure().unwrap_or(0.0);
    let total_trades = db.get_total_trades_count().unwrap_or(0);
    let next_cycle = db.get_next_cycle_number().unwrap_or(1);
    let api_cost_24h = db.get_api_cost_since(24).unwrap_or(0.0);
    let total_trading_fees = db.get_total_trading_fees();

    Json(StatusResponse {
        trading_mode: state.trading_mode.clone(),
        bankroll,
        peak_bankroll,
        exposure,
        total_trades,
        next_cycle,
        api_cost_24h,
        total_trading_fees,
    })
}

async fn api_positions(State(state): State<AppState>) -> impl IntoResponse {
    let db = state.db.lock().unwrap();
    let positions = db.get_open_positions_with_market().unwrap_or_default();

    let resp: Vec<PositionResponse> = positions
        .into_iter()
        .map(|p| PositionResponse {
            market_condition_id: p.market_condition_id,
            side: p.side,
            entry_price: p.entry_price,
            current_price: p.current_price,
            size: p.size,
            unrealized_pnl: p.unrealized_pnl,
            estimated_probability: p.estimated_probability,
            question: p.question,
        })
        .collect();

    Json(resp)
}

async fn api_trades(
    State(state): State<AppState>,
    Query(params): Query<TradesQuery>,
) -> impl IntoResponse {
    let limit = params.limit.unwrap_or(50).min(200);
    let db = state.db.lock().unwrap();
    let trades = db.get_recent_trades(limit).unwrap_or_default();

    let resp: Vec<TradeResponse> = trades
        .into_iter()
        .map(|t| TradeResponse {
            trade_id: t.trade_id,
            market_condition_id: t.market_condition_id,
            side: t.side,
            price: t.price,
            size: t.size,
            status: t.status,
            paper: t.paper,
            created_at: t.created_at,
            question: t.question,
            realized_pnl: t.realized_pnl,
            unrealized_pnl: t.unrealized_pnl,
            position_status: t.position_status,
            entry_fee: t.entry_fee,
        })
        .collect();

    Json(resp)
}

async fn api_history(State(state): State<AppState>) -> impl IntoResponse {
    let db = state.db.lock().unwrap();
    let mut stmt = match db.conn.prepare(
        "SELECT cycle_number, markets_scanned, markets_filtered, trades_placed, \
         api_cost_usd, bankroll_before, bankroll_after, created_at \
         FROM cycle_log ORDER BY cycle_number",
    ) {
        Ok(s) => s,
        Err(_) => return Json(Vec::<CycleHistoryRow>::new()),
    };

    let history: Vec<CycleHistoryRow> = match stmt.query_map([], |row| {
        Ok(CycleHistoryRow {
            cycle_number: row.get(0)?,
            markets_scanned: row.get(1)?,
            markets_filtered: row.get(2)?,
            trades_placed: row.get(3)?,
            api_cost_usd: row.get(4)?,
            bankroll_before: row.get(5)?,
            bankroll_after: row.get(6)?,
            created_at: row.get(7)?,
        })
    }) {
        Ok(rows) => rows.filter_map(|r| r.ok()).collect(),
        Err(_) => Vec::new(),
    };
    Json(history)
}

async fn api_alerts(State(state): State<AppState>) -> impl IntoResponse {
    let db = state.db.lock().unwrap();
    let mut stmt = match db.conn.prepare(
        "SELECT id, market_condition_id, alert_type, details, action_taken, \
         cycle_number, created_at \
         FROM position_alerts ORDER BY id DESC LIMIT 50",
    ) {
        Ok(s) => s,
        Err(_) => return Json(Vec::<AlertRow>::new()),
    };

    let alerts: Vec<AlertRow> = match stmt.query_map([], |row| {
        Ok(AlertRow {
            id: row.get(0)?,
            market_condition_id: row.get(1)?,
            alert_type: row.get(2)?,
            details: row.get(3)?,
            action_taken: row.get(4)?,
            cycle_number: row.get(5)?,
            created_at: row.get(6)?,
        })
    }) {
        Ok(rows) => rows.filter_map(|r| r.ok()).collect(),
        Err(_) => Vec::new(),
    };
    Json(alerts)
}

async fn api_weather(State(state): State<AppState>) -> impl IntoResponse {
    let db = state.db.lock().unwrap();
    let snapshots = db.get_latest_weather_snapshots().unwrap_or_default();

    let resp: Vec<WeatherResponse> = snapshots
        .into_iter()
        .map(|s| {
            let buckets = serde_json::from_str(&s.bucket_data).unwrap_or(serde_json::Value::Null);
            WeatherResponse {
                city: s.city,
                forecast_date: s.forecast_date,
                ensemble_mean: s.ensemble_mean,
                ensemble_std: s.ensemble_std,
                gefs_count: s.gefs_count,
                ecmwf_count: s.ecmwf_count,
                buckets,
                cycle_number: s.cycle_number,
            }
        })
        .collect();

    Json(resp)
}

async fn api_opportunities(
    State(state): State<AppState>,
    Query(params): Query<OpportunitiesQuery>,
) -> impl IntoResponse {
    let limit = params.limit.unwrap_or(50).min(200);
    let db = state.db.lock().unwrap();
    let opps = db.get_recent_opportunities(limit).unwrap_or_default();

    let resp: Vec<OpportunityResponse> = opps
        .into_iter()
        .map(|o| OpportunityResponse {
            question: o.question,
            side: o.side,
            market_price: o.market_price,
            estimated_probability: o.estimated_probability,
            edge: o.edge,
            confidence: o.confidence,
            status: o.status,
            reject_reason: o.reject_reason,
            cycle_number: o.cycle_number,
            created_at: Some(o.created_at),
        })
        .collect();

    Json(resp)
}

async fn serve_dashboard() -> impl IntoResponse {
    Html(include_str!("../static/dashboard.html"))
}

// ─── Router & server startup ───────────────────────────

fn build_router(state: AppState, password: &str) -> Router {
    let api_routes = Router::new()
        .route("/api/status", get(api_status))
        .route("/api/positions", get(api_positions))
        .route("/api/trades", get(api_trades))
        .route("/api/history", get(api_history))
        .route("/api/alerts", get(api_alerts))
        .route("/api/weather", get(api_weather))
        .route("/api/opportunities", get(api_opportunities))
        .route("/ws", get(ws_handler).with_state(state.event_tx.clone()));

    let app = Router::new()
        .route("/", get(serve_dashboard))
        .merge(api_routes)
        .with_state(state);

    if password.is_empty() {
        app
    } else {
        app.layer(ValidateRequestHeaderLayer::basic("admin", password))
    }
}

/// Start the dashboard HTTP + WebSocket server.
/// Runs forever — call from `tokio::spawn`.
pub async fn start_dashboard(config: &Config, event_tx: EventSender) -> Result<()> {
    let db =
        Database::open(&config.database_path).context("Failed to open dashboard DB connection")?;

    let state = AppState {
        db: Arc::new(Mutex::new(db)),
        event_tx,
        trading_mode: config.trading_mode.to_string(),
    };

    let app = build_router(state, &config.dashboard_password);
    let addr = format!("0.0.0.0:{}", config.dashboard_port);
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .with_context(|| format!("Failed to bind dashboard to {}", addr))?;

    info!("Dashboard listening on http://{}", addr);
    axum::serve(listener, app)
        .await
        .context("Dashboard server error")?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    fn test_state() -> AppState {
        let db = Database::open_in_memory().unwrap();
        db.ensure_bankroll_seeded(50.0).unwrap();
        AppState {
            db: Arc::new(Mutex::new(db)),
            event_tx: crate::websocket::new_event_channel(),
            trading_mode: "paper".to_string(),
        }
    }

    fn test_router(state: AppState) -> Router {
        build_router(state, "")
    }

    #[tokio::test]
    async fn test_status_endpoint() {
        let state = test_state();
        let app = test_router(state);

        let resp = app
            .oneshot(Request::get("/api/status").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 1_000_000)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["trading_mode"], "paper");
        assert_eq!(json["bankroll"], 50.0);
        assert_eq!(json["total_trades"], 0);
    }

    #[tokio::test]
    async fn test_positions_empty() {
        let state = test_state();
        let app = test_router(state);

        let resp = app
            .oneshot(Request::get("/api/positions").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 1_000_000)
            .await
            .unwrap();
        let json: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();
        assert!(json.is_empty());
    }

    #[tokio::test]
    async fn test_trades_empty() {
        let state = test_state();
        let app = test_router(state);

        let resp = app
            .oneshot(Request::get("/api/trades").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 1_000_000)
            .await
            .unwrap();
        let json: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();
        assert!(json.is_empty());
    }

    #[tokio::test]
    async fn test_history_empty() {
        let state = test_state();
        let app = test_router(state);

        let resp = app
            .oneshot(Request::get("/api/history").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 1_000_000)
            .await
            .unwrap();
        let json: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();
        assert!(json.is_empty());
    }

    #[tokio::test]
    async fn test_alerts_empty() {
        let state = test_state();
        let app = test_router(state);

        let resp = app
            .oneshot(Request::get("/api/alerts").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 1_000_000)
            .await
            .unwrap();
        let json: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();
        assert!(json.is_empty());
    }

    #[tokio::test]
    async fn test_dashboard_html_served() {
        let state = test_state();
        let app = test_router(state);

        let resp = app
            .oneshot(Request::get("/").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 1_000_000)
            .await
            .unwrap();
        let html = String::from_utf8_lossy(&body);
        assert!(html.contains("Polymarket Agent"));
    }

    #[tokio::test]
    async fn test_status_with_data() {
        let state = test_state();
        {
            let db = state.db.lock().unwrap();
            // Insert a cycle
            db.conn
                .execute(
                    "INSERT INTO cycle_log (cycle_number, markets_scanned, markets_filtered, trades_placed, api_cost_usd, bankroll_before, bankroll_after) VALUES (1, 50, 10, 2, 0.15, 50.0, 49.85)",
                    [],
                )
                .unwrap();
            // Insert a market and trade
            db.conn
                .execute(
                    "INSERT INTO markets (condition_id, question, active) VALUES ('0xtest', 'Test?', 1)",
                    [],
                )
                .unwrap();
            db.insert_trade("t1", "0xtest", "tok1", "YES", 0.60, 5.0, "filled", true, 0.0)
                .unwrap();
        }

        let app = test_router(state);
        let resp = app
            .oneshot(Request::get("/api/status").body(Body::empty()).unwrap())
            .await
            .unwrap();

        let body = axum::body::to_bytes(resp.into_body(), 1_000_000)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["total_trades"], 1);
        assert_eq!(json["next_cycle"], 2);
    }

    #[tokio::test]
    async fn test_trades_with_limit() {
        let state = test_state();
        {
            let db = state.db.lock().unwrap();
            db.conn
                .execute(
                    "INSERT INTO markets (condition_id, question, active) VALUES ('0xtest', 'Test?', 1)",
                    [],
                )
                .unwrap();
            for i in 1..=5 {
                db.insert_trade(
                    &format!("t{}", i),
                    "0xtest",
                    "tok1",
                    "YES",
                    0.60,
                    1.0,
                    "filled",
                    true,
                    0.0,
                )
                .unwrap();
            }
        }

        let app = test_router(state);
        let resp = app
            .oneshot(
                Request::get("/api/trades?limit=3")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        let body = axum::body::to_bytes(resp.into_body(), 1_000_000)
            .await
            .unwrap();
        let json: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();
        assert_eq!(json.len(), 3);
    }
}
