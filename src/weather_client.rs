use anyhow::{Context, Result};
use chrono::{Datelike, Utc};
use regex::Regex;
use reqwest::Client;
use serde::Deserialize;
use std::time::Duration;
use tracing::warn;

/// City name patterns mapped to internal city codes.
/// Used by `parse_weather_market()` to identify cities in question text.
pub const CITY_PATTERNS: &[(&str, &str)] = &[
    ("New York", "NYC"),
    ("NYC", "NYC"),
    ("Los Angeles", "LAX"),
    ("Chicago", "CHI"),
    ("Houston", "HOU"),
    ("Phoenix", "PHX"),
    ("Philadelphia", "PHL"),
    ("San Antonio", "SAN"),
    ("San Diego", "SDG"),
    ("Dallas", "DAL"),
    ("San Jose", "SJC"),
    ("Atlanta", "ATL"),
    ("Miami", "MIA"),
    ("Boston", "BOS"),
    ("Seattle", "SEA"),
    ("Denver", "DEN"),
    ("Washington", "DCA"),
    ("Minneapolis", "MSP"),
    ("Detroit", "DTW"),
    ("Tampa", "TPA"),
    ("St. Louis", "STL"),
    ("St Louis", "STL"),
];

/// Map internal city code to Gamma API event slug fragment.
/// E.g. "NYC" → "nyc", "LAX" → "los-angeles"
pub fn city_slug(code: &str) -> Option<&'static str> {
    match code {
        "NYC" => Some("nyc"),
        "LAX" => Some("los-angeles"),
        "CHI" => Some("chicago"),
        "HOU" => Some("houston"),
        "PHX" => Some("phoenix"),
        "PHL" => Some("philadelphia"),
        "SAN" => Some("san-antonio"),
        "SDG" => Some("san-diego"),
        "DAL" => Some("dallas"),
        "SJC" => Some("san-jose"),
        "ATL" => Some("atlanta"),
        "MIA" => Some("miami"),
        "BOS" => Some("boston"),
        "SEA" => Some("seattle"),
        "DEN" => Some("denver"),
        "DCA" => Some("washington"),
        "MSP" => Some("minneapolis"),
        "DTW" => Some("detroit"),
        "TPA" => Some("tampa"),
        "STL" => Some("st-louis"),
        _ => None,
    }
}

/// All unique city codes (deduplicated from CITY_PATTERNS).
pub const WEATHER_CITY_CODES: &[&str] = &[
    "NYC", "LAX", "CHI", "HOU", "PHX", "PHL", "SAN", "SDG", "DAL", "SJC", "ATL", "MIA", "BOS",
    "SEA", "DEN", "DCA", "MSP", "DTW", "TPA", "STL",
];

/// Probability for a single temperature bucket
#[derive(Debug, Clone, Deserialize)]
pub struct BucketProbability {
    pub bucket_label: String,
    pub lower: f64,
    pub upper: f64,
    pub probability: f64,
}

/// Full weather probability response from sidecar
#[derive(Debug, Clone, Default, Deserialize)]
pub struct WeatherProbabilities {
    pub city: String,
    pub station_icao: String,
    pub forecast_date: String,
    pub buckets: Vec<BucketProbability>,
    pub ensemble_mean: f64,
    pub ensemble_std: f64,
    pub gefs_count: u32,
    pub ecmwf_count: u32,
    #[serde(default)]
    pub nws_forecast_high: Option<f64>,
    #[serde(default)]
    pub bias_correction: f64,
    #[serde(default)]
    pub raw_ensemble_mean: f64,
    #[serde(default)]
    pub icon_count: u32,
    #[serde(default)]
    pub gem_count: u32,
    #[serde(default)]
    pub total_members: u32,
    #[serde(default)]
    pub hrrr_max_temp: Option<f64>,
    #[serde(default)]
    pub hrrr_shift: f64,
    #[serde(default)]
    pub nbm_max_temp: Option<f64>,
    #[serde(default)]
    pub calibration_bias: Option<f64>,
    #[serde(default)]
    pub calibration_spread: Option<f64>,
    #[serde(default)]
    pub wu_high: Option<f64>,
    #[serde(default)]
    pub wu_forecast_high: Option<f64>,
    #[serde(default)]
    pub wu_forecast_shift: f64,
}

