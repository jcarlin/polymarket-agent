use anyhow::Result;
use chrono::{NaiveDate, Timelike, Utc};
use futures::{stream, StreamExt};
use std::collections::HashMap;
use std::time::Duration;
use tracing::{error, info, warn};

use polymarket_agent::accounting::Accountant;
use polymarket_agent::clob_client::ClobClient;
use polymarket_agent::config::Config;
use polymarket_agent::dashboard;
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
    WEATHER_CITY_CODES,
};
use polymarket_agent::websocket::{new_event_channel, DashboardEvent};

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
    info!(
        "Config: weather_daily_loss_limit=${:.2}, max_total_weather_exposure={:.0}%, kelly_fraction={}, trading_mode={}",
        config.weather_daily_loss_limit,
        config.max_total_weather_exposure_pct * 100.0,
        config.kelly_fraction,
        config.trading_mode,
    );

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

    // Start dashboard (Phase 7)
    let event_tx = new_event_channel();
    {
        let config_clone = config.clone();
        let event_tx_clone = event_tx.clone();
        tokio::spawn(async move {
            if let Err(e) = dashboard::start_dashboard(&config_clone, event_tx_clone).await {
                error!("Dashboard server failed: {}", e);
            }
        });
        info!("Dashboard spawned on port {}", config.dashboard_port);
    }

    // Initialize components
    let scanner = MarketScanner::new(&config)?;
    let clob = ClobClient::new(&config.clob_api_url, config.scanner_request_timeout_secs)?;
    let edge_detector = EdgeDetector::new(config.min_edge_threshold, config.trading_fee_rate);
    let _position_sizer = PositionSizer::new(
        config.kelly_fraction,
        config.max_position_pct,
        config.max_total_exposure_pct,
        config.trading_fee_rate,
    );
    let executor = Executor::new(
        &config.sidecar_url(),
        config.trading_mode.clone(),
        config.executor_request_timeout_secs,
        config.trading_fee_rate,
    )?;

    // Initialize position manager (Phase 6)
    let position_manager = PositionManager::new(
        config.stop_loss_pct,
        config.take_profit_pct,
        config.min_exit_edge,
        config.volume_spike_factor,
        config.whale_move_threshold,
        config.max_correlated_exposure_pct,
        config.max_total_weather_exposure_pct,
        config.trading_fee_rate,
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

    // Bootstrap calibration: backfill historical weather data if DB is sparse
    if let Some(ref wc) = weather_client {
        let actuals_count = db.count_weather_actuals()?;
        if actuals_count < 100 {
            info!(
                "Insufficient calibration data ({} rows, need ~100), running backfill...",
                actuals_count,
            );
            match wc.backfill(10).await {
                Ok(rows) => {
                    info!("Backfill complete: {} rows inserted", rows);
                    match wc.trigger_calibration().await {
                        Ok(n) => info!("Post-backfill calibration: {} cities calibrated", n),
                        Err(e) => warn!("Post-backfill calibration failed: {}", e),
                    }
                }
                Err(e) => warn!("Backfill failed (non-fatal, will retry next startup): {}", e),
            }
        } else {
            info!(
                "Calibration data sufficient ({} rows), skipping backfill",
                actuals_count
            );
        }
    }

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
    let mut cycles_run = 0u64;

    info!(
        "Starting recurring loop at cycle {} (bankroll: ${:.2}){}",
        cycle_number,
        db.get_current_bankroll()?,
        config
            .max_cycles
            .map_or(String::new(), |n| format!(", max {} cycles", n)),
    );

    // ═══════════════════════════════════════
    // Recurring trading loop
    // ═══════════════════════════════════════
    loop {
        let cycle_start = tokio::time::Instant::now();
        info!("═══ Cycle {} starting ═══", cycle_number);

        // Step 1: Scan and filter markets
        let markets = if config.scanner_weather_only {
            // Weather-only mode: single tag-based query for all weather events
            match scanner.scan_weather_events(WEATHER_CITY_CODES, 7).await {
                Ok(m) => {
                    let filtered = scanner.filter_markets(m);
                    info!(
                        "Weather-only scan: {} markets after filtering",
                        filtered.len()
                    );
                    filtered
                }
                Err(e) => {
                    error!("Weather scan failed: {}", e);
                    vec![]
                }
            }
        } else {
            match scanner.scan_and_filter().await {
                Ok(m) => {
                    info!("Found {} candidate markets after filtering", m.len());
                    m
                }
                Err(e) => {
                    error!("Market scan failed: {}", e);
                    vec![]
                }
            }
        };

        // Step 1.5: Persist scanned markets to DB (satisfies FK constraints for trades)
        for market in &markets {
            if let Err(e) = db.upsert_market(market) {
                warn!("Failed to upsert market '{}': {}", market.question, e);
            }
        }

        // Step 2: Get CLOB prices for each market's YES token (concurrent, capped at 5)
        let priced_markets: Vec<(GammaMarket, _)> = stream::iter(
            markets
                .iter()
                .filter_map(|market| {
                    market
                        .tokens
                        .iter()
                        .find(|t| t.outcome == "Yes")
                        .map(|yes_token| (market.clone(), yes_token.token_id.clone()))
                })
                .collect::<Vec<_>>(),
        )
        .map(|(market, token_id)| {
            let clob = &clob;
            async move {
                match clob.get_market_prices(&token_id, "Yes").await {
                    Ok(prices) => Some((market, prices)),
                    Err(e) => {
                        warn!("Failed to get prices for '{}': {}", market.question, e);
                        None
                    }
                }
            }
        })
        .buffer_unordered(5)
        .filter_map(|x| async { x })
        .collect()
        .await;
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
                        let today_str = Utc::now().date_naive().format("%Y-%m-%d").to_string();
                        let is_same_day = info.date == today_str;
                        match wc.get_probabilities(&info.city, &info.date, is_same_day).await {
                            Ok(probs) => {
                                info!(
                                    "Weather {}/{}: ensemble={:.1}°F, NWS={}, WU_fcst={}, WU_actual={}{} → to_llm={:.1}°F | std={:.1}°F, {} members, cal_bias={}",
                                    info.city, info.date,
                                    probs.raw_ensemble_mean,
                                    probs.nws_forecast_high.map_or("n/a".to_string(), |h| format!("{:.0}°F", h)),
                                    probs.wu_forecast_high.map_or("n/a".to_string(), |h| format!("{:.0}°F", h)),
                                    probs.wu_high.map_or("n/a".to_string(), |h| format!("{:.0}°F", h)),
                                    probs.hrrr_max_temp.map_or(String::new(), |h| format!(", HRRR={:.0}°F", h)),
                                    probs.ensemble_mean,
                                    probs.ensemble_std,
                                    probs.gefs_count + probs.ecmwf_count + probs.icon_count + probs.gem_count,
                                    probs.calibration_bias.map_or("n/a".to_string(), |b| format!("{:+.1}°F", b)),
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
            // Persist weather snapshots to DB for dashboard
            for ((city, date), probs) in &weather_cache {
                let bucket_json = serde_json::to_string(
                    &probs
                        .buckets
                        .iter()
                        .map(|b| {
                            serde_json::json!({
                                "label": b.bucket_label,
                                "lower": b.lower,
                                "upper": b.upper,
                                "probability": b.probability,
                            })
                        })
                        .collect::<Vec<_>>(),
                )
                .unwrap_or_default();
                if let Err(e) = db.insert_weather_snapshot(
                    cycle_number,
                    city,
                    date,
                    probs.ensemble_mean,
                    probs.ensemble_std,
                    probs.gefs_count as i32,
                    probs.ecmwf_count as i32,
                    &bucket_json,
                ) {
                    warn!("Failed to insert weather snapshot: {}", e);
                }
            }

            // Collect WU actuals for past-date weather markets (resolution data)
            if let Some(ref wc) = weather_client {
                let today = Utc::now().date_naive();
                for ((city, date), probs) in &weather_cache {
                    if let Ok(forecast_date) = NaiveDate::parse_from_str(date, "%Y-%m-%d") {
                        if forecast_date < today {
                            match wc
                                .collect_wu_actual(
                                    city,
                                    date,
                                    Some(probs.ensemble_mean),
                                    probs.nws_forecast_high,
                                )
                                .await
                            {
                                Ok(Some(wu_high)) => {
                                    info!(
                                        "WU actual {}/{}: {:.0}°F (our forecast={:.1}°F, gap={:+.1}°F)",
                                        city, date, wu_high, probs.ensemble_mean,
                                        probs.ensemble_mean - wu_high,
                                    );
                                }
                                Ok(None) => {
                                    warn!("WU actual not available for {}/{}", city, date);
                                }
                                Err(e) => {
                                    warn!("WU collect failed for {}/{}: {}", city, date, e);
                                }
                            }
                        }
                    }
                }
            }
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
            // Persist opportunity (initially as 'pending', updated below)
            if let Err(e) = db.insert_opportunity(
                cycle_number,
                &opp.market_id,
                &opp.question,
                &opp.side.to_string(),
                opp.market_price,
                opp.estimated_probability,
                opp.edge,
                opp.confidence,
                "pending",
                None,
            ) {
                warn!("Failed to insert opportunity: {}", e);
            }
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
            config.trading_fee_rate,
        );

        // Get current open positions for correlation checks
        let open_positions = db.get_open_positions_with_market().unwrap_or_default();

        // Daily weather loss circuit breaker
        let weather_losses_today = db.get_weather_losses_today();
        let weather_breaker_active = weather_losses_today >= config.weather_daily_loss_limit;
        if weather_breaker_active {
            info!(
                "Weather daily loss circuit breaker ACTIVE: ${:.2} lost today >= ${:.2} limit",
                weather_losses_today, config.weather_daily_loss_limit,
            );
        }

        for opp in &opportunities {
            let is_weather_opp = parse_weather_market(&opp.question).is_some();

            // Daily loss breaker: skip all weather opportunities this cycle
            if is_weather_opp && weather_breaker_active {
                info!(
                    "Skipping {} — weather daily loss circuit breaker active",
                    opp.question,
                );
                let _ = db.conn.execute(
                    "UPDATE cycle_opportunities SET status = 'rejected', reject_reason = 'daily_loss_limit' WHERE cycle_number = ?1 AND condition_id = ?2 AND status = 'pending'",
                    rusqlite::params![cycle_number, opp.market_id],
                );
                continue;
            }

            let token_id = match find_token_id(&markets, &opp.market_id, &opp.side) {
                Some(id) => id,
                None => {
                    warn!(
                        "No token_id found for {} side of {}",
                        opp.side, opp.market_id
                    );
                    let _ = db.conn.execute(
                        "UPDATE cycle_opportunities SET status = 'skipped', reject_reason = 'no_token_id' WHERE cycle_number = ?1 AND condition_id = ?2 AND status = 'pending'",
                        rusqlite::params![cycle_number, opp.market_id],
                    );
                    continue;
                }
            };

            // No re-buy: skip if already positioned in this market (weather-only)
            if is_weather_opp && db.has_open_position(&opp.market_id) {
                info!("Skipping {} — already have open position", opp.question,);
                let _ = db.conn.execute(
                    "UPDATE cycle_opportunities SET status = 'rejected', reject_reason = 'already_positioned' WHERE cycle_number = ?1 AND condition_id = ?2 AND status = 'pending'",
                    rusqlite::params![cycle_number, opp.market_id],
                );
                continue;
            }

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
                let _ = db.conn.execute(
                    "UPDATE cycle_opportunities SET status = 'rejected', reject_reason = 'correlation_limit' WHERE cycle_number = ?1 AND condition_id = ?2 AND status = 'pending'",
                    rusqlite::params![cycle_number, opp.market_id],
                );
                continue;
            }

            // Check total weather exposure cap
            if is_weather_opp
                && position_manager.is_total_weather_over_limit(&open_positions, bankroll)
            {
                info!(
                    "Skipping {} — total weather exposure over {:.0}% limit",
                    opp.question,
                    config.max_total_weather_exposure_pct * 100.0,
                );
                let _ = db.conn.execute(
                    "UPDATE cycle_opportunities SET status = 'rejected', reject_reason = 'weather_exposure_limit' WHERE cycle_number = ?1 AND condition_id = ?2 AND status = 'pending'",
                    rusqlite::params![cycle_number, opp.market_id],
                );
                continue;
            }

            // Time-based sizing for weather markets
            let days_until = if is_weather_opp {
                parse_weather_market(&opp.question).and_then(|info| {
                    NaiveDate::parse_from_str(&info.date, "%Y-%m-%d")
                        .ok()
                        .map(|d| {
                            let today = Utc::now().date_naive();
                            (d - today).num_days()
                        })
                })
            } else {
                None
            };

            let sizing = effective_sizer.size_position_with_time(
                opp,
                bankroll,
                current_exposure,
                days_until,
            );
            if sizing.is_rejected() {
                info!(
                    "Skipping {}: {}",
                    opp.question,
                    sizing.reject_reason.as_deref().unwrap_or("unknown"),
                );
                let _ = db.conn.execute(
                    "UPDATE cycle_opportunities SET status = 'rejected', reject_reason = ?1 WHERE cycle_number = ?2 AND condition_id = ?3 AND status = 'pending'",
                    rusqlite::params![sizing.reject_reason.as_deref().unwrap_or("sizing_rejected"), cycle_number, opp.market_id],
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
                    // Mark opportunity as executed
                    let _ = db.conn.execute(
                        "UPDATE cycle_opportunities SET status = 'executed' WHERE cycle_number = ?1 AND condition_id = ?2 AND status = 'pending'",
                        rusqlite::params![cycle_number, opp.market_id],
                    );
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
                    let _ = event_tx.send(DashboardEvent::TradeExecuted {
                        trade_id: result.trade_id.clone(),
                        market_id: result.market_condition_id.clone(),
                        side: result.side.to_string(),
                        price: result.price,
                        size: sizing.position_usd,
                        paper: result.paper,
                    });
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
                    let _ = db.conn.execute(
                        "UPDATE cycle_opportunities SET status = 'rejected', reject_reason = 'execution_failed' WHERE cycle_number = ?1 AND condition_id = ?2 AND status = 'pending'",
                        rusqlite::params![cycle_number, opp.market_id],
                    );
                }
            }
        }

        // Step 5.5: Position management — check stop-loss, take-profit, edge decay
        if config.position_check_enabled {
            match position_manager
                .check_positions(&db, &clob, cycle_number, weather_client.as_ref())
                .await
            {
                Ok(mgmt_result) => {
                    for (pos, reason) in &mgmt_result.exits_triggered {
                        let exit_price = pos.current_price.unwrap_or(pos.entry_price);
                        match executor.exit_position(&db, pos, exit_price).await {
                            Ok(pnl) => {
                                let _ = event_tx.send(DashboardEvent::PositionExit {
                                    market_id: pos.market_condition_id.clone(),
                                    side: pos.side.clone(),
                                    exit_price,
                                    pnl,
                                    reason: reason.clone(),
                                });
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
                        let _ = event_tx.send(DashboardEvent::PositionAlert {
                            market_id: alert.market_condition_id.clone(),
                            alert_type: alert.alert_type.clone(),
                            details: alert.details.clone(),
                        });
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

        // Step 5.6: Daily WU actual collection & calibration
        // Once per day (first cycle after midnight UTC), collect yesterday's actuals
        if let Some(ref wc) = weather_client {
            let now = Utc::now();
            // Run daily collection at midnight UTC (hour 0, first cycle)
            if now.hour() == 0 && cycle_number > 1 {
                let yesterday = (now - chrono::Duration::days(1))
                    .format("%Y-%m-%d")
                    .to_string();
                match wc.collect_actuals_batch(Some(&yesterday)).await {
                    Ok(n) => info!("Daily WU collection: {} cities collected for {}", n, yesterday),
                    Err(e) => warn!("Daily WU collection failed: {}", e),
                }
                // Trigger calibration after collecting actuals
                match wc.trigger_calibration().await {
                    Ok(n) => info!("Daily calibration: {} cities calibrated", n),
                    Err(e) => warn!("Daily calibration failed: {}", e),
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

        let _ = event_tx.send(DashboardEvent::CycleComplete {
            cycle_number,
            bankroll: accounting.bankroll_after,
            exposure: db.get_total_exposure().unwrap_or(0.0),
            trades_placed,
            api_cost: accounting.api_cost,
            positions_checked: if config.position_check_enabled {
                db.get_open_positions().map(|p| p.len() as u32).unwrap_or(0)
            } else {
                0
            },
        });

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
        cycles_run += 1;

        if config.max_cycles.is_some_and(|max| cycles_run >= max) {
            info!("Reached MAX_CYCLES={} — shutting down", cycles_run);
            if let Some(ref mut s) = sidecar {
                s.shutdown();
            }
            return Ok(());
        }

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
