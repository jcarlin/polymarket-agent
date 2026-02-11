use tracing::info;

/// A news alert from OpenClaw research layer.
#[derive(Debug, Clone)]
pub struct NewsAlert {
    pub market_id: String,
    pub headline: String,
    pub relevance: f64,
}

/// Stub client for OpenClaw integration.
/// Real implementation deferred to Phase 7+.
pub struct OpenClawClient;

impl Default for OpenClawClient {
    fn default() -> Self {
        Self::new()
    }
}

impl OpenClawClient {
    pub fn new() -> Self {
        info!("OpenClaw client initialized (stub — no real API calls)");
        OpenClawClient
    }

    /// Check for breaking news affecting the given markets.
    /// Returns empty vec — stub implementation.
    pub fn check_news_alerts(&self, _market_ids: &[String]) -> Vec<NewsAlert> {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_stub_returns_empty() {
        let client = OpenClawClient::new();
        let alerts = client.check_news_alerts(&["0xabc".to_string()]);
        assert!(alerts.is_empty());
    }
}