/// Parsed weather market info from Polymarket question text
#[derive(Debug, Clone)]
pub struct WeatherMarketInfo {
    pub city: String,
    pub date: String,
    pub bucket_label: String,
    pub bucket_lower: f64,
    pub bucket_upper: f64,
}

pub struct WeatherClient {
    client: Client,
    base_url: String,
    max_retries: u32,
}

impl WeatherClient {
    pub fn new(base_url: &str, timeout_secs: u64, max_retries: u32) -> Result<Self> {
        let client = Client::builder()
            .timeout(Duration::from_secs(timeout_secs))
            .build()
            .context("Failed to build WeatherClient HTTP client")?;

        Ok(WeatherClient {
            client,
            base_url: base_url.trim_end_matches('/').to_string(),
            max_retries,
        })
    }

    /// Fetch weather probabilities for a single city/date
    pub async fn get_probabilities(
        &self,
        city: &str,
        date: &str,
        same_day: bool,
    ) -> Result<WeatherProbabilities> {
        let mut url = format!(
            "{}/weather/probabilities?city={}&date={}",
            self.base_url, city, date
        );
        if same_day {
            url.push_str("&same_day=true");
        }

        let mut last_err = None;
        for attempt in 0..=self.max_retries {
            if attempt > 0 {
                let delay = Duration::from_millis(1000 * 2u64.pow(attempt - 1));
                warn!(
                    "Retrying weather API after {:?} (attempt {})",
                    delay,
                    attempt + 1
                );
                tokio::time::sleep(delay).await;
            }

            match self.client.get(&url).send().await {
                Ok(resp) => {
                    let status = resp.status();
                    if status.is_success() {
                        return resp
                            .json::<WeatherProbabilities>()
                            .await
                            .context("Failed to parse weather probabilities response");
                    }
                    let code = status.as_u16();
                    let body = resp.text().await.unwrap_or_default();
                    if code >= 500 {
                        warn!("Weather API returned {}: {}", code, body);
                        last_err = Some(anyhow::anyhow!("Weather API returned {}: {}", code, body));
                        continue;
                    }
                    // 4xx errors are not retryable
                    anyhow::bail!("Weather API returned {}: {}", code, body);
                }
                Err(e) => {
                    warn!("Weather API request failed: {}", e);
                    last_err = Some(e.into());
                }
            }
        }

        Err(last_err.unwrap_or_else(|| anyhow::anyhow!("Weather API failed after retries")))
    }

    /// Collect WU actual for a single city/date and store in weather_actuals
    pub async fn collect_wu_actual(
        &self,
        city: &str,
        date: &str,
        ensemble_mean: Option<f64>,
        nws_forecast_high: Option<f64>,
    ) -> Result<Option<f64>> {
        let url = format!("{}/weather/collect_actual", self.base_url);

        let mut body = serde_json::json!({
            "city": city,
            "date": date,
        });
        if let Some(em) = ensemble_mean {
            body["ensemble_mean"] = serde_json::json!(em);
        }
        if let Some(nws) = nws_forecast_high {
            body["nws_forecast_high"] = serde_json::json!(nws);
        }

        let resp = self
            .client
            .post(&url)
            .json(&body)
            .send()
            .await
            .context("Failed to call collect_actual")?;

        if !resp.status().is_success() {
            let code = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("collect_actual returned {}: {}", code, body);
        }

        #[derive(Deserialize)]
        struct CollectResponse {
            wu_actual_high: Option<f64>,
        }
        let result: CollectResponse = resp
            .json()
            .await
            .context("Failed to parse collect_actual response")?;
        Ok(result.wu_actual_high)
    }

    /// Collect WU actuals for all cities for a given date (or yesterday if None)
    pub async fn collect_actuals_batch(&self, date: Option<&str>) -> Result<u32> {
        let mut url = format!("{}/weather/collect_actuals_batch", self.base_url);
        if let Some(d) = date {
            url = format!("{}?date={}", url, d);
        }

        let resp = self
            .client
            .post(&url)
            .send()
            .await
            .context("Failed to call collect_actuals_batch")?;

        if !resp.status().is_success() {
            let code = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("collect_actuals_batch returned {}: {}", code, body);
        }

        #[derive(Deserialize)]
        struct BatchResponse {
            collected: u32,
        }
        let result: BatchResponse = resp
            .json()
            .await
            .context("Failed to parse collect_actuals_batch response")?;
        Ok(result.collected)
    }

