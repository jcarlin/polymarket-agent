use anyhow::{Context, Result};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tracing::info;

use crate::config::TradingMode;
use crate::db::Database;
use crate::edge_detector::{EdgeOpportunity, TradeSide};
use crate::position_sizer::SizingResult;

#[derive(Debug, Clone)]
pub struct TradeIntent {
    pub opportunity: EdgeOpportunity,
    pub token_id: String,
    pub sizing: SizingResult,
}

#[derive(Debug, Clone)]
pub struct TradeResult {
    pub trade_id: String,
    pub market_condition_id: String,
    pub token_id: String,
    pub side: TradeSide,
    pub price: f64,
    pub size: f64,
    pub status: String,
    pub paper: bool,
}

#[derive(Debug, Serialize)]
struct SidecarOrderRequest {
    token_id: String,
    price: f64,
    size: f64,
    side: String,
}

#[derive(Debug, Deserialize)]
struct SidecarOrderResponse {
    order_id: String,
    status: String,
}

pub struct Executor {
    client: Client,
    sidecar_url: String,
    trading_mode: TradingMode,
}

impl Executor {
    pub fn new(sidecar_url: &str, trading_mode: TradingMode, timeout_secs: u64) -> Result<Self> {
        let client = Client::builder()
            .timeout(Duration::from_secs(timeout_secs))
            .build()
            .context("Failed to build Executor HTTP client")?;

        Ok(Executor {
            client,
            sidecar_url: sidecar_url.trim_end_matches('/').to_string(),
            trading_mode,
        })
    }

    #[cfg(test)]
    fn with_client(client: Client, sidecar_url: String, trading_mode: TradingMode) -> Self {
        Executor {
            client,
            sidecar_url,
            trading_mode,
        }
    }

    pub async fn execute(&self, intent: &TradeIntent, db: &Database) -> Result<TradeResult> {
        match self.trading_mode {
            TradingMode::Paper => self.execute_paper(intent, db),
            TradingMode::Live => self.execute_live(intent, db).await,
        }
    }

    fn execute_paper(&self, intent: &TradeIntent, db: &Database) -> Result<TradeResult> {
        let trade_id = uuid::Uuid::new_v4().to_string();
        let side_str = intent.opportunity.side.to_string();

        // Log trade
        db.insert_trade(
            &trade_id,
            &intent.opportunity.market_id,
            &intent.token_id,
            &side_str,
            intent.sizing.limit_price,
            intent.sizing.shares,
            "filled",
            true,
        )?;

        // Update position
        db.upsert_position(
            &intent.opportunity.market_id,
            &intent.token_id,
            &side_str,
            intent.sizing.limit_price,
            intent.sizing.shares,
        )?;

        // Update bankroll (deduct the cost of shares)
        let current_bankroll = db.get_current_bankroll()?;
        let new_bankroll = current_bankroll - intent.sizing.position_usd;
        db.log_bankroll_entry(
            "trade",
            -intent.sizing.position_usd,
            new_bankroll,
            &format!(
                "Paper {} {} @ {:.2} ({:.1} shares)",
                side_str,
                intent.opportunity.question,
                intent.sizing.limit_price,
                intent.sizing.shares,
            ),
        )?;

        info!(
            "PAPER TRADE: {} {} @ {:.2} ({:.1} shares, ${:.2})",
            side_str,
            intent.opportunity.question,
            intent.sizing.limit_price,
            intent.sizing.shares,
            intent.sizing.position_usd,
        );

        Ok(TradeResult {
            trade_id,
            market_condition_id: intent.opportunity.market_id.clone(),
            token_id: intent.token_id.clone(),
            side: intent.opportunity.side,
            price: intent.sizing.limit_price,
            size: intent.sizing.shares,
            status: "filled".to_string(),
            paper: true,
        })
    }

