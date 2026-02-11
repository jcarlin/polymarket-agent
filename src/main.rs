use anyhow::Result;
use std::collections::HashMap;
use std::time::Duration;
use tracing::{error, info, warn};

use polymarket_agent::accounting::Accountant;
use polymarket_agent::clob_client::ClobClient;
use polymarket_agent::config::Config;
use polymarket_agent::db::Database;
use polymarket_agent::edge_detector::{EdgeDetector, TradeSide};
use polymarket_agent::estimator::{Estimator, WeatherContext};
use polymarket_agent::executor::{Executor, TradeIntent};
use polymarket_agent::market_scanner::{GammaMarket, MarketScanner};
use polymarket_agent::position_manager::PositionManager;
use polymarket_agent::position_sizer::PositionSizer;
use polymarket_agent::sidecar::SidecarProcess;
use polymarket_agent::weather_client::{
    get_weather_model_probability, parse_weather_market, WeatherClient, WeatherProbabilities,
};

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
    let _position_sizer = PositionSizer::new(
        config.kelly_fraction,
        config.max_position_pct,
        config.max_total_exposure_pct,
    );
    let executor = Executor::new(
        &config.sidecar_url(),
        config.trading_mode.clone(),
        config.executor_request_timeout_secs,
    )?;

    // Initialize position manager (Phase 6)
    let position_manager = PositionManager::new(
        config.stop_loss_pct,
        config.take_profit_pct,
        config.min_exit_edge,
        config.volume_spike_factor,
        config.whale_move_threshold,
        config.max_correlated_exposure_pct,
    );

    // Initialize weather client (uses sidecar endpoint)
    let weather_client = match WeatherClient::new(
        &config.sidecar_url(),
        config.executor_request_timeout_secs,
        2,
    ) {
        Ok(wc) => {
            info!("Weather client initialized");
            Some(wc)
        }
        Err(e) => {
            warn!("Failed to create weather client: {}", e);
            None
        }
    };

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

        // Step 2.5: Fetch weather data for weather markets
        let mut weather_cache: HashMap<(String, String), WeatherProbabilities> = HashMap::new();
        if let Some(ref wc) = weather_client {
            for (market, _) in &priced_markets {
                if let Some(info) = parse_weather_market(&market.question) {
                    let key = (info.city.clone(), info.date.clone());
                    if let std::collections::hash_map::Entry::Vacant(entry) =
                        weather_cache.entry(key)
                    {
                        match wc.get_probabilities(&info.city, &info.date).await {
                            Ok(probs) => {
                                info!(
                                    "Weather data for {}/{}: mean={:.1}°F, std={:.1}°F",
                                    info.city, info.date, probs.ensemble_mean, probs.ensemble_std
                                );
                                entry.insert(probs);
                            }
                            Err(err) => {
                                warn!(
                                    "Weather fetch failed for {}/{}: {}",
                                    info.city, info.date, err
                                );
                            }
                        }
                    }
                }
            }
        }
        if !weather_cache.is_empty() {
            info!(
                "Fetched weather data for {} city/date combinations",
                weather_cache.len()
            );
        }

        // Step 3: Claude analysis (if estimator available)
        let mut cycle_cost = 0.0_f64;
        let mut analyses = Vec::new();

        if let Some(ref estimator) = estimator {
            for (market, prices) in &priced_markets {
                // Build weather context if available for this market
                let weather_ctx = parse_weather_market(&market.question).and_then(|info| {
                    let key = (info.city.clone(), info.date.clone());
                    weather_cache.get(&key).map(|probs| {
                        let model_prob = get_weather_model_probability(&info, probs);
                        WeatherContext {
                            probs,
                            model_probability: model_prob,
                        }
                    })
                });

                match estimator
                    .evaluate(
                        market,
                        prices,
                        cycle_cost,
                        config.max_api_cost_per_cycle,
                        weather_ctx.as_ref(),
                    )
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

        // Check drawdown state for sizing adjustment
        let drawdown_state = match PositionManager::check_drawdown(
            &db,
            bankroll,
            config.drawdown_circuit_breaker_pct,
        ) {
            Ok(state) => Some(state),
            Err(e) => {
                warn!("Drawdown check failed: {}", e);
                None
            }
        };

        // Adjust kelly fraction during drawdown
        let effective_kelly = if drawdown_state
            .as_ref()
            .is_some_and(|s| s.is_circuit_breaker_active)
        {
            let reduced = config.kelly_fraction * config.drawdown_sizing_reduction;
            info!(
                "Drawdown active — reducing Kelly from {:.2} to {:.2}",
                config.kelly_fraction, reduced,
            );
            reduced
        } else {
            config.kelly_fraction
        };

        let effective_sizer = PositionSizer::new(
            effective_kelly,
            config.max_position_pct,
            config.max_total_exposure_pct,
        );

        // Get current open positions for correlation checks
        let open_positions = db.get_open_positions_with_market().unwrap_or_default();

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

            // Check correlation limit before sizing
            if position_manager.is_correlated_group_over_limit(
                &opp.question,
                &open_positions,
                bankroll,
            ) {
                info!(
                    "Skipping {} — correlated weather group over exposure limit",
                    opp.question,
                );
                continue;
            }

            let sizing = effective_sizer.size_position(opp, bankroll, current_exposure);
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
                token_id: token_id.clone(),
                sizing: sizing.clone(),
            };

            match executor.execute(&intent, &db).await {
                Ok(result) => {
                    // Store estimated_probability with the position
                    if let Err(e) = db.upsert_position_with_estimate(
                        &result.market_condition_id,
                        &result.token_id,
                        &result.side.to_string(),
                        result.price,
                        result.size,
                        Some(opp.estimated_probability),
                    ) {
                        warn!("Failed to update position with estimate: {}", e);
                    }

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

        // Step 5.5: Position management — check stop-loss, take-profit, edge decay
        if config.position_check_enabled {
            match position_manager
                .check_positions(&db, &clob, cycle_number)
                .await
            {
                Ok(mgmt_result) => {
                    for (pos, reason) in &mgmt_result.exits_triggered {
                        let exit_price = pos.current_price.unwrap_or(pos.entry_price);
                        match executor.exit_position(&db, pos, exit_price).await {
                            Ok(pnl) => {
                                info!(
                                    "Position exit: {} {} pnl=${:.2} ({})",
                                    pos.side, pos.market_condition_id, pnl, reason,
                                );
                            }
                            Err(e) => {
                                warn!(
                                    "Failed to exit {} {}: {}",
                                    pos.side, pos.market_condition_id, e,
                                );
                            }
                        }
                    }

                    // Log correlation alerts
                    let corr_alerts = position_manager.check_correlated_exposure(
                        &db.get_open_positions_with_market().unwrap_or_default(),
                        db.get_current_bankroll()?,
                    );
                    for alert in &corr_alerts {
                        warn!("CORRELATION ALERT: {}", alert.details);
                        let _ = db.log_position_alert(
                            &alert.market_condition_id,
                            &alert.alert_type,
                            &alert.details,
                            &alert.action_taken,
                            cycle_number,
                        );
                    }

                    if mgmt_result.positions_checked > 0 {
                        info!(
                            "Position management: {} checked, {} exits, {} re-analyze",
                            mgmt_result.positions_checked,
                            mgmt_result.exits_triggered.len(),
                            mgmt_result.re_analyses_triggered,
                        );
                    }
                }
                Err(e) => {
                    warn!("Position management check failed: {}", e);
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