    /// Trigger calibration recomputation on the sidecar
    pub async fn trigger_calibration(&self) -> Result<u32> {
        let url = format!("{}/weather/calibrate", self.base_url);
        let resp = self
            .client
            .post(&url)
            .send()
            .await
            .context("Failed to call calibrate")?;

        if !resp.status().is_success() {
            let code = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("calibrate returned {}: {}", code, body);
        }

        #[derive(Deserialize)]
        struct CalibResponse {
            cities_calibrated: u32,
        }
        let result: CalibResponse = resp
            .json()
            .await
            .context("Failed to parse calibrate response")?;
        Ok(result.cities_calibrated)
    }

    /// Fetch weather probabilities for multiple cities in parallel
    pub async fn get_probabilities_batch(
        &self,
        cities: &[String],
        date: &str,
        same_day: bool,
    ) -> Vec<(String, WeatherProbabilities)> {
        let mut results = Vec::new();
        let mut handles = Vec::new();

        for city in cities {
            let city = city.clone();
            let date = date.to_string();
            let client = self.client.clone();
            let base_url = self.base_url.clone();
            let max_retries = self.max_retries;

            handles.push(tokio::spawn(async move {
                let weather_client = WeatherClient {
                    client,
                    base_url,
                    max_retries,
                };
                match weather_client.get_probabilities(&city, &date, same_day).await {
                    Ok(probs) => Some((city, probs)),
                    Err(e) => {
                        warn!("Batch weather fetch failed for {}: {}", city, e);
                        None
                    }
                }
            }));
        }

        for handle in handles {
            if let Ok(Some(result)) = handle.await {
                results.push(result);
            }
        }

        results
    }
}

/// Parse a Polymarket weather question to extract city, date, and temperature bucket.
///
/// Expected patterns like:
/// - "Will the high temperature in New York City on February 20, 2026 be between 40°F and 42°F?"
/// - "Will the high temperature in Chicago on March 5, 2026 be 60°F or above?"
/// - "Will the high temperature in NYC on 2026-02-20 be 72-74°F?"
pub fn parse_weather_market(question: &str) -> Option<WeatherMarketInfo> {
    // Must contain "temperature" to be a weather market
    let q_lower = question.to_lowercase();
    if !q_lower.contains("temperature") {
        return None;
    }

    // Find city using module-level constant
    let city_code = CITY_PATTERNS.iter().find_map(|(pattern, code)| {
        if question.contains(pattern) {
            Some(code.to_string())
        } else {
            None
        }
    })?;

    // Find date — try multiple formats
    let date = extract_date(question)?;

    // Find temperature range
    let (lower, upper) = extract_temperature_range(question)?;

    let bucket_label = format!("{}-{}", lower as i32, upper as i32);

    Some(WeatherMarketInfo {
        city: city_code,
        date,
        bucket_label,
        bucket_lower: lower,
        bucket_upper: upper,
    })
}

/// Extract date from question text
fn extract_date(question: &str) -> Option<String> {
    // Try ISO format first: 2026-02-20
    let iso_re = Regex::new(r"(\d{4}-\d{2}-\d{2})").ok()?;
    if let Some(caps) = iso_re.captures(question) {
        return Some(caps[1].to_string());
    }

    // Try "Month Day, Year" format: February 20, 2026
    let month_year_re = Regex::new(
        r"(?i)(January|February|March|April|May|June|July|August|September|October|November|December)\s+(\d{1,2}),?\s+(\d{4})"
    ).ok()?;

    if let Some(caps) = month_year_re.captures(question) {
        let month_name = &caps[1];
        let day: u32 = caps[2].parse().ok()?;
        let year: u32 = caps[3].parse().ok()?;

        let month = month_name_to_number(month_name)?;
        return Some(format!("{:04}-{:02}-{:02}", year, month, day));
    }

    // Try "on Month Day" format (no year): "on February 11"
    // Actual Polymarket format: "...be between 34-35°F on February 11?"
    let month_only_re = Regex::new(
        r"(?i)on\s+(January|February|March|April|May|June|July|August|September|October|November|December)\s+(\d{1,2})\b"
    ).ok()?;

    if let Some(caps) = month_only_re.captures(question) {
        let month_name = &caps[1];
        let day: u32 = caps[2].parse().ok()?;
        let month = month_name_to_number(month_name)?;

        // Infer year: if month/day is more than 7 days in the past, assume next year
        let today = Utc::now().date_naive();
        let current_year = today.year();

        let candidate = chrono::NaiveDate::from_ymd_opt(current_year, month, day)?;
        let year = if candidate < today - chrono::Duration::days(7) {
            current_year + 1
        } else {
            current_year
        };

        return Some(format!("{:04}-{:02}-{:02}", year, month, day));
    }

    None
}

