use anyhow::Result;
use tracing::{error, info, warn};

use polymarket_agent::clob_client::ClobClient;
use polymarket_agent::config::Config;
use polymarket_agent::db::Database;
use polymarket_agent::edge_detector::{EdgeDetector, TradeSide};
use polymarket_agent::estimator::Estimator;
use polymarket_agent::executor::{Executor, TradeIntent};
use polymarket_agent::market_scanner::{GammaMarket, MarketScanner};
use polymarket_agent::position_sizer::PositionSizer;
use polymarket_agent::sidecar::SidecarProcess;

/// Look up the token_id for a given market condition_id and trade side.
fn find_token_id(markets: &[GammaMarket], condition_id: &str, side: &TradeSide) -> Option<String> {
    let target_outcome = match side {
        TradeSide::Yes => "Yes",
        TradeSide::No => "No",
    };
    markets
        .iter()
        .find(|m| m.condition_id.as_deref() == Some(condition_id))
        .and_then(|m| {
            m.tokens
                .iter()
                .find(|t| t.outcome == target_outcome)
                .map(|t| t.token_id.clone())
        })
}

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

    // Step 5: Position sizing & execution (Phase 3)
    let position_sizer = PositionSizer::new(
        config.kelly_fraction,
        config.max_position_pct,
        config.max_total_exposure_pct,
    );
    let executor = Executor::new(
        &config.sidecar_url(),
        config.trading_mode.clone(),
        config.executor_request_timeout_secs,
    )?;

    // Seed bankroll if first run
    db.ensure_bankroll_seeded(config.initial_bankroll)?;
    let mut bankroll = db.get_current_bankroll()?;
    let mut current_exposure = db.get_total_exposure()?;
    let mut trades_placed = 0u32;

    for opp in &opportunities {
        // Look up the token_id from the market data
        let token_id = match find_token_id(&markets, &opp.market_id, &opp.side) {
            Some(id) => id,
            None => {
                warn!("No token_id found for {} side of {}", opp.side, opp.market_id);
                continue;
            }
        };

        let sizing = position_sizer.size_position(opp, bankroll, current_exposure);
        if sizing.is_rejected() {
            info!(
                "Skipping {}: {}",
                opp.question,
                sizing.reject_reason.as_deref().unwrap_or("unknown"),
            );
            continue;
        }

        let intent = TradeIntent {
            opportunity: opp.clone(),
            token_id,
            sizing: sizing.clone(),
        };

        match executor.execute(&intent, &db).await {
            Ok(result) => {
                trades_placed += 1;
                bankroll = db.get_current_bankroll()?;
                current_exposure = db.get_total_exposure()?;
                info!(
                    "Trade #{}: {} {} @ {:.2} (${:.2}), bankroll=${:.2}",
                    trades_placed, result.side, result.market_condition_id,
                    result.price, sizing.position_usd, bankroll,
                );
            }
            Err(e) => {
                warn!("Trade execution failed for '{}': {}", opp.question, e);
            }
        }
    }

    // Step 6: Log cycle summary
    if let Err(e) = db.conn.execute(
        "INSERT INTO cycle_log (cycle_number, markets_scanned, markets_filtered, trades_placed, api_cost_usd) VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![1, markets.len() as i64, analyses.len() as i64, trades_placed as i64, cycle_cost],
    ) {
        warn!("Failed to log cycle summary: {}", e);
    }

    // Shutdown
    if let Some(ref mut s) = sidecar {
        s.shutdown();
    }
    info!(
        "Phase 3 cycle complete. {} trades from {} opportunities from {} markets. Bankroll: ${:.2}",
        trades_placed,
        opportunities.len(),
        markets.len(),
        bankroll,
    );

    Ok(())
}
