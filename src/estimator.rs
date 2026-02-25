use anyhow::{Context, Result};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tracing::{debug, info, warn};

use crate::clob_client::MarketPrices;
use crate::config::Config;
use crate::market_scanner::GammaMarket;
use crate::weather_client::WeatherProbabilities;

// ─── Token pricing (USD per million tokens) ───

#[derive(Debug, Clone, Copy)]
pub struct ModelPricing {
    pub input_per_mtok: f64,
    pub output_per_mtok: f64,
}

impl ModelPricing {
    pub fn for_model(model: &str) -> Self {
        if model.contains("haiku") {
            ModelPricing {
                input_per_mtok: 1.0,
                output_per_mtok: 5.0,
            }
        } else if model.contains("opus") {
            ModelPricing {
                input_per_mtok: 15.0,
                output_per_mtok: 75.0,
            }
        } else {
            // Sonnet or unknown — use Sonnet pricing as safe default
            ModelPricing {
                input_per_mtok: 3.0,
                output_per_mtok: 15.0,
            }
        }
    }

    pub fn calculate_cost(&self, input_tokens: u64, output_tokens: u64) -> f64 {
        (input_tokens as f64 / 1_000_000.0) * self.input_per_mtok
            + (output_tokens as f64 / 1_000_000.0) * self.output_per_mtok
    }
}

// ─── Anthropic API types ───

#[derive(Debug, Serialize)]
struct AnthropicRequest {
    model: String,
    max_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<String>,
    messages: Vec<AnthropicMessage>,
}

#[derive(Debug, Serialize)]
struct AnthropicMessage {
    role: String,
    content: String,
}

#[derive(Debug, Deserialize)]
struct AnthropicResponse {
    #[allow(dead_code)]
    id: String,
    content: Vec<ContentBlock>,
    #[allow(dead_code)]
    model: String,
    #[allow(dead_code)]
    stop_reason: Option<String>,
    usage: ApiUsage,
}

#[derive(Debug, Deserialize)]
struct ContentBlock {
    #[serde(rename = "type")]
    #[allow(dead_code)]
    block_type: String,
    text: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ApiUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    #[serde(default)]
    pub cache_creation_input_tokens: u64,
    #[serde(default)]
    pub cache_read_input_tokens: u64,
}

// ─── Analysis types ───

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FairValueEstimate {
    pub probability: f64,
    pub confidence: f64,
    pub reasoning: String,
    pub data_quality: String,
}

#[derive(Debug, Clone)]
pub struct ApiCallCost {
    pub model: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cost_usd: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TriageDecision {
    Analyze,
    Skip,
}

#[derive(Debug, Clone)]
pub struct AnalysisResult {
    pub market_id: String,
    pub question: String,
    pub estimate: FairValueEstimate,
    pub market_yes_price: f64,
    pub total_cost: f64,
    pub api_calls: Vec<ApiCallCost>,
}

// ─── Weather context for prompt enrichment ───

/// Weather data passed to the estimator for prompt enrichment
pub struct WeatherContext<'a> {
    pub probs: &'a WeatherProbabilities,
    pub model_probability: Option<f64>,
}

// ─── The Estimator ───

pub struct Estimator {
    client: Client,
    api_url: String,
    api_key: String,
    haiku_model: String,
    sonnet_model: String,
    prompt_template: String,
    max_retries: u32,
}

impl Estimator {
    pub fn new(config: &Config) -> Result<Self> {
        let prompt_template = std::fs::read_to_string("prompts/fair_value.md")
            .context("Failed to load prompts/fair_value.md")?;

        let client = Client::builder()
            .timeout(Duration::from_secs(config.estimator_request_timeout_secs))
            .build()
            .context("Failed to build Estimator HTTP client")?;

        Ok(Estimator {
            client,
            api_url: config.anthropic_api_url.clone(),
            api_key: config.anthropic_api_key.clone(),
            haiku_model: config.haiku_model.clone(),
            sonnet_model: config.sonnet_model.clone(),
            prompt_template,
            max_retries: config.estimator_max_retries,
        })
    }