/// Convert month name to number (1-12)
fn month_name_to_number(name: &str) -> Option<u32> {
    match name.to_lowercase().as_str() {
        "january" => Some(1),
        "february" => Some(2),
        "march" => Some(3),
        "april" => Some(4),
        "may" => Some(5),
        "june" => Some(6),
        "july" => Some(7),
        "august" => Some(8),
        "september" => Some(9),
        "october" => Some(10),
        "november" => Some(11),
        "december" => Some(12),
        _ => None,
    }
}

/// Extract temperature range (lower, upper) from question text
fn extract_temperature_range(question: &str) -> Option<(f64, f64)> {
    // Pattern: "between X°F and Y°F" or "between XF and YF"
    let between_and_re = Regex::new(r"between\s+(\d+)°?F?\s+and\s+(\d+)°?F").ok()?;
    if let Some(caps) = between_and_re.captures(question) {
        let lower: f64 = caps[1].parse().ok()?;
        let upper: f64 = caps[2].parse().ok()?;
        return Some((lower, upper));
    }

    // Pattern: "between X-Y°F" (actual Polymarket format)
    let between_dash_re = Regex::new(r"between\s+(\d+)\s*[-\u{2013}]\s*(\d+)°F").ok()?;
    if let Some(caps) = between_dash_re.captures(question) {
        let lower: f64 = caps[1].parse().ok()?;
        let upper: f64 = caps[2].parse().ok()?;
        return Some((lower, upper));
    }

    // Pattern: "X-Y°F" or "X - Y°F" (standalone range without "between")
    let range_re = Regex::new(r"(\d+)\s*[-\u{2013}]\s*(\d+)°F").ok()?;
    if let Some(caps) = range_re.captures(question) {
        let lower: f64 = caps[1].parse().ok()?;
        let upper: f64 = caps[2].parse().ok()?;
        return Some((lower, upper));
    }

    // Pattern: "X°F or above" / "X°F or higher" → bucket [X, 130]
    let above_re = Regex::new(r"(\d+)°F\s+or\s+(?:above|higher|more)").ok()?;
    if let Some(caps) = above_re.captures(question) {
        let lower: f64 = caps[1].parse().ok()?;
        return Some((lower, 130.0));
    }

    // Pattern: "X°F or below" (actual Polymarket format) → bucket [-60, X]
    let or_below_re = Regex::new(r"(\d+)°F\s+or\s+below").ok()?;
    if let Some(caps) = or_below_re.captures(question) {
        let upper: f64 = caps[1].parse().ok()?;
        return Some((-60.0, upper));
    }

    // Pattern: "below X°F" / "under X°F" → bucket [-60, X]
    let below_re = Regex::new(r"(?:below|under)\s+(\d+)°F").ok()?;
    if let Some(caps) = below_re.captures(question) {
        let upper: f64 = caps[1].parse().ok()?;
        return Some((-60.0, upper));
    }

    None
}