    async fn execute_live(&self, intent: &TradeIntent, db: &Database) -> Result<TradeResult> {
        let side_str = intent.opportunity.side.to_string();

        let request = SidecarOrderRequest {
            token_id: intent.token_id.clone(),
            price: intent.sizing.limit_price,
            size: intent.sizing.shares,
            side: side_str.clone(),
        };

        let response = self
            .client
            .post(format!("{}/order", self.sidecar_url))
            .json(&request)
            .send()
            .await
            .context("Failed to send order to sidecar")?;

        let status_code = response.status();
        if !status_code.is_success() {
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("Sidecar order failed ({}): {}", status_code, body);
        }

        let order_resp: SidecarOrderResponse = response
            .json()
            .await
            .context("Failed to parse sidecar order response")?;

        // Log trade
        db.insert_trade(
            &order_resp.order_id,
            &intent.opportunity.market_id,
            &intent.token_id,
            &side_str,
            intent.sizing.limit_price,
            intent.sizing.shares,
            &order_resp.status,
            false,
        )?;

        // Update position
        db.upsert_position(
            &intent.opportunity.market_id,
            &intent.token_id,
            &side_str,
            intent.sizing.limit_price,
            intent.sizing.shares,
        )?;

        // Update bankroll
        let current_bankroll = db.get_current_bankroll()?;
        let new_bankroll = current_bankroll - intent.sizing.position_usd;
        db.log_bankroll_entry(
            "trade",
            -intent.sizing.position_usd,
            new_bankroll,
            &format!(
                "Live {} {} @ {:.2} ({:.1} shares)",
                side_str,
                intent.opportunity.question,
                intent.sizing.limit_price,
                intent.sizing.shares,
            ),
        )?;

        info!(
            "LIVE TRADE: {} {} @ {:.2} ({:.1} shares, ${:.2}) order_id={}",
            side_str,
            intent.opportunity.question,
            intent.sizing.limit_price,
            intent.sizing.shares,
            intent.sizing.position_usd,
            order_resp.order_id,
        );

        Ok(TradeResult {
            trade_id: order_resp.order_id,
            market_condition_id: intent.opportunity.market_id.clone(),
            token_id: intent.token_id.clone(),
            side: intent.opportunity.side,
            price: intent.sizing.limit_price,
            size: intent.sizing.shares,
            status: order_resp.status,
            paper: false,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::edge_detector::EdgeOpportunity;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn make_intent(side: TradeSide, market_id: &str) -> TradeIntent {
        TradeIntent {
            opportunity: EdgeOpportunity {
                market_id: market_id.to_string(),
                question: "Test market?".to_string(),
                side,
                estimated_probability: 0.75,
                market_price: 0.55,
                edge: 0.20,
                confidence: 0.85,
                data_quality: "high".to_string(),
                reasoning: "Test".to_string(),
                analysis_cost: 0.01,
            },
            token_id: "tok_yes_1".to_string(),
            sizing: SizingResult {
                raw_kelly: 0.4444,
                adjusted_kelly: 0.2222,
                position_usd: 3.0,
                shares: 5.45,
                limit_price: 0.55,
                reject_reason: None,
            },
        }
    }

    fn setup_test_db(market_id: &str) -> Database {
        let db = Database::open_in_memory().unwrap();
        // Insert market to satisfy FK constraint
        db.conn
            .execute(
                "INSERT INTO markets (condition_id, question, active) VALUES (?1, ?2, 1)",
                rusqlite::params![market_id, "Test market?"],
            )
            .unwrap();
        // Seed bankroll
        db.ensure_bankroll_seeded(50.0).unwrap();
        db
    }

    #[tokio::test]
    async fn test_paper_mode_logs_trade() {
        let db = setup_test_db("0xpaper1");
        let executor = Executor::with_client(
            Client::new(),
            "http://unused".to_string(),
            TradingMode::Paper,
        );

        let intent = make_intent(TradeSide::Yes, "0xpaper1");
        let result = executor.execute(&intent, &db).await.unwrap();

        assert!(result.paper);
        assert_eq!(result.status, "filled");
        assert_eq!(result.market_condition_id, "0xpaper1");
        assert!((result.price - 0.55).abs() < f64::EPSILON);
    }

    #[tokio::test]
    async fn test_paper_mode_updates_position() {
        let db = setup_test_db("0xpaper2");
        let executor = Executor::with_client(
            Client::new(),
            "http://unused".to_string(),
            TradingMode::Paper,
        );

        let intent = make_intent(TradeSide::Yes, "0xpaper2");
        executor.execute(&intent, &db).await.unwrap();

        let positions = db.get_open_positions().unwrap();
        assert_eq!(positions.len(), 1);
        assert!((positions[0].size - 5.45).abs() < 0.01);
    }

    #[tokio::test]
    async fn test_paper_mode_updates_bankroll() {
        let db = setup_test_db("0xpaper3");
        let executor = Executor::with_client(
            Client::new(),
            "http://unused".to_string(),
            TradingMode::Paper,
        );

        let intent = make_intent(TradeSide::Yes, "0xpaper3");
        executor.execute(&intent, &db).await.unwrap();

        let bankroll = db.get_current_bankroll().unwrap();
        // 50.0 - 3.0 = 47.0
        assert!((bankroll - 47.0).abs() < 0.01);
    }

    #[tokio::test]
    async fn test_live_mode_calls_sidecar() {
        let server = MockServer::start().await;
        let db = setup_test_db("0xlive1");

        Mock::given(method("POST"))
            .and(path("/order"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "order_id": "sidecar-order-123",
                "status": "live"
            })))
            .mount(&server)
            .await;

        let executor = Executor::with_client(Client::new(), server.uri(), TradingMode::Live);

        let intent = make_intent(TradeSide::Yes, "0xlive1");
        let result = executor.execute(&intent, &db).await.unwrap();

        assert!(!result.paper);
        assert_eq!(result.trade_id, "sidecar-order-123");
        assert_eq!(result.status, "live");
    }

    #[tokio::test]
    async fn test_live_mode_logs_to_db() {
        let server = MockServer::start().await;
        let db = setup_test_db("0xlive2");

        Mock::given(method("POST"))
            .and(path("/order"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "order_id": "order-456",
                "status": "live"
            })))
            .mount(&server)
            .await;