    #[cfg(test)]
    fn with_client(
        client: Client,
        api_url: String,
        api_key: String,
        prompt_template: String,
    ) -> Self {
        Estimator {
            client,
            api_url,
            api_key,
            haiku_model: "claude-haiku-4-5-20251001".to_string(),
            sonnet_model: "claude-sonnet-4-5-20250929".to_string(),
            prompt_template,
            max_retries: 1,
        }
    }

    /// Haiku triage — quick check if market is worth deep analysis
    pub async fn triage(
        &self,
        market: &GammaMarket,
        prices: &MarketPrices,
    ) -> Result<(TriageDecision, ApiCallCost)> {
        let yes_price = prices.midpoint;
        let prompt = format!(
            "You are a prediction market analyst. A market asks: \"{}\"\n\
             Current YES price: {:.2} (implied {:.0}% probability). Category: {}. Volume: ${:.0}.\n\n\
             Could a well-informed analyst find >8% mispricing here? \
             Answer ONLY \"YES\" or \"NO\" with one brief sentence of explanation.",
            market.question,
            yes_price,
            yes_price * 100.0,
            market
                .tags
                .first()
                .and_then(|t| t.label.as_deref())
                .unwrap_or("General"),
            market.volume.unwrap_or(0.0),
        );

        let response = self.call_claude(&self.haiku_model, &prompt, 100).await?;
        let text = self.extract_text(&response)?;
        let decision = if text.to_uppercase().starts_with("YES") {
            TriageDecision::Analyze
        } else {
            TriageDecision::Skip
        };

        let pricing = ModelPricing::for_model(&self.haiku_model);
        let cost = ApiCallCost {
            model: self.haiku_model.clone(),
            input_tokens: response.usage.input_tokens,
            output_tokens: response.usage.output_tokens,
            cost_usd: pricing
                .calculate_cost(response.usage.input_tokens, response.usage.output_tokens),
        };

        debug!(
            "Triage for '{}': {:?} (cost: ${:.5})",
            market.question, decision, cost.cost_usd
        );
        Ok((decision, cost))
    }

    /// Sonnet deep analysis — full fair value estimation
    pub async fn analyze(
        &self,
        market: &GammaMarket,
        prices: &MarketPrices,
        weather: Option<&WeatherContext<'_>>,
    ) -> Result<(FairValueEstimate, ApiCallCost)> {
        let prompt = self.render_prompt(market, prices, weather);
        let response = self.call_claude(&self.sonnet_model, &prompt, 1024).await?;
        let estimate = self.parse_estimate(&response)?;

        let pricing = ModelPricing::for_model(&self.sonnet_model);
        let cost = ApiCallCost {
            model: self.sonnet_model.clone(),
            input_tokens: response.usage.input_tokens,
            output_tokens: response.usage.output_tokens,
            cost_usd: pricing
                .calculate_cost(response.usage.input_tokens, response.usage.output_tokens),
        };

        info!(
            "Analysis for '{}': prob={:.2}, conf={:.2}, cost=${:.5}",
            market.question, estimate.probability, estimate.confidence, cost.cost_usd
        );
        Ok((estimate, cost))
    }

    /// Full two-tier pipeline with cost budget enforcement
    pub async fn evaluate(
        &self,
        market: &GammaMarket,
        prices: &MarketPrices,
        cycle_cost_so_far: f64,
        max_cost_per_cycle: f64,
        weather: Option<&WeatherContext<'_>>,
    ) -> Result<Option<AnalysisResult>> {
        if cycle_cost_so_far >= max_cost_per_cycle {
            info!(
                "Cycle cost budget exhausted (${:.4}), skipping",
                cycle_cost_so_far
            );
            return Ok(None);
        }

        let (decision, triage_cost) = self.triage(market, prices).await?;
        let mut total_cost = triage_cost.cost_usd;
        let mut api_calls = vec![triage_cost];

        if decision == TriageDecision::Skip {
            return Ok(None);
        }

        if cycle_cost_so_far + total_cost >= max_cost_per_cycle {
            info!("Budget exhausted after triage, skipping deep analysis");
            return Ok(None);
        }

        let (estimate, analysis_cost) = self.analyze(market, prices, weather).await?;
        total_cost += analysis_cost.cost_usd;
        api_calls.push(analysis_cost);

        Ok(Some(AnalysisResult {
            market_id: market.condition_id.clone().unwrap_or_default(),
            question: market.question.clone(),
            estimate,
            market_yes_price: prices.midpoint,
            total_cost,
            api_calls,
        }))
    }