/// Look up the model probability for a specific bucket from weather probabilities
pub fn get_weather_model_probability(
    info: &WeatherMarketInfo,
    probs: &WeatherProbabilities,
) -> Option<f64> {
    // For "X or above" type markets, sum all buckets >= lower
    if info.bucket_upper >= 130.0 {
        let total: f64 = probs
            .buckets
            .iter()
            .filter(|b| b.lower >= info.bucket_lower)
            .map(|b| b.probability)
            .sum();
        return Some(total);
    }

    // For "below X" type markets, sum all buckets < upper
    if info.bucket_lower <= -59.0 {
        let total: f64 = probs
            .buckets
            .iter()
            .filter(|b| b.upper <= info.bucket_upper)
            .map(|b| b.probability)
            .sum();
        return Some(total);
    }

    // For exact range, find matching bucket(s)
    let total: f64 = probs
        .buckets
        .iter()
        .filter(|b| b.lower >= info.bucket_lower && b.upper <= info.bucket_upper)
        .map(|b| b.probability)
        .sum();

    if total > 0.0 {
        Some(total)
    } else {
        // Try overlapping buckets
        let total: f64 = probs
            .buckets
            .iter()
            .filter(|b| b.lower < info.bucket_upper && b.upper > info.bucket_lower)
            .map(|b| {
                // Calculate overlap fraction
                let overlap_lower = b.lower.max(info.bucket_lower);
                let overlap_upper = b.upper.min(info.bucket_upper);
                let overlap = (overlap_upper - overlap_lower) / (b.upper - b.lower);
                b.probability * overlap
            })
            .sum();
        Some(total)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn sample_weather_response() -> serde_json::Value {
        serde_json::json!({
            "city": "NYC",
            "station_icao": "KLGA",
            "forecast_date": "2026-02-20",
            "buckets": [
                {"bucket_label": "72-74", "lower": 72.0, "upper": 74.0, "probability": 0.15},
                {"bucket_label": "74-76", "lower": 74.0, "upper": 76.0, "probability": 0.35},
                {"bucket_label": "76-78", "lower": 76.0, "upper": 78.0, "probability": 0.30},
                {"bucket_label": "78-80", "lower": 78.0, "upper": 80.0, "probability": 0.15},
                {"bucket_label": "80-82", "lower": 80.0, "upper": 82.0, "probability": 0.05}
            ],
            "ensemble_mean": 75.8,
            "ensemble_std": 2.3,
            "gefs_count": 31,
            "ecmwf_count": 51
        })
    }

    #[tokio::test]
    async fn test_get_probabilities_success() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/weather/probabilities"))
            .and(query_param("city", "NYC"))
            .and(query_param("date", "2026-02-20"))
            .respond_with(ResponseTemplate::new(200).set_body_json(sample_weather_response()))
            .mount(&server)
            .await;

        let client = WeatherClient::new(&server.uri(), 5, 1).unwrap();
        let result = client.get_probabilities("NYC", "2026-02-20", false).await.unwrap();

        assert_eq!(result.city, "NYC");
        assert_eq!(result.station_icao, "KLGA");
        assert_eq!(result.buckets.len(), 5);
        assert!((result.ensemble_mean - 75.8).abs() < 0.01);
        assert_eq!(result.gefs_count, 31);
        assert_eq!(result.ecmwf_count, 51);
    }

    #[tokio::test]
    async fn test_get_probabilities_404() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/weather/probabilities"))
            .respond_with(ResponseTemplate::new(404).set_body_string("Unknown city"))
            .mount(&server)
            .await;

        let client = WeatherClient::new(&server.uri(), 5, 0).unwrap();
        let result = client.get_probabilities("UNKNOWN", "2026-02-20", false).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_get_probabilities_502_retries() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/weather/probabilities"))
            .respond_with(ResponseTemplate::new(502).set_body_string("Upstream failed"))
            .expect(2) // initial + 1 retry
            .mount(&server)
            .await;

        let client = WeatherClient::new(&server.uri(), 5, 1).unwrap();
        let result = client.get_probabilities("NYC", "2026-02-20", false).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_get_probabilities_batch() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/weather/probabilities"))
            .respond_with(ResponseTemplate::new(200).set_body_json(sample_weather_response()))
            .mount(&server)
            .await;

        let client = WeatherClient::new(&server.uri(), 5, 1).unwrap();
        let cities = vec!["NYC".to_string(), "CHI".to_string()];
        let results = client.get_probabilities_batch(&cities, "2026-02-20", false).await;
        assert_eq!(results.len(), 2);
    }

    // --- Tests using actual Polymarket question formats ---

    #[test]
    fn test_parse_weather_market_between_dash() {
        // Actual Polymarket format: "between X-Y°F on Month Day" (no year)
        let q = "Will the highest temperature in New York City be between 34-35\u{00b0}F on February 11?";
        let info = parse_weather_market(q).unwrap();
        assert_eq!(info.city, "NYC");
        assert_eq!(info.bucket_lower, 34.0);
        assert_eq!(info.bucket_upper, 35.0);
        assert_eq!(info.bucket_label, "34-35");
        // Date should be inferred with current year (or next year if past)
        assert!(info.date.starts_with("20")); // valid year
        assert!(info.date.ends_with("-02-11"));
    }

    #[test]
    fn test_parse_weather_market_or_below() {
        // Actual Polymarket format: "X°F or below"
        let q = "Will the highest temperature in Chicago be 33\u{00b0}F or below on February 11?";
        let info = parse_weather_market(q).unwrap();
        assert_eq!(info.city, "CHI");
        assert_eq!(info.bucket_lower, -60.0);
        assert_eq!(info.bucket_upper, 33.0);
        assert!(info.date.ends_with("-02-11"));
    }

    #[test]
    fn test_parse_weather_market_or_higher() {
        // Actual Polymarket format: "X°F or higher"
        let q = "Will the highest temperature in Miami be 78\u{00b0}F or higher on February 12?";
        let info = parse_weather_market(q).unwrap();
        assert_eq!(info.city, "MIA");
        assert_eq!(info.bucket_lower, 78.0);
        assert_eq!(info.bucket_upper, 130.0);
        assert!(info.date.ends_with("-02-12"));
    }

    #[test]
    fn test_parse_weather_market_not_weather() {
        let q = "Will Bitcoin reach $100,000 by March 2026?";
        assert!(parse_weather_market(q).is_none());
    }

    // --- Legacy format tests (kept as fallback coverage) ---

    #[test]
    fn test_parse_weather_market_between_and() {
        // Legacy format: "between X°F and Y°F" with full date
        let q = "Will the high temperature in New York City on February 20, 2026 be between 74\u{00b0}F and 76\u{00b0}F?";
        let info = parse_weather_market(q).unwrap();
        assert_eq!(info.city, "NYC");
        assert_eq!(info.date, "2026-02-20");
        assert_eq!(info.bucket_lower, 74.0);
        assert_eq!(info.bucket_upper, 76.0);
    }

    #[test]
    fn test_parse_weather_market_iso_date() {
        let q = "Will the high temperature in Chicago on 2026-03-05 be 60-62\u{00b0}F?";
        let info = parse_weather_market(q).unwrap();
        assert_eq!(info.city, "CHI");
        assert_eq!(info.date, "2026-03-05");
        assert_eq!(info.bucket_lower, 60.0);
        assert_eq!(info.bucket_upper, 62.0);
    }

    #[test]
    fn test_get_weather_model_probability_exact_bucket() {
        let probs = WeatherProbabilities {
            city: "NYC".to_string(),
            station_icao: "KLGA".to_string(),
            forecast_date: "2026-02-20".to_string(),
            buckets: vec![
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
            ensemble_mean: 75.5,
            ensemble_std: 2.0,
            gefs_count: 31,
            ecmwf_count: 51,
            ..Default::default()
        };

        let info = WeatherMarketInfo {
            city: "NYC".to_string(),
            date: "2026-02-20".to_string(),
            bucket_label: "74-76".to_string(),
            bucket_lower: 74.0,
            bucket_upper: 76.0,
        };

        let prob = get_weather_model_probability(&info, &probs).unwrap();
        assert!((prob - 0.35).abs() < 0.01);
    }

    #[test]
    fn test_get_weather_model_probability_above() {
        let probs = WeatherProbabilities {
            city: "MIA".to_string(),
            station_icao: "KMIA".to_string(),
            forecast_date: "2026-03-10".to_string(),
            buckets: vec![
                BucketProbability {
                    bucket_label: "88-90".to_string(),
                    lower: 88.0,
                    upper: 90.0,
                    probability: 0.20,
                },
                BucketProbability {
                    bucket_label: "90-92".to_string(),
                    lower: 90.0,
                    upper: 92.0,
                    probability: 0.05,
                },
                BucketProbability {
                    bucket_label: "92-94".to_string(),
                    lower: 92.0,
                    upper: 94.0,
                    probability: 0.01,
                },
            ],
            ensemble_mean: 85.0,
            ensemble_std: 3.0,
            gefs_count: 31,
            ecmwf_count: 51,
            ..Default::default()
        };

        let info = WeatherMarketInfo {
            city: "MIA".to_string(),
            date: "2026-03-10".to_string(),
            bucket_label: "90-130".to_string(),
            bucket_lower: 90.0,
            bucket_upper: 130.0,
        };

        let prob = get_weather_model_probability(&info, &probs).unwrap();
        assert!((prob - 0.06).abs() < 0.01); // 0.05 + 0.01
    }
}
