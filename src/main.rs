use anyhow::Result;
use tracing::{error, info, warn};

use polymarket_agent::clob_client::ClobClient;
use polymarket_agent::config::Config;
use polymarket_agent::db::Database;
use polymarket_agent::edge_detector::EdgeDetector;
use polymarket_agent::estimator::Estimator;
use polymarket_agent::market_scanner::MarketScanner;
use polymarket_agent::sidecar::SidecarProcess;

#[tokio::main]
async fn main() -> Result<()> {
    // Load configuration
    let config = Config::from_env()?;

    // Initialize logging
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("polymarket_agent=info")),
        )
        .init();

    info!("Polymarket Agent starting in {} mode", config.trading_mode);

    // Open database
    let db = Database::open(&config.database_path)?;
    info!("Database initialized at {}", config.database_path);

    // Spawn Python sidecar (non-fatal if it fails)
    let mut sidecar = match SidecarProcess::spawn(&config).await {
        Ok(s) => {
            info!("Sidecar spawned and healthy");
            Some(s)
        }
        Err(e) => {
            error!("Failed to spawn sidecar (continuing without it): {}", e);
            None
        }
    };

    // Initialize Phase 2 components
    let scanner = MarketScanner::new(&config)?;
    let clob = ClobClient::new(&config.clob_api_url, config.scanner_request_timeout_secs)?;
    let edge_detector = EdgeDetector::new(config.min_edge_threshold);

    // Estimator requires API key — skip analysis if not configured
    let estimator = if config.anthropic_api_key.is_empty() {
        warn!("ANTHROPIC_API_KEY not set — skipping Claude analysis");
        None
    } else {
        match Estimator::new(&config) {
            Ok(e) => Some(e),
            Err(e) => {
                error!("Failed to create Estimator: {}", e);
                None
            }
        }
    };

    // ═══════════════════════════════════════
    // Phase 2: Single analysis cycle
    // (Phase 4 will add the recurring loop)
    // ═══════════════════════════════════════

    info!("Starting analysis cycle");

    // Step 1: Scan and filter markets
    let markets = match scanner.scan_and_filter().await {
        Ok(m) => {
            info!("Found {} candidate markets after filtering", m.len());
            m
        }
        Err(e) => {
            error!("Market scan failed: {}", e);
            vec![]
        }
    };

    // Step 2: Get CLOB prices for each market's YES token
    let mut priced_markets = Vec::new();
    for market in &markets {
        if let Some(yes_token) = market.tokens.iter().find(|t| t.outcome == "Yes") {
            match clob.get_market_prices(&yes_token.token_id, "Yes").await {
                Ok(prices) => priced_markets.push((market.clone(), prices)),
                Err(e) => warn!("Failed to get prices for '{}': {}", market.question, e),
            }
        }
    }
    info!("Got CLOB prices for {} markets", priced_markets.len());

    // Step 3: Claude analysis (if estimator available)
    let mut cycle_cost = 0.0_f64;
    let mut analyses = Vec::new();

    if let Some(ref estimator) = estimator {
        for (market, prices) in &priced_markets {
            match estimator
                .evaluate(market, prices, cycle_cost, config.max_api_cost_per_cycle)
                .await
            {
                Ok(Some(result)) => {
                    cycle_cost += result.total_cost;
                    // Log API costs to database
                    for call in &result.api_calls {
                        if let Err(e) = db.log_api_cost(
                            1, // cycle number (hardcoded until Phase 4)
                            Some(&result.market_id),
                            &call.model,
                            call.input_tokens,
                            call.output_tokens,
                            call.cost_usd,
                            if call.model.contains("haiku") {
                                "triage"
                            } else {
                                "analysis"
                            },
                        ) {
                            warn!("Failed to log API cost: {}", e);
                        }
                    }
                    analyses.push(result);
                }
                Ok(None) => {} // Skipped (triage rejected or budget exhausted)
                Err(e) => warn!("Analysis failed for '{}': {}", market.question, e),
            }
        }
    }

    info!(
        "Analyzed {} markets, total API cost: ${:.4}",
        analyses.len(),
        cycle_cost,
    );

    // Step 4: Edge detection
    let opportunities = edge_detector.detect_batch(&analyses);
    for opp in &opportunities {
        info!(
            "OPPORTUNITY: {} {} @ {:.1}% edge (est={:.2}, mkt={:.2}, conf={:.2})",
            opp.side,
            opp.question,
            opp.edge * 100.0,
            opp.estimated_probability,
            opp.market_price,
            opp.confidence,
        );
    }

    // Step 5: Log cycle summary
    if let Err(e) = db.conn.execute(
        "INSERT INTO cycle_log (cycle_number, markets_scanned, markets_filtered, trades_placed, api_cost_usd) VALUES (?1, ?2, ?3, 0, ?4)",
        rusqlite::params![1, markets.len() as i64, analyses.len() as i64, cycle_cost],
    ) {
        warn!("Failed to log cycle summary: {}", e);
    }

    // Shutdown
    if let Some(ref mut s) = sidecar {
        s.shutdown();
    }
    info!(
        "Phase 2 cycle complete. {} opportunities found from {} markets.",
        opportunities.len(),
        markets.len(),
    );

    Ok(())
}