    // ─── Internal helpers ───

    fn render_prompt(
        &self,
        market: &GammaMarket,
        prices: &MarketPrices,
        weather: Option<&WeatherContext<'_>>,
    ) -> String {
        let category = market
            .tags
            .first()
            .and_then(|t| t.label.as_deref())
            .unwrap_or("General");

        let yes_price = prices.midpoint;
        let no_price = 1.0 - yes_price;

        let mut prompt = self.prompt_template.clone();
        prompt = prompt.replace("{{question}}", &market.question);
        prompt = prompt.replace("{{resolution_criteria}}", "See market description");
        prompt = prompt.replace(
            "{{end_date}}",
            market.end_date.as_deref().unwrap_or("Unknown"),
        );
        prompt = prompt.replace("{{category}}", category);
        prompt = prompt.replace("{{yes_price}}", &format!("{:.2}", yes_price));
        prompt = prompt.replace("{{no_price}}", &format!("{:.2}", no_price));
        prompt = prompt.replace(
            "{{volume_24h}}",
            &format!("{:.0}", market.volume.unwrap_or(0.0)),
        );
        prompt = prompt.replace(
            "{{liquidity}}",
            &format!("{:.0}", market.liquidity.unwrap_or(0.0)),
        );

        // Weather: fill or remove conditional block
        if let Some(wx) = weather {
            let weather_content = Self::render_weather_block(wx);
            prompt = Self::replace_conditional_block(&prompt, "weather_data", &weather_content);
        } else {
            prompt = Self::remove_conditional_blocks(&prompt, "weather_data");
        }

        // Remove other conditional blocks — not yet available
        prompt = Self::remove_conditional_blocks(&prompt, "sports_data");
        prompt = Self::remove_conditional_blocks(&prompt, "crypto_data");
        prompt = Self::remove_conditional_blocks(&prompt, "news_data");

        prompt
    }

    /// Render weather ensemble data as prompt content
    fn render_weather_block(wx: &WeatherContext<'_>) -> String {
        let mut block = String::new();
        block.push_str("### Weather Ensemble Forecast\n");
        block.push_str(&format!("- **City:** {}\n", wx.probs.city));
        block.push_str(&format!(
            "- **Station:** {} (resolution source: Weather Underground)\n",
            wx.probs.station_icao
        ));
        block.push_str(&format!(
            "- **Forecast date:** {}\n",
            wx.probs.forecast_date
        ));
        block.push_str(&format!(
            "- **Ensemble members:** {} GEFS + {} ECMWF = {} total\n",
            wx.probs.gefs_count,
            wx.probs.ecmwf_count,
            wx.probs.gefs_count + wx.probs.ecmwf_count,
        ));
        block.push_str(&format!(
            "- **Combined ensemble mean:** {:.1}°F\n",
            wx.probs.ensemble_mean
        ));
        block.push_str(&format!(
            "- **Combined ensemble std dev:** {:.1}°F\n",
            wx.probs.ensemble_std
        ));
        block.push_str("- **Temperature bucket probabilities:**\n");
        for bucket in &wx.probs.buckets {
            if bucket.probability > 0.005 {
                block.push_str(&format!(
                    "  - {}°F: {:.1}%\n",
                    bucket.bucket_label,
                    bucket.probability * 100.0
                ));
            }
        }
        if let Some(nws_high) = wx.probs.nws_forecast_high {
            block.push_str(&format!(
                "- **NWS Official Forecast High:** {:.0}°F\n",
                nws_high
            ));
        }
        if let Some(bias) = wx.probs.bias_correction {
            block.push_str(&format!(
                "- **Bias Correction Applied:** {:+.1}°F (ensemble shifted to match NWS)\n",
                bias
            ));
        }
        if let Some(mp) = wx.model_probability {
            block.push_str(&format!(
                "- **Model probability for this outcome:** {:.1}%\n",
                mp * 100.0
            ));
        }
        block
    }