        let executor = Executor::with_client(Client::new(), server.uri(), TradingMode::Live);

        let intent = make_intent(TradeSide::Yes, "0xlive2");
        executor.execute(&intent, &db).await.unwrap();

        let positions = db.get_open_positions().unwrap();
        assert_eq!(positions.len(), 1);

        let bankroll = db.get_current_bankroll().unwrap();
        assert!((bankroll - 47.0).abs() < 0.01);
    }

    #[tokio::test]
    async fn test_sidecar_error_handling() {
        let server = MockServer::start().await;
        let db = setup_test_db("0xerr1");

        Mock::given(method("POST"))
            .and(path("/order"))
            .respond_with(ResponseTemplate::new(503).set_body_string("Service Unavailable"))
            .mount(&server)
            .await;

        let executor = Executor::with_client(Client::new(), server.uri(), TradingMode::Live);

        let intent = make_intent(TradeSide::Yes, "0xerr1");
        let result = executor.execute(&intent, &db).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("503"));
    }

    #[tokio::test]
    async fn test_sidecar_timeout() {
        let server = MockServer::start().await;
        let db = setup_test_db("0xtimeout1");

        // Respond with a delay longer than the client timeout
        Mock::given(method("POST"))
            .and(path("/order"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"order_id": "x", "status": "live"}))
                    .set_delay(std::time::Duration::from_secs(5)),
            )
            .mount(&server)
            .await;

        // Build client with very short timeout
        let client = Client::builder()
            .timeout(Duration::from_millis(100))
            .build()
            .unwrap();
        let executor = Executor::with_client(client, server.uri(), TradingMode::Live);

        let intent = make_intent(TradeSide::Yes, "0xtimeout1");
        let result = executor.execute(&intent, &db).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_paper_no_side_trade() {
        let db = setup_test_db("0xno1");
        let executor = Executor::with_client(
            Client::new(),
            "http://unused".to_string(),
            TradingMode::Paper,
        );

        let mut intent = make_intent(TradeSide::No, "0xno1");
        intent.sizing.limit_price = 0.45;
        intent.token_id = "tok_no_1".to_string();
        let result = executor.execute(&intent, &db).await.unwrap();

        assert_eq!(result.side, TradeSide::No);
        assert!((result.price - 0.45).abs() < f64::EPSILON);
    }
}
