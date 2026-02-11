use anyhow::{Context, Result};
use std::env;
use std::str::FromStr;

#[derive(Debug, Clone, PartialEq)]
pub enum TradingMode {
    Paper,
    Live,
}

impl FromStr for TradingMode {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self> {
        match s.to_lowercase().as_str() {
            "paper" => Ok(TradingMode::Paper),
            "live" => Ok(TradingMode::Live),
            _ => anyhow::bail!("Invalid trading mode: '{}'. Must be 'paper' or 'live'", s),
        }
    }
}

impl std::fmt::Display for TradingMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TradingMode::Paper => write!(f, "paper"),
            TradingMode::Live => write!(f, "live"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Config {
    pub trading_mode: TradingMode,
    // API URLs
    pub gamma_api_url: String,
    pub clob_api_url: String,
    pub data_api_url: String,
    // Sidecar
    pub sidecar_host: String,
    pub sidecar_port: u16,
    pub sidecar_startup_timeout_secs: u64,
    pub sidecar_health_interval_ms: u64,
    // Scanner
    pub scanner_page_size: u32,
    pub scanner_max_markets: u32,
    pub scanner_min_liquidity: f64,
    pub scanner_min_volume: f64,
    pub scanner_request_timeout_secs: u64,
    // Database
    pub database_path: String,
    // Claude API
    pub anthropic_api_key: String,
    pub anthropic_api_url: String,
    pub haiku_model: String,
    pub sonnet_model: String,
    pub max_api_cost_per_cycle: f64,
    pub min_edge_threshold: f64,
    pub estimator_request_timeout_secs: u64,
    pub estimator_max_retries: u32,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        dotenvy::dotenv().ok(); // Don't fail if .env missing

        Ok(Config {
            trading_mode: env::var("TRADING_MODE")
                .unwrap_or_else(|_| "paper".to_string())
                .parse()
                .context("Failed to parse TRADING_MODE")?,
            gamma_api_url: env::var("GAMMA_API_URL")
                .unwrap_or_else(|_| "https://gamma-api.polymarket.com".to_string()),
            clob_api_url: env::var("CLOB_API_URL")
                .unwrap_or_else(|_| "https://clob.polymarket.com".to_string()),
            data_api_url: env::var("DATA_API_URL")
                .unwrap_or_else(|_| "https://data-api.polymarket.com".to_string()),
            sidecar_host: env::var("SIDECAR_HOST").unwrap_or_else(|_| "127.0.0.1".to_string()),
            sidecar_port: env::var("SIDECAR_PORT")
                .unwrap_or_else(|_| "9090".to_string())
                .parse()
                .context("Failed to parse SIDECAR_PORT")?,
            sidecar_startup_timeout_secs: env::var("SIDECAR_STARTUP_TIMEOUT_SECS")
                .unwrap_or_else(|_| "30".to_string())
                .parse()
                .context("Failed to parse SIDECAR_STARTUP_TIMEOUT_SECS")?,
            sidecar_health_interval_ms: env::var("SIDECAR_HEALTH_INTERVAL_MS")
                .unwrap_or_else(|_| "500".to_string())
                .parse()
                .context("Failed to parse SIDECAR_HEALTH_INTERVAL_MS")?,
            scanner_page_size: env::var("SCANNER_PAGE_SIZE")
                .unwrap_or_else(|_| "50".to_string())
                .parse()
                .context("Failed to parse SCANNER_PAGE_SIZE")?,
            scanner_max_markets: env::var("SCANNER_MAX_MARKETS")
                .unwrap_or_else(|_| "500".to_string())
                .parse()
                .context("Failed to parse SCANNER_MAX_MARKETS")?,
            scanner_min_liquidity: env::var("SCANNER_MIN_LIQUIDITY")
                .unwrap_or_else(|_| "500.0".to_string())
                .parse()
                .context("Failed to parse SCANNER_MIN_LIQUIDITY")?,
            scanner_min_volume: env::var("SCANNER_MIN_VOLUME")
                .unwrap_or_else(|_| "1000.0".to_string())
                .parse()
                .context("Failed to parse SCANNER_MIN_VOLUME")?,
            scanner_request_timeout_secs: env::var("SCANNER_REQUEST_TIMEOUT_SECS")
                .unwrap_or_else(|_| "15".to_string())
                .parse()
                .context("Failed to parse SCANNER_REQUEST_TIMEOUT_SECS")?,
            database_path: env::var("DATABASE_PATH")
                .unwrap_or_else(|_| "data/polymarket-agent.db".to_string()),
            anthropic_api_key: env::var("ANTHROPIC_API_KEY").unwrap_or_default(),
            anthropic_api_url: env::var("ANTHROPIC_API_URL")
                .unwrap_or_else(|_| "https://api.anthropic.com".to_string()),
            haiku_model: env::var("HAIKU_MODEL")
                .unwrap_or_else(|_| "claude-haiku-4-5-20251001".to_string()),
            sonnet_model: env::var("SONNET_MODEL")
                .unwrap_or_else(|_| "claude-sonnet-4-5-20250929".to_string()),
            max_api_cost_per_cycle: env::var("MAX_API_COST_PER_CYCLE")
                .unwrap_or_else(|_| "0.50".to_string())
                .parse()
                .context("Failed to parse MAX_API_COST_PER_CYCLE")?,
            min_edge_threshold: env::var("MIN_EDGE_THRESHOLD")
                .unwrap_or_else(|_| "0.08".to_string())
                .parse()
                .context("Failed to parse MIN_EDGE_THRESHOLD")?,
            estimator_request_timeout_secs: env::var("ESTIMATOR_REQUEST_TIMEOUT_SECS")
                .unwrap_or_else(|_| "30".to_string())
                .parse()
                .context("Failed to parse ESTIMATOR_REQUEST_TIMEOUT_SECS")?,
            estimator_max_retries: env::var("ESTIMATOR_MAX_RETRIES")
                .unwrap_or_else(|_| "2".to_string())
                .parse()
                .context("Failed to parse ESTIMATOR_MAX_RETRIES")?,
        })
    }

    pub fn sidecar_url(&self) -> String {
        format!("http://{}:{}", self.sidecar_host, self.sidecar_port)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_defaults_applied() {
        // Clear any env vars that might interfere
        // from_env should succeed with defaults
        let config = Config::from_env().unwrap();
        assert_eq!(config.trading_mode, TradingMode::Paper);
        assert_eq!(config.gamma_api_url, "https://gamma-api.polymarket.com");
        assert_eq!(config.sidecar_port, 9090);
        assert_eq!(config.scanner_page_size, 50);
        assert_eq!(config.scanner_min_liquidity, 500.0);
        assert_eq!(config.database_path, "data/polymarket-agent.db");
        assert_eq!(config.anthropic_api_url, "https://api.anthropic.com");
        assert_eq!(config.haiku_model, "claude-haiku-4-5-20251001");
        assert_eq!(config.sonnet_model, "claude-sonnet-4-5-20250929");
        assert_eq!(config.max_api_cost_per_cycle, 0.50);
        assert_eq!(config.min_edge_threshold, 0.08);
        assert_eq!(config.estimator_request_timeout_secs, 30);
        assert_eq!(config.estimator_max_retries, 2);
    }

    #[test]
    fn test_sidecar_url() {
        let config = Config::from_env().unwrap();
        assert_eq!(config.sidecar_url(), "http://127.0.0.1:9090");
    }

    #[test]
    fn test_trading_mode_parsing() {
        assert_eq!("paper".parse::<TradingMode>().unwrap(), TradingMode::Paper);
        assert_eq!("live".parse::<TradingMode>().unwrap(), TradingMode::Live);
        assert_eq!("Paper".parse::<TradingMode>().unwrap(), TradingMode::Paper);
        assert_eq!("LIVE".parse::<TradingMode>().unwrap(), TradingMode::Live);
        assert!("invalid".parse::<TradingMode>().is_err());
    }

    #[test]
    fn test_trading_mode_display() {
        assert_eq!(TradingMode::Paper.to_string(), "paper");
        assert_eq!(TradingMode::Live.to_string(), "live");
    }
}
