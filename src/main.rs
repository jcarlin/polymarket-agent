use anyhow::Result;
use std::time::Duration;
use tracing::{error, info, warn};

use polymarket_agent::accounting::Accountant;
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

    // Seed bankroll if first run
    db.ensure_bankroll_seeded(config.initial_bankroll)?;

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

    // Initialize components
    let scanner = MarketScanner::new(&config)?;
    let clob = ClobClient::new(&config.clob_api_url, config.scanner_request_timeout_secs)?;
    let edge_detector = EdgeDetector::new(config.min_edge_threshold);
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

    let accountant = Accountant::new(config.low_bankroll_threshold);
    let mut cycle_number = db.get_next_cycle_number()?;

    info!(
        "Starting recurring loop at cycle {} (bankroll: ${:.2})",
        cycle_number,
        db.get_current_bankroll()?,
    );

    // ═══════════════════════════════════════
    // Recurring trading loop
    // ═══════════════════════════════════════
    loop {
        let cycle_start = tokio::time::Instant::now();
        info!("═══ Cycle {} starting ═══", cycle_number);

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
                        for call in &result.api_calls {
                            if let Err(e) = db.log_api_cost(
                                cycle_number,
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

        // Step 5: Position sizing & execution
        let mut bankroll = db.get_current_bankroll()?;
        let mut current_exposure = db.get_total_exposure()?;
        let mut trades_placed = 0u32;

        for opp in &opportunities {
            let token_id = match find_token_id(&markets, &opp.market_id, &opp.side) {
                Some(id) => id,
                None => {
                    warn!(
                        "No token_id found for {} side of {}",
                        opp.side, opp.market_id
                    );
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
                        trades_placed,
                        result.side,
                        result.market_condition_id,
                        result.price,
                        sizing.position_usd,
                        bankroll,
                    );
                }
                Err(e) => {
                    warn!("Trade execution failed for '{}': {}", opp.question, e);
                }
            }
        }

        // Step 6: Close cycle — deduct API costs, check survival
        let bankroll_before = db.get_current_bankroll()?;
        let accounting = accountant.close_cycle(&db, cycle_number)?;

        // Log cycle summary
        if let Err(e) = db.conn.execute(
            "INSERT INTO cycle_log (cycle_number, markets_scanned, markets_filtered, trades_placed, api_cost_usd, bankroll_before, bankroll_after) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![
                cycle_number,
                markets.len() as i64,
                analyses.len() as i64,
                trades_placed as i64,
                accounting.api_cost,
                bankroll_before,
                accounting.bankroll_after,
            ],
        ) {
            warn!("Failed to log cycle summary: {}", e);
        }

        info!(
            "═══ Cycle {} complete: {} trades, API cost ${:.4}, bankroll ${:.2} → ${:.2} ═══",
            cycle_number,
            trades_placed,
            accounting.api_cost,
            accounting.bankroll_before,
            accounting.bankroll_after,
        );

        // Death check
        if !accounting.is_alive {
            error!("BANKROLL DEPLETED — agent is dying");
            let report = accountant.generate_death_report(&db)?;
            report.display();
            if let Some(ref mut s) = sidecar {
                s.shutdown();
            }
            std::process::exit(config.death_exit_code);
        }

        cycle_number += 1;

        // Adaptive sleep — shorter cycles when bankroll is high
        let target_secs = accountant.get_cycle_duration_secs(
            accounting.bankroll_after,
            config.cycle_frequency_high_secs,
            config.cycle_frequency_low_secs,
        );
        let elapsed = cycle_start.elapsed();
        let sleep_duration = Duration::from_secs(target_secs).saturating_sub(elapsed);

        if !sleep_duration.is_zero() {
            info!(
                "Sleeping {:.0}s until next cycle (target: {}s)",
                sleep_duration.as_secs_f64(),
                target_secs,
            );
        }

        // Wait for sleep OR Ctrl+C — whichever comes first
        tokio::select! {
            _ = tokio::time::sleep(sleep_duration) => {}
            _ = tokio::signal::ctrl_c() => {
                info!("Ctrl+C received — shutting down gracefully");
                if let Some(ref mut s) = sidecar {
                    s.shutdown();
                }
                return Ok(());
            }
        }
    }
}