    /// Replace {{#if var}}...{{/if}} with provided content
    fn replace_conditional_block(template: &str, var_name: &str, content: &str) -> String {
        let start_tag = format!("{{{{#if {}}}}}", var_name);
        let end_tag = "{{/if}}";

        if let Some(start_pos) = template.find(&start_tag) {
            if let Some(end_offset) = template[start_pos..].find(end_tag) {
                let end_abs = start_pos + end_offset + end_tag.len();
                let end_abs = if template.as_bytes().get(end_abs) == Some(&b'\n') {
                    end_abs + 1
                } else {
                    end_abs
                };
                return format!(
                    "{}{}{}",
                    &template[..start_pos],
                    content,
                    &template[end_abs..]
                );
            }
        }
        template.to_string()
    }

    /// Remove {{#if var}}...{{/if}} blocks from template
    fn remove_conditional_blocks(template: &str, var_name: &str) -> String {
        let start_tag = format!("{{{{#if {}}}}}", var_name);
        let end_tag = "{{/if}}";

        let mut result = template.to_string();
        while let Some(start_pos) = result.find(&start_tag) {
            if let Some(end_pos) = result[start_pos..].find(end_tag) {
                let end_abs = start_pos + end_pos + end_tag.len();
                // Also remove trailing newline if present
                let end_abs = if result.as_bytes().get(end_abs) == Some(&b'\n') {
                    end_abs + 1
                } else {
                    end_abs
                };
                result = format!("{}{}", &result[..start_pos], &result[end_abs..]);
            } else {
                break;
            }
        }
        result
    }

    fn extract_text(&self, response: &AnthropicResponse) -> Result<String> {
        response
            .content
            .first()
            .and_then(|block| block.text.clone())
            .context("No text in Claude response")
    }

    fn parse_estimate(&self, response: &AnthropicResponse) -> Result<FairValueEstimate> {
        let text = self.extract_text(response)?;

        // Try direct JSON parse first
        let estimate: FairValueEstimate = serde_json::from_str(&text)
            .or_else(|_| {
                // Try stripping markdown code fences
                let stripped = text
                    .trim()
                    .strip_prefix("```json")
                    .or_else(|| text.trim().strip_prefix("```"))
                    .unwrap_or(&text)
                    .strip_suffix("```")
                    .unwrap_or(&text)
                    .trim();
                serde_json::from_str(stripped)
            })
            .context("Failed to parse Claude response as FairValueEstimate JSON")?;

        // Validate ranges
        if !(0.0..=1.0).contains(&estimate.probability) {
            anyhow::bail!("probability {} out of range [0, 1]", estimate.probability);
        }
        if !(0.0..=1.0).contains(&estimate.confidence) {
            anyhow::bail!("confidence {} out of range [0, 1]", estimate.confidence);
        }
        let valid_qualities = ["high", "medium", "low"];
        if !valid_qualities.contains(&estimate.data_quality.as_str()) {
            anyhow::bail!("invalid data_quality: {}", estimate.data_quality);
        }

        Ok(estimate)
    }

