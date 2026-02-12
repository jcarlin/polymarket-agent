use anyhow::{Context, Result};
use reqwest::Client;
use serde::Deserialize;
use std::time::Duration;
use tracing::{debug, warn};

/// Response from GET /midpoint?token_id=<id>
#[derive(Debug, Clone, Deserialize)]
pub struct MidpointResponse {
    pub mid: String, // CLOB returns prices as strings
}

/// Response from GET /price?token_id=<id>&side=BUY|SELL
#[derive(Debug, Clone, Deserialize)]
pub struct PriceResponse {
    pub price: String,
}

/// A single level in the orderbook
#[derive(Debug, Clone, Deserialize)]
pub struct OrderLevel {
    pub price: String,
    pub size: String,
}

/// Response from GET /book?token_id=<id>
#[derive(Debug, Clone, Deserialize)]
pub struct OrderBook {
    pub bids: Vec<OrderLevel>,
    pub asks: Vec<OrderLevel>,
}

/// Enriched market price data fetched from CLOB
#[derive(Debug, Clone)]
pub struct MarketPrices {
    pub token_id: String,
    pub outcome: String,
    pub midpoint: f64,
    pub best_bid: Option<f64>,
    pub best_ask: Option<f64>,
    pub spread: Option<f64>,
}

pub struct ClobClient {
    client: Client,
    base_url: String,
    max_retries: u32,
}

impl ClobClient {
    pub fn new(clob_api_url: &str, timeout_secs: u64) -> Result<Self> {
        let client = Client::builder()
            .timeout(Duration::from_secs(timeout_secs))
            .build()
            .context("Failed to build CLOB HTTP client")?;
        Ok(ClobClient {
            client,
            base_url: clob_api_url.trim_end_matches('/').to_string(),
            max_retries: 2,
        })
    }

    #[cfg(test)]
    fn with_client(client: Client, base_url: String) -> Self {
        ClobClient {
            client,
            base_url,
            max_retries: 2,
        }
    }

    /// Fetch midpoint price for a token
    pub async fn get_midpoint(&self, token_id: &str) -> Result<f64> {
        let url = format!("{}/midpoint?token_id={}", self.base_url, token_id);
        let resp: MidpointResponse = self.get_with_retry(&url).await?;
        resp.mid
            .parse::<f64>()
            .context("Failed to parse midpoint price")
    }

    /// Fetch best bid or ask price
    pub async fn get_price(&self, token_id: &str, side: &str) -> Result<f64> {
        let url = format!(
            "{}/price?token_id={}&side={}",
            self.base_url, token_id, side
        );
        let resp: PriceResponse = self.get_with_retry(&url).await?;
        resp.price.parse::<f64>().context("Failed to parse price")
    }

    /// Fetch full orderbook
    pub async fn get_orderbook(&self, token_id: &str) -> Result<OrderBook> {
        let url = format!("{}/book?token_id={}", self.base_url, token_id);
        self.get_with_retry(&url).await
    }

    /// Fetch all price data for a token (midpoint + bid + ask).
    /// Graceful degradation: if bid/ask fail, uses midpoint only.
    pub async fn get_market_prices(&self, token_id: &str, outcome: &str) -> Result<MarketPrices> {
        // Fetch midpoint, bid, and ask concurrently (3 independent calls)
        let (mid_result, bid_result, ask_result) = tokio::join!(
            self.get_midpoint(token_id),
            self.get_price(token_id, "BUY"),
            self.get_price(token_id, "SELL"),
        );
        let midpoint = mid_result?;
        let bid = bid_result.ok();
        let ask = ask_result.ok();
        let spread = match (bid, ask) {
            (Some(b), Some(a)) => Some(a - b),
            _ => None,
        };

        Ok(MarketPrices {
            token_id: token_id.to_string(),
            outcome: outcome.to_string(),
            midpoint,
            best_bid: bid,
            best_ask: ask,
            spread,
        })
    }

    /// Retry wrapper for HTTP GETs with exponential backoff
    async fn get_with_retry<T: serde::de::DeserializeOwned>(&self, url: &str) -> Result<T> {
        let mut last_err = None;
        for attempt in 0..=self.max_retries {
            if attempt > 0 {
                let delay = Duration::from_millis(500 * 2u64.pow(attempt - 1));
                debug!(
                    "Retrying {} after {:?} (attempt {})",
                    url,
                    delay,
                    attempt + 1
                );
                tokio::time::sleep(delay).await;
            }

            match self.client.get(url).send().await {
                Ok(resp) => {
                    let status = resp.status();
                    if status.is_success() {
                        return resp
                            .json::<T>()
                            .await
                            .context("Failed to parse CLOB response");
                    }
                    if status.as_u16() == 429 || status.is_server_error() {
                        let body = resp.text().await.unwrap_or_default();
                        warn!("CLOB API {} returned {}: {}", url, status, body);
                        last_err = Some(anyhow::anyhow!("CLOB API returned {}: {}", status, body));
                        continue;
                    }
                    // Client error (4xx except 429) -- don't retry
                    let body = resp.text().await.unwrap_or_default();
                    anyhow::bail!("CLOB API returned {}: {}", status, body);
                }
                Err(e) => {
                    warn!("CLOB API request failed: {}", e);
                    last_err = Some(e.into());
                }
            }
        }
        Err(last_err.unwrap_or_else(|| anyhow::anyhow!("CLOB API request failed after retries")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn test_get_midpoint_success() {
        let server = MockServer::start().await;
        let client = ClobClient::with_client(Client::new(), server.uri());

        Mock::given(method("GET"))
            .and(path("/midpoint"))
            .and(query_param("token_id", "tok_abc"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"mid": "0.65"})),
            )
            .mount(&server)
            .await;

        let mid = client.get_midpoint("tok_abc").await.unwrap();
        assert!((mid - 0.65).abs() < f64::EPSILON);
    }

    #[tokio::test]
    async fn test_get_midpoint_string_parsing() {
        let server = MockServer::start().await;
        let client = ClobClient::with_client(Client::new(), server.uri());

        // Test various string formats the CLOB API might return
        let cases = vec![
            ("tok_1", "0.5", 0.5),
            ("tok_2", "0.99", 0.99),
            ("tok_3", "0.001", 0.001),
            ("tok_4", "1.0", 1.0),
            ("tok_5", "0", 0.0),
        ];

        for (token_id, mid_str, expected) in cases {
            Mock::given(method("GET"))
                .and(path("/midpoint"))
                .and(query_param("token_id", token_id))
                .respond_with(
                    ResponseTemplate::new(200).set_body_json(serde_json::json!({"mid": mid_str})),
                )
                .mount(&server)
                .await;

            let mid = client.get_midpoint(token_id).await.unwrap();
            assert!(
                (mid - expected).abs() < f64::EPSILON,
                "Expected {} for mid='{}', got {}",
                expected,
                mid_str,
                mid
            );
        }
    }

    #[tokio::test]
    async fn test_get_price_buy_sell() {
        let server = MockServer::start().await;
        let client = ClobClient::with_client(Client::new(), server.uri());

        Mock::given(method("GET"))
            .and(path("/price"))
            .and(query_param("token_id", "tok_xyz"))
            .and(query_param("side", "BUY"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"price": "0.63"})),
            )
            .mount(&server)
            .await;

