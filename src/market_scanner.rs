use anyhow::{Context, Result};
use reqwest::Client;
use serde::Deserialize;
use std::time::Duration;
use tracing::{debug, info};

use crate::config::Config;

// Custom deserializer for fields that may be strings or numbers or null
fn deserialize_optional_f64<'de, D>(deserializer: D) -> Result<Option<f64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de;

    #[derive(Deserialize)]
    #[serde(untagged)]
    enum StringOrFloat {
        Float(f64),
        String(String),
        Null,
    }

    match StringOrFloat::deserialize(deserializer)? {
        StringOrFloat::Float(f) => Ok(Some(f)),
        StringOrFloat::String(s) => {
            if s.is_empty() {
                Ok(None)
            } else {
                s.parse::<f64>().map(Some).map_err(de::Error::custom)
            }
        }
        StringOrFloat::Null => Ok(None),
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct GammaMarket {
    pub id: u64,
    pub question: String,
    #[serde(default)]
    pub slug: Option<String>,
    #[serde(rename = "conditionId", default)]
    pub condition_id: Option<String>,
    #[serde(default)]
    pub tokens: Vec<Token>,
    #[serde(default, deserialize_with = "deserialize_optional_f64")]
    pub volume: Option<f64>,
    #[serde(default, deserialize_with = "deserialize_optional_f64")]
    pub liquidity: Option<f64>,
    #[serde(rename = "endDate", default)]
    pub end_date: Option<String>,
    #[serde(default)]
    pub closed: bool,
    #[serde(default)]
    pub active: bool,
    #[serde(default)]
    pub tags: Vec<Tag>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Token {
    pub token_id: String,
    pub outcome: String,
    #[serde(default, deserialize_with = "deserialize_optional_f64")]
    pub price: Option<f64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Tag {
    #[serde(default)]
    pub id: Option<u64>,
    #[serde(default)]
    pub label: Option<String>,
}

pub struct MarketScanner {
    client: Client,
    gamma_url: String,
    page_size: u32,
    max_markets: u32,
    min_liquidity: f64,
    min_volume: f64,
}

impl MarketScanner {
    pub fn new(config: &Config) -> Result<Self> {
        let client = Client::builder()
            .timeout(Duration::from_secs(config.scanner_request_timeout_secs))
            .build()
            .context("Failed to build HTTP client")?;

        Ok(MarketScanner {
            client,
            gamma_url: config.gamma_api_url.clone(),
            page_size: config.scanner_page_size,
            max_markets: config.scanner_max_markets,
            min_liquidity: config.scanner_min_liquidity,
            min_volume: config.scanner_min_volume,
        })
    }

    /// For testing: create scanner with custom client and URL
    #[cfg(test)]
    fn with_client(client: Client, base_url: String, config: &Config) -> Self {
        MarketScanner {
            client,
            gamma_url: base_url,
            page_size: config.scanner_page_size,
            max_markets: config.scanner_max_markets,
            min_liquidity: config.scanner_min_liquidity,
            min_volume: config.scanner_min_volume,
        }
    }

    pub async fn fetch_page(&self, offset: u32) -> Result<Vec<GammaMarket>> {
        let url = format!("{}/markets", self.gamma_url);

        let response = self
            .client
            .get(&url)
            .query(&[
                ("closed", "false"),
                ("limit", &self.page_size.to_string()),
                ("offset", &offset.to_string()),
            ])
            .send()
            .await
            .context("Failed to fetch markets page")?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("Gamma API returned {}: {}", status, body);
        }

        let markets: Vec<GammaMarket> = response
            .json()
            .await
            .context("Failed to parse Gamma API response")?;

        debug!("Fetched {} markets at offset {}", markets.len(), offset);
        Ok(markets)
    }

    pub async fn scan_all(&self) -> Result<Vec<GammaMarket>> {
        let mut all_markets = Vec::new();
        let mut offset = 0u32;

        loop {
            let page = self.fetch_page(offset).await?;
            let page_len = page.len() as u32;

            if page.is_empty() {
                break;
            }

            all_markets.extend(page);

            if all_markets.len() as u32 >= self.max_markets {
                info!("Reached max markets limit ({})", self.max_markets);
                all_markets.truncate(self.max_markets as usize);
                break;
            }

            if page_len < self.page_size {
                break; // Last page
            }

            offset += page_len;
        }

        info!("Scanned {} total markets", all_markets.len());
        Ok(all_markets)
    }

    pub fn filter_markets(&self, markets: Vec<GammaMarket>) -> Vec<GammaMarket> {
        let before = markets.len();
        let filtered: Vec<GammaMarket> = markets
            .into_iter()
            .filter(|m| {
                if m.closed {
                    return false;
                }
                if !m.active {
                    return false;
                }
                if m.tokens.is_empty() {
                    return false;
                }
                if m.condition_id.is_none() {
                    return false;
                }
                let liquidity = m.liquidity.unwrap_or(0.0);
                if liquidity < self.min_liquidity {
                    return false;
                }
                let volume = m.volume.unwrap_or(0.0);
                if volume < self.min_volume {
                    return false;
                }
                true
            })
            .collect();

        info!(
            "Filtered {} -> {} markets (removed {})",
            before,
            filtered.len(),
            before - filtered.len()
        );
        filtered
    }

    pub async fn scan_and_filter(&self) -> Result<Vec<GammaMarket>> {
        let markets = self.scan_all().await?;
        Ok(self.filter_markets(markets))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn test_config() -> Config {
        Config {
            trading_mode: crate::config::TradingMode::Paper,
            gamma_api_url: String::new(), // overridden in tests
            clob_api_url: String::new(),
            data_api_url: String::new(),
            sidecar_host: "127.0.0.1".to_string(),
            sidecar_port: 9090,
            sidecar_startup_timeout_secs: 30,
            sidecar_health_interval_ms: 500,
            scanner_page_size: 2, // small for testing
            scanner_max_markets: 10,
            scanner_min_liquidity: 500.0,
            scanner_min_volume: 1000.0,
            scanner_request_timeout_secs: 5,
            database_path: ":memory:".to_string(),
            anthropic_api_key: "test-key".to_string(),
            anthropic_api_url: String::new(),
            haiku_model: "claude-haiku-4-5-20251001".to_string(),
            sonnet_model: "claude-sonnet-4-5-20250929".to_string(),
            max_api_cost_per_cycle: 0.50,
            min_edge_threshold: 0.08,
            estimator_request_timeout_secs: 30,
            estimator_max_retries: 2,
            kelly_fraction: 0.5,
            max_position_pct: 0.06,
            max_total_exposure_pct: 0.40,
            initial_bankroll: 50.0,
            executor_request_timeout_secs: 15,
            cycle_frequency_high_secs: 600,
            cycle_frequency_low_secs: 1800,
            low_bankroll_threshold: 200.0,
            death_exit_code: 42,
            weather_spread_correction: 1.0,
        }
    }

    fn sample_market_json(id: u64, volume: f64, liquidity: f64, closed: bool) -> serde_json::Value {
        serde_json::json!({
            "id": id,
            "question": format!("Test market {}", id),
            "slug": format!("test-market-{}", id),
            "conditionId": format!("0xcond{}", id),
            "tokens": [
                {"token_id": format!("tok_yes_{}", id), "outcome": "Yes", "price": 0.65},
                {"token_id": format!("tok_no_{}", id), "outcome": "No", "price": 0.35}
            ],
            "volume": volume,
            "liquidity": liquidity,
            "endDate": "2026-03-01T00:00:00Z",
            "closed": closed,
            "active": !closed,
            "tags": [{"id": 100381, "label": "Weather"}]
        })
    }

    #[tokio::test]
    async fn test_fetch_single_page() {
        let server = MockServer::start().await;
        let config = test_config();
        let scanner = MarketScanner::with_client(Client::new(), server.uri(), &config);

        let body = serde_json::json!([
            sample_market_json(1, 5000.0, 2000.0, false),
            sample_market_json(2, 3000.0, 1500.0, false),
        ]);

        Mock::given(method("GET"))
            .and(path("/markets"))
            .and(query_param("closed", "false"))
            .and(query_param("limit", "2"))
            .and(query_param("offset", "0"))
            .respond_with(ResponseTemplate::new(200).set_body_json(&body))
            .mount(&server)
            .await;

        let markets = scanner.fetch_page(0).await.unwrap();
        assert_eq!(markets.len(), 2);
        assert_eq!(markets[0].id, 1);
        assert_eq!(markets[1].id, 2);
    }

    #[tokio::test]
    async fn test_pagination_stops_on_short_page() {
        let server = MockServer::start().await;
        let config = test_config();
        let scanner = MarketScanner::with_client(Client::new(), server.uri(), &config);

        // Page 1: full page (2 items)
        Mock::given(method("GET"))
            .and(path("/markets"))
            .and(query_param("offset", "0"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                sample_market_json(1, 5000.0, 2000.0, false),
                sample_market_json(2, 3000.0, 1500.0, false),
            ])))
            .mount(&server)
            .await;

        // Page 2: short page (1 item) -- stops pagination
        Mock::given(method("GET"))
            .and(path("/markets"))
            .and(query_param("offset", "2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                sample_market_json(3, 8000.0, 3000.0, false),
            ])))
            .mount(&server)
            .await;

        let markets = scanner.scan_all().await.unwrap();
        assert_eq!(markets.len(), 3);
    }

    #[tokio::test]
    async fn test_pagination_stops_on_empty_page() {
        let server = MockServer::start().await;
        let config = test_config();
        let scanner = MarketScanner::with_client(Client::new(), server.uri(), &config);

        Mock::given(method("GET"))
            .and(path("/markets"))
            .and(query_param("offset", "0"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
            .mount(&server)
            .await;

        let markets = scanner.scan_all().await.unwrap();
        assert!(markets.is_empty());
    }

    #[tokio::test]
    async fn test_filter_removes_low_liquidity() {
        let config = test_config();
        let scanner = MarketScanner::with_client(Client::new(), "http://unused".into(), &config);

        let markets: Vec<GammaMarket> = serde_json::from_value(serde_json::json!([
            sample_market_json(1, 5000.0, 2000.0, false), // passes
            sample_market_json(2, 5000.0, 100.0, false),  // fails liquidity
        ]))
        .unwrap();

        let filtered = scanner.filter_markets(markets);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].id, 1);
    }

    #[tokio::test]
    async fn test_filter_removes_closed() {
        let config = test_config();
        let scanner = MarketScanner::with_client(Client::new(), "http://unused".into(), &config);

        let markets: Vec<GammaMarket> = serde_json::from_value(serde_json::json!([
            sample_market_json(1, 5000.0, 2000.0, false),
            sample_market_json(2, 5000.0, 2000.0, true), // closed
        ]))
        .unwrap();

        let filtered = scanner.filter_markets(markets);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].id, 1);
    }

    #[tokio::test]
    async fn test_handles_null_fields() {
        // Volume/liquidity as null or string
        let market_json = serde_json::json!({
            "id": 1,
            "question": "Test?",
            "slug": "test",
            "conditionId": "0x123",
            "tokens": [{"token_id": "t1", "outcome": "Yes", "price": "0.50"}],
            "volume": null,
            "liquidity": "2500.50",
            "endDate": null,
            "closed": false,
            "active": true,
            "tags": []
        });

        let market: GammaMarket = serde_json::from_value(market_json).unwrap();
        assert!(market.volume.is_none());
        assert_eq!(market.liquidity, Some(2500.50));
        assert_eq!(market.tokens[0].price, Some(0.50));
    }

    #[tokio::test]
    async fn test_handles_missing_fields() {
        // Minimal market with most fields missing
        let market_json = serde_json::json!({
            "id": 1,
            "question": "Test?"
        });

        let market: GammaMarket = serde_json::from_value(market_json).unwrap();
        assert!(market.slug.is_none());
        assert!(market.condition_id.is_none());
        assert!(market.tokens.is_empty());
        assert!(market.volume.is_none());
        assert!(!market.closed);
    }
}