    async fn call_claude(
        &self,
        model: &str,
        user_message: &str,
        max_tokens: u32,
    ) -> Result<AnthropicResponse> {
        let request = AnthropicRequest {
            model: model.to_string(),
            max_tokens,
            system: None,
            messages: vec![AnthropicMessage {
                role: "user".to_string(),
                content: user_message.to_string(),
            }],
        };

        let mut last_err = None;
        for attempt in 0..=self.max_retries {
            if attempt > 0 {
                let delay = Duration::from_millis(1000 * 2u64.pow(attempt - 1));
                warn!(
                    "Retrying Claude API after {:?} (attempt {})",
                    delay,
                    attempt + 1
                );
                tokio::time::sleep(delay).await;
            }

            match self
                .client
                .post(format!("{}/v1/messages", self.api_url))
                .header("x-api-key", &self.api_key)
                .header("anthropic-version", "2023-06-01")
                .header("content-type", "application/json")
                .json(&request)
                .send()
                .await
            {
                Ok(resp) => {
                    let status = resp.status();
                    if status.is_success() {
                        return resp
                            .json::<AnthropicResponse>()
                            .await
                            .context("Failed to parse Anthropic API response");
                    }
                    let code = status.as_u16();
                    let body = resp.text().await.unwrap_or_default();
                    if code == 429 || code >= 500 {
                        warn!("Anthropic API returned {}: {}", code, body);
                        last_err =
                            Some(anyhow::anyhow!("Anthropic API returned {}: {}", code, body));
                        continue;
                    }
                    anyhow::bail!("Anthropic API returned {}: {}", code, body);
                }
                Err(e) => {
                    warn!("Anthropic API request failed: {}", e);
                    last_err = Some(e.into());
                }
            }
        }
        Err(last_err.unwrap_or_else(|| anyhow::anyhow!("Anthropic API failed after retries")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::market_scanner::{GammaMarket, Tag, Token};
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn sample_market() -> GammaMarket {
        GammaMarket {
            id: "1".to_string(),
            question: "Will it rain in NYC tomorrow?".to_string(),
            slug: Some("rain-nyc".to_string()),
            condition_id: Some("0xcond1".to_string()),
            tokens: vec![
                Token {
                    token_id: "tok_yes".to_string(),
                    outcome: "Yes".to_string(),
                    price: Some(0.65),
                },
                Token {
                    token_id: "tok_no".to_string(),
                    outcome: "No".to_string(),
                    price: Some(0.35),
                },
            ],
            volume: Some(5000.0),
            liquidity: Some(2000.0),
            end_date: Some("2026-03-01T00:00:00Z".to_string()),
            closed: false,
            active: true,
            tags: vec![Tag {
                id: Some(84),
                label: Some("Weather".to_string()),
            }],
        }
    }

    fn sample_prices() -> MarketPrices {
        MarketPrices {
            token_id: "tok_yes".to_string(),
            outcome: "Yes".to_string(),
            midpoint: 0.65,
            best_bid: Some(0.64),
            best_ask: Some(0.66),
            spread: Some(0.02),
        }
    }

    fn mock_anthropic_response(text: &str) -> serde_json::Value {
        serde_json::json!({
            "id": "msg_test",
            "content": [{"type": "text", "text": text}],
            "model": "claude-haiku-4-5-20251001",
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 500, "output_tokens": 50}
        })
    }

    fn test_template() -> String {
        "Question: {{question}}\nCategory: {{category}}\nYES: {{yes_price}}\nNO: {{no_price}}\nEnd: {{end_date}}\nVolume: {{volume_24h}}\nLiquidity: {{liquidity}}\n{{#if weather_data}}Weather: {{weather}}{{/if}}\n{{#if sports_data}}Sports: {{sports}}{{/if}}".to_string()
    }

    // 1. test_model_pricing_haiku
    #[test]
    fn test_model_pricing_haiku() {
        let pricing = ModelPricing::for_model("claude-haiku-4-5-20251001");
        assert_eq!(pricing.input_per_mtok, 1.0);
        assert_eq!(pricing.output_per_mtok, 5.0);
        let cost = pricing.calculate_cost(500, 50);
        // (500/1M)*1.0 + (50/1M)*5.0 = 0.0005 + 0.00025 = 0.00075
        let expected = 0.0005 + 0.00025;
        assert!(
            (cost - expected).abs() < 1e-10,
            "expected {}, got {}",
            expected,
            cost
        );
    }

    // 2. test_model_pricing_sonnet
    #[test]
    fn test_model_pricing_sonnet() {
        let pricing = ModelPricing::for_model("claude-sonnet-4-5-20250929");
        assert_eq!(pricing.input_per_mtok, 3.0);
        assert_eq!(pricing.output_per_mtok, 15.0);
    }

    // 3. test_model_pricing_unknown_defaults
    #[test]
    fn test_model_pricing_unknown_defaults() {
        let pricing = ModelPricing::for_model("some-unknown-model");
        // Should default to sonnet pricing
        assert_eq!(pricing.input_per_mtok, 3.0);
        assert_eq!(pricing.output_per_mtok, 15.0);
    }

    // 4. test_render_prompt_basic
    #[test]
    fn test_render_prompt_basic() {
        let estimator = Estimator::with_client(
            Client::new(),
            "http://unused".to_string(),
            "test-key".to_string(),
            test_template(),
        );
        let market = sample_market();
        let prices = sample_prices();
        let prompt = estimator.render_prompt(&market, &prices, None);

        assert!(prompt.contains("Will it rain in NYC tomorrow?"));
        assert!(prompt.contains("Weather"));
        assert!(prompt.contains("0.65"));
        assert!(prompt.contains("0.35"));
        assert!(prompt.contains("2026-03-01T00:00:00Z"));
        assert!(prompt.contains("5000"));
        assert!(prompt.contains("2000"));
    }

    // 5. test_render_prompt_removes_conditional_blocks
    #[test]
    fn test_render_prompt_removes_conditional_blocks() {
        let estimator = Estimator::with_client(
            Client::new(),
            "http://unused".to_string(),
            "test-key".to_string(),
            test_template(),
        );
        let market = sample_market();
        let prices = sample_prices();
        let prompt = estimator.render_prompt(&market, &prices, None);

        // The {{#if weather_data}}...{{/if}} block should be removed
        assert!(!prompt.contains("{{#if weather_data}}"));
        assert!(!prompt.contains("{{/if}}"));
        assert!(!prompt.contains("{{weather}}"));
        assert!(!prompt.contains("{{#if sports_data}}"));
    }

    // 5b. test_render_prompt_with_weather
    #[test]
    fn test_render_prompt_with_weather() {
        use crate::weather_client::{BucketProbability as WBucket, WeatherProbabilities as WProbs};

        let estimator = Estimator::with_client(
            Client::new(),
            "http://unused".to_string(),
            "test-key".to_string(),
            test_template(),
        );
        let market = sample_market();
        let prices = sample_prices();

        let weather_probs = WProbs {
            city: "NYC".to_string(),
            station_icao: "KLGA".to_string(),
            forecast_date: "2026-02-20".to_string(),
            buckets: vec![
                WBucket {
                    bucket_label: "74-76".to_string(),
                    lower: 74.0,
                    upper: 76.0,
                    probability: 0.35,
                },
                WBucket {
                    bucket_label: "76-78".to_string(),
                    lower: 76.0,
                    upper: 78.0,
                    probability: 0.30,
                },
            ],
            ensemble_mean: 75.8,
            ensemble_std: 2.3,
            gefs_count: 31,
            ecmwf_count: 51,
            gem_count: None,
            icon_count: None,
            nws_forecast_high: None,
            bias_correction: None,
            nbm_p50: None,
            anchor_source: None,
        };
        let wx = WeatherContext {
            probs: &weather_probs,
            model_probability: Some(0.35),
        };
        let prompt = estimator.render_prompt(&market, &prices, Some(&wx));

        // Weather block should be filled in, not removed
        assert!(!prompt.contains("{{#if weather_data}}"));
        assert!(prompt.contains("KLGA"));
        assert!(prompt.contains("2026-02-20"));
        assert!(prompt.contains("75.8"));
        assert!(prompt.contains("31 GEFS"));
        assert!(prompt.contains("74-76"));
        assert!(prompt.contains("35.0%"));
    }

    // 6. test_parse_estimate_valid_json
    #[test]
    fn test_parse_estimate_valid_json() {
        let estimator = Estimator::with_client(
            Client::new(),
            "http://unused".to_string(),
            "test-key".to_string(),
            String::new(),
        );
        let response_json = r#"{"probability": 0.72, "confidence": 0.85, "reasoning": "Test reasoning", "data_quality": "high"}"#;
        let response: AnthropicResponse =
            serde_json::from_value(mock_anthropic_response(response_json)).unwrap();
        let estimate = estimator.parse_estimate(&response).unwrap();

        assert!((estimate.probability - 0.72).abs() < 1e-10);
        assert!((estimate.confidence - 0.85).abs() < 1e-10);
        assert_eq!(estimate.reasoning, "Test reasoning");
        assert_eq!(estimate.data_quality, "high");
    }

    // 7. test_parse_estimate_fenced_json
    #[test]
    fn test_parse_estimate_fenced_json() {
        let estimator = Estimator::with_client(
            Client::new(),
            "http://unused".to_string(),
            "test-key".to_string(),
            String::new(),
        );
        let response_json = "```json\n{\"probability\": 0.60, \"confidence\": 0.70, \"reasoning\": \"Fenced test\", \"data_quality\": \"medium\"}\n```";
        let response: AnthropicResponse =
            serde_json::from_value(mock_anthropic_response(response_json)).unwrap();
        let estimate = estimator.parse_estimate(&response).unwrap();

        assert!((estimate.probability - 0.60).abs() < 1e-10);
        assert!((estimate.confidence - 0.70).abs() < 1e-10);
        assert_eq!(estimate.data_quality, "medium");
    }

    // 8. test_parse_estimate_invalid_probability
    #[test]
    fn test_parse_estimate_invalid_probability() {
        let estimator = Estimator::with_client(
            Client::new(),
            "http://unused".to_string(),
            "test-key".to_string(),
            String::new(),
        );
        let response_json = r#"{"probability": 1.5, "confidence": 0.85, "reasoning": "Bad", "data_quality": "high"}"#;
        let response: AnthropicResponse =
            serde_json::from_value(mock_anthropic_response(response_json)).unwrap();
        let result = estimator.parse_estimate(&response);

        assert!(result.is_err());
        assert!(
            result.unwrap_err().to_string().contains("probability"),
            "Error should mention probability"
        );
    }

    // 9. test_parse_estimate_invalid_data_quality
    #[test]
    fn test_parse_estimate_invalid_data_quality() {
        let estimator = Estimator::with_client(
            Client::new(),
            "http://unused".to_string(),
            "test-key".to_string(),
            String::new(),
        );
        let response_json = r#"{"probability": 0.72, "confidence": 0.85, "reasoning": "Bad quality", "data_quality": "excellent"}"#;
        let response: AnthropicResponse =
            serde_json::from_value(mock_anthropic_response(response_json)).unwrap();
        let result = estimator.parse_estimate(&response);

        assert!(result.is_err());
        assert!(
            result.unwrap_err().to_string().contains("data_quality"),
            "Error should mention data_quality"
        );
    }

    // 10. test_triage_returns_decision
    #[tokio::test]
    async fn test_triage_returns_decision() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .and(header("x-api-key", "test-key"))
            .and(header("anthropic-version", "2023-06-01"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(mock_anthropic_response(
                    "YES. This weather market likely has mispricing based on ensemble data.",
                )),
            )
            .mount(&server)
            .await;

        let estimator = Estimator::with_client(
            Client::new(),
            server.uri(),
            "test-key".to_string(),
            String::new(),
        );
        let market = sample_market();
        let prices = sample_prices();

        let (decision, cost) = estimator.triage(&market, &prices).await.unwrap();
        assert_eq!(decision, TriageDecision::Analyze);
        assert!(cost.cost_usd > 0.0);
        assert_eq!(cost.input_tokens, 500);
        assert_eq!(cost.output_tokens, 50);
    }

    // 11. test_evaluate_budget_enforcement
    #[tokio::test]
    async fn test_evaluate_budget_enforcement() {
        // No mock server needed — should return None without making any API calls
        let estimator = Estimator::with_client(
            Client::new(),
            "http://should-not-be-called".to_string(),
            "test-key".to_string(),
            String::new(),
        );
        let market = sample_market();
        let prices = sample_prices();

        // cycle_cost_so_far >= max_cost_per_cycle => should skip
        let result = estimator
            .evaluate(&market, &prices, 0.50, 0.50, None)
            .await
            .unwrap();
        assert!(result.is_none());

        // Also test when over budget
        let result = estimator
            .evaluate(&market, &prices, 1.00, 0.50, None)
            .await
            .unwrap();
        assert!(result.is_none());
    }
}