        Mock::given(method("GET"))
            .and(path("/price"))
            .and(query_param("token_id", "tok_xyz"))
            .and(query_param("side", "SELL"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"price": "0.67"})),
            )
            .mount(&server)
            .await;

        let buy = client.get_price("tok_xyz", "BUY").await.unwrap();
        assert!((buy - 0.63).abs() < f64::EPSILON);

        let sell = client.get_price("tok_xyz", "SELL").await.unwrap();
        assert!((sell - 0.67).abs() < f64::EPSILON);
    }

    #[tokio::test]
    async fn test_get_orderbook() {
        let server = MockServer::start().await;
        let client = ClobClient::with_client(Client::new(), server.uri());

        let book_json = serde_json::json!({
            "bids": [
                {"price": "0.63", "size": "100"},
                {"price": "0.62", "size": "250"}
            ],
            "asks": [
                {"price": "0.67", "size": "150"},
                {"price": "0.68", "size": "200"}
            ]
        });

        Mock::given(method("GET"))
            .and(path("/book"))
            .and(query_param("token_id", "tok_book"))
            .respond_with(ResponseTemplate::new(200).set_body_json(book_json))
            .mount(&server)
            .await;

        let book = client.get_orderbook("tok_book").await.unwrap();
        assert_eq!(book.bids.len(), 2);
        assert_eq!(book.asks.len(), 2);
        assert_eq!(book.bids[0].price, "0.63");
        assert_eq!(book.bids[0].size, "100");
        assert_eq!(book.asks[0].price, "0.67");
        assert_eq!(book.asks[1].size, "200");
    }

    #[tokio::test]
    async fn test_get_market_prices_combines_all() {
        let server = MockServer::start().await;
        let client = ClobClient::with_client(Client::new(), server.uri());

        let token_id = "tok_full";

        Mock::given(method("GET"))
            .and(path("/midpoint"))
            .and(query_param("token_id", token_id))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"mid": "0.65"})),
            )
            .mount(&server)
            .await;

        Mock::given(method("GET"))
            .and(path("/price"))
            .and(query_param("token_id", token_id))
            .and(query_param("side", "BUY"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"price": "0.63"})),
            )
            .mount(&server)
            .await;

        Mock::given(method("GET"))
            .and(path("/price"))
            .and(query_param("token_id", token_id))
            .and(query_param("side", "SELL"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"price": "0.67"})),
            )
            .mount(&server)
            .await;

        let prices = client.get_market_prices(token_id, "Yes").await.unwrap();
        assert_eq!(prices.token_id, "tok_full");
        assert_eq!(prices.outcome, "Yes");
        assert!((prices.midpoint - 0.65).abs() < f64::EPSILON);
        assert!((prices.best_bid.unwrap() - 0.63).abs() < f64::EPSILON);
        assert!((prices.best_ask.unwrap() - 0.67).abs() < f64::EPSILON);
        // spread = ask - bid = 0.67 - 0.63 = 0.04
        assert!((prices.spread.unwrap() - 0.04).abs() < f64::EPSILON);
    }

    #[tokio::test]
    async fn test_retry_on_server_error() {
        let server = MockServer::start().await;
        // Reduce retries to 1 to keep the test fast
        let mut client = ClobClient::with_client(Client::new(), server.uri());
        client.max_retries = 1;

        // Mount success mock first (lower priority in LIFO order)
        Mock::given(method("GET"))
            .and(path("/midpoint"))
            .and(query_param("token_id", "tok_retry"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"mid": "0.72"})),
            )
            .mount(&server)
            .await;

        // Mount error mock last (higher priority in LIFO). It fires at most once,
        // then falls through to the success mock on the retry.
        Mock::given(method("GET"))
            .and(path("/midpoint"))
            .and(query_param("token_id", "tok_retry"))
            .respond_with(ResponseTemplate::new(500).set_body_string("Internal Server Error"))
            .up_to_n_times(1)
            .mount(&server)
            .await;

        let mid = client.get_midpoint("tok_retry").await.unwrap();
        assert!((mid - 0.72).abs() < f64::EPSILON);
    }
}
