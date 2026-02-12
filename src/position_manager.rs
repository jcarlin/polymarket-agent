use anyhow::Result;
use tracing::{info, warn};

use crate::clob_client::ClobClient;
use crate::db::{Database, PositionRow};
use crate::weather_client::{
    get_weather_model_probability, parse_weather_market, WeatherClient,
};

/// Action to take for a position after management checks.
#[derive(Debug, Clone, PartialEq)]
pub enum PositionAction {
    Hold,
    Exit { reason: String },
    ReAnalyze { reason: String },
}

/// Alert generated during position management.
#[derive(Debug, Clone)]
pub struct PositionAlert {
    pub market_condition_id: String,
    pub alert_type: String,
    pub details: String,
    pub action_taken: String,
}

/// Result of a full position management check cycle.
#[derive(Debug)]
pub struct PositionManagementResult {
    pub positions_checked: usize,
    pub exits_triggered: Vec<(PositionRow, String)>,
    pub re_analyses_triggered: usize,
    pub alerts: Vec<PositionAlert>,
}

/// Drawdown state computed from peak vs current bankroll.
#[derive(Debug, Clone)]
pub struct DrawdownState {
    pub peak_bankroll: f64,
    pub current_bankroll: f64,
    pub drawdown_pct: f64,
    pub is_circuit_breaker_active: bool,
}

/// Correlation group for weather markets (nearby cities).
#[derive(Debug, Clone, PartialEq)]
pub struct CorrelationGroup {
    pub name: String,
    pub cities: Vec<String>,
}

pub struct PositionManager {
    pub stop_loss_pct: f64,
    pub take_profit_pct: f64,
    pub min_exit_edge: f64,
    pub volume_spike_factor: f64,
    pub whale_move_threshold: f64,
    pub max_correlated_exposure_pct: f64,
    pub max_total_weather_exposure_pct: f64,
    correlation_groups: Vec<CorrelationGroup>,
}

impl PositionManager {
    pub fn new(
        stop_loss_pct: f64,
        take_profit_pct: f64,
        min_exit_edge: f64,
        volume_spike_factor: f64,
        whale_move_threshold: f64,
        max_correlated_exposure_pct: f64,
        max_total_weather_exposure_pct: f64,
    ) -> Self {
        let correlation_groups = vec![
            CorrelationGroup {
                name: "Northeast".to_string(),
                cities: vec![
                    "NYC".to_string(),
                    "PHL".to_string(),
                    "BOS".to_string(),
                    "DCA".to_string(),
                ],
            },
            CorrelationGroup {
                name: "Southeast".to_string(),
                cities: vec!["MIA".to_string(), "ATL".to_string(), "TPA".to_string()],
            },
            CorrelationGroup {
                name: "Midwest".to_string(),
                cities: vec![
                    "CHI".to_string(),
                    "DTW".to_string(),
                    "MSP".to_string(),
                    "STL".to_string(),
                ],
            },
            CorrelationGroup {
                name: "Texas".to_string(),
                cities: vec!["HOU".to_string(), "DAL".to_string(), "SAN".to_string()],
            },
            CorrelationGroup {
                name: "West Coast".to_string(),
                cities: vec![
                    "LAX".to_string(),
                    "SDG".to_string(),
                    "SJC".to_string(),
                    "SEA".to_string(),
                ],
            },
        ];

        PositionManager {
            stop_loss_pct,
            take_profit_pct,
            min_exit_edge,
            volume_spike_factor,
            whale_move_threshold,
            max_correlated_exposure_pct,
            max_total_weather_exposure_pct,
            correlation_groups,
        }
    }

    /// Run all position management checks for open positions.
    /// `weather_client`: if provided, used to refresh ensemble probabilities for weather positions.
    pub async fn check_positions(
        &self,
        db: &Database,
        clob: &ClobClient,
        cycle_number: i64,
        weather_client: Option<&WeatherClient>,
    ) -> Result<PositionManagementResult> {
        let positions = db.get_open_positions_with_market()?;
        let mut exits_triggered = Vec::new();
        let mut re_analyses_triggered = 0usize;
        let mut alerts = Vec::new();

        for pos in &positions {
            // Fetch current midpoint price from CLOB
            let current_price = match clob.get_midpoint(&pos.token_id).await {
                Ok(price) => {
                    // Update price in DB
                    if let Err(e) =
                        db.update_position_price(&pos.market_condition_id, &pos.side, price)
                    {
                        warn!("Failed to update position price: {}", e);
                    }
                    price
                }
                Err(e) => {
                    warn!(
                        "Failed to get midpoint for {} ({}): {} — skipping checks",
                        pos.market_condition_id, pos.token_id, e
                    );
                    continue;
                }
            };

            // For weather positions, refresh ensemble probability before edge decay check
            let mut pos_refreshed = pos.clone();
            if let (Some(wc), Some(question)) = (weather_client, pos.question.as_ref()) {
                if let Some(info) = parse_weather_market(question) {
                    match wc.get_probabilities(&info.city, &info.date).await {
                        Ok(probs) => {
                            if let Some(fresh_prob) = get_weather_model_probability(&info, &probs) {
                                info!(
                                    "Refreshed weather estimate for {}: {:.3} → {:.3}",
                                    pos.market_condition_id,
                                    pos.estimated_probability.unwrap_or(0.0),
                                    fresh_prob,
                                );
                                pos_refreshed.estimated_probability = Some(fresh_prob);
                                if let Err(e) = db.update_position_estimate(
                                    &pos.market_condition_id,
                                    fresh_prob,
                                ) {
                                    warn!("Failed to update position estimate: {}", e);
                                }
                            }
                        }
                        Err(e) => {
                            warn!(
                                "Weather refresh failed for {}: {} — using stale estimate",
                                pos.market_condition_id, e,
                            );
                        }
                    }
                }
            }

            let action = self.evaluate_position(&pos_refreshed, current_price);

            match action {
                PositionAction::Hold => {}
                PositionAction::Exit { ref reason } => {
                    let alert = PositionAlert {
                        market_condition_id: pos.market_condition_id.clone(),
                        alert_type: "exit".to_string(),
                        details: reason.clone(),
                        action_taken: "exit_triggered".to_string(),
                    };
                    if let Err(e) = db.log_position_alert(
                        &pos.market_condition_id,
                        &alert.alert_type,
                        &alert.details,
                        &alert.action_taken,
                        cycle_number,
                    ) {
                        warn!("Failed to log position alert: {}", e);
                    }
                    alerts.push(alert);
                    exits_triggered.push((pos.clone(), reason.clone()));
                }
                PositionAction::ReAnalyze { ref reason } => {
                    let alert = PositionAlert {
                        market_condition_id: pos.market_condition_id.clone(),
                        alert_type: "re_analyze".to_string(),
                        details: reason.clone(),
                        action_taken: "logged".to_string(),
                    };
                    if let Err(e) = db.log_position_alert(
                        &pos.market_condition_id,
                        &alert.alert_type,
                        &alert.details,
                        &alert.action_taken,
                        cycle_number,
                    ) {
                        warn!("Failed to log position alert: {}", e);
                    }
                    alerts.push(alert);
                    re_analyses_triggered += 1;
                }
            }
        }

        info!(
            "Position management: {} checked, {} exits, {} re-analyze alerts",
            positions.len(),
            exits_triggered.len(),
            re_analyses_triggered,
        );

        Ok(PositionManagementResult {
            positions_checked: positions.len(),
            exits_triggered,
            re_analyses_triggered,
            alerts,
        })
    }

    /// Evaluate a single position and decide what action to take.
    pub fn evaluate_position(&self, pos: &PositionRow, current_price: f64) -> PositionAction {
        let is_weather = pos
            .question
            .as_ref()
            .is_some_and(|q| parse_weather_market(q).is_some());

        // Weather markets: skip price-based stop-loss and take-profit.
        // These are small binary bets that resolve in days — hold to resolution.
        // Only exit on edge decay (i.e., new ensemble forecast changes our model probability).
        if !is_weather {
            // Stop-loss check: position value dropped too much
            if let Some(action) = self.check_stop_loss(pos, current_price) {
                return action;
            }

            // Take-profit check: captured enough of expected value
            if let Some(action) = self.check_take_profit(pos, current_price) {
                return action;
            }
        }

        // Edge decay check: exit if model-based edge has shrunk below minimum
        if let Some(action) = self.check_edge_decay(pos, current_price) {
            return action;
        }

        PositionAction::Hold
    }

    /// Stop-loss: exit if position is down more than stop_loss_pct from entry.
    fn check_stop_loss(&self, pos: &PositionRow, current_price: f64) -> Option<PositionAction> {
        if pos.entry_price <= 0.0 {
            return None;
        }

        let loss_pct = (pos.entry_price - current_price) / pos.entry_price;

        if loss_pct > self.stop_loss_pct {
            Some(PositionAction::Exit {
                reason: format!(
                    "Stop-loss: down {:.1}% (entry={:.3}, current={:.3}, threshold={:.1}%)",
                    loss_pct * 100.0,
                    pos.entry_price,
                    current_price,
                    self.stop_loss_pct * 100.0,
                ),
            })
        } else {
            None
        }
    }

    /// Take-profit: exit if we've captured enough of the expected value.
    /// For a binary market position bought at entry_price, max profit is (1.0 - entry_price).
    /// We exit when (current - entry) / (1.0 - entry) >= take_profit_pct.
    fn check_take_profit(&self, pos: &PositionRow, current_price: f64) -> Option<PositionAction> {
        if pos.entry_price >= 1.0 {
            return None;
        }

        let max_profit = 1.0 - pos.entry_price;
        if max_profit <= 0.0 {
            return None;
        }

        let current_profit = current_price - pos.entry_price;
        let captured_pct = current_profit / max_profit;

        if captured_pct >= self.take_profit_pct {
            Some(PositionAction::Exit {
                reason: format!(
                    "Take-profit: captured {:.1}% of max (entry={:.3}, current={:.3}, threshold={:.1}%)",
                    captured_pct * 100.0,
                    pos.entry_price,
                    current_price,
                    self.take_profit_pct * 100.0,
                ),
            })
        } else {
            None
        }
    }

    /// Edge decay: if we stored the estimated probability at entry, check if the
    /// current edge has fallen below min_exit_edge.
    fn check_edge_decay(&self, pos: &PositionRow, current_price: f64) -> Option<PositionAction> {
        let estimated_prob = pos.estimated_probability?;

        // Compute current edge the same way as at entry
        let current_edge = (estimated_prob - current_price).abs();

        if current_edge < self.min_exit_edge {
            Some(PositionAction::Exit {
                reason: format!(
                    "Edge decay: edge={:.1}% < threshold {:.1}% (est={:.3}, current={:.3})",
                    current_edge * 100.0,
                    self.min_exit_edge * 100.0,
                    estimated_prob,
                    current_price,
                ),
            })
        } else {
            None
        }
    }

    /// Check if a volume spike warrants re-analysis.
    pub fn check_volume_spike(&self, current_volume: f64, avg_volume: f64) -> bool {
        if avg_volume <= 0.0 {
            return false;
        }
        current_volume / avg_volume > self.volume_spike_factor
    }

    /// Whale monitoring — stub. Returns no alerts.
    pub fn check_whale_activity(&self, _market_condition_id: &str) -> Vec<PositionAlert> {
        // Whale monitoring deferred to Phase 6.5 — requires Polygon RPC integration
        Vec::new()
    }

    /// Check correlated exposure across weather market groups.
    /// Returns alerts for any groups exceeding the max correlated exposure limit.
    pub fn check_correlated_exposure(
        &self,
        positions: &[PositionRow],
        bankroll: f64,
    ) -> Vec<PositionAlert> {
        if bankroll <= 0.0 {
            return Vec::new();
        }

        let max_group_exposure = self.max_correlated_exposure_pct * bankroll;
        let mut alerts = Vec::new();

        for group in &self.correlation_groups {
            let group_exposure: f64 = positions
                .iter()
                .filter(|p| {
                    p.question.as_ref().is_some_and(|q| {
                        parse_weather_market(q)
                            .is_some_and(|info| group.cities.contains(&info.city))
                    })
                })
                .map(|p| p.entry_price * p.size)
                .sum();

            if group_exposure > max_group_exposure {
                alerts.push(PositionAlert {
                    market_condition_id: format!("group:{}", group.name),
                    alert_type: "correlated_exposure".to_string(),
                    details: format!(
                        "{} group exposure ${:.2} > limit ${:.2} ({:.0}% of ${:.2} bankroll)",
                        group.name,
                        group_exposure,
                        max_group_exposure,
                        self.max_correlated_exposure_pct * 100.0,
                        bankroll,
                    ),
                    action_taken: "block_new_trades".to_string(),
                });
            }
        }

        alerts
    }

    /// Check if a specific market's city is in a group that's already over-exposed.
    /// Returns true if the trade should be blocked.
    pub fn is_correlated_group_over_limit(
        &self,
        market_question: &str,
        positions: &[PositionRow],
        bankroll: f64,
    ) -> bool {
        if bankroll <= 0.0 {
            return false;
        }

        let market_city = match parse_weather_market(market_question) {
            Some(info) => info.city,
            None => return false, // Non-weather market — no correlation concern
        };

        let group = match self
            .correlation_groups
            .iter()
            .find(|g| g.cities.contains(&market_city))
        {
            Some(g) => g,
            None => return false,
        };

        let max_group_exposure = self.max_correlated_exposure_pct * bankroll;
        let group_exposure: f64 = positions
            .iter()
            .filter(|p| {
                p.question.as_ref().is_some_and(|q| {
                    parse_weather_market(q).is_some_and(|info| group.cities.contains(&info.city))
                })
            })
            .map(|p| p.entry_price * p.size)
            .sum();

        group_exposure >= max_group_exposure
    }

    /// Check if total weather exposure exceeds the global weather cap.
    /// Returns true if new weather bets should be blocked.
    pub fn is_total_weather_over_limit(
        &self,
        positions: &[PositionRow],
        bankroll: f64,
    ) -> bool {
        if bankroll <= 0.0 {
            return false;
        }

        let max_weather_exposure = self.max_total_weather_exposure_pct * bankroll;
        let total_weather_exposure: f64 = positions
            .iter()
            .filter(|p| {
                p.question
                    .as_ref()
                    .is_some_and(|q| parse_weather_market(q).is_some())
            })
            .map(|p| p.entry_price * p.size)
            .sum();

        total_weather_exposure >= max_weather_exposure
    }

    /// Compute drawdown state from peak and current bankroll.
    pub fn check_drawdown(
        db: &Database,
        current_bankroll: f64,
        threshold: f64,
    ) -> Result<DrawdownState> {
        let peak = db.update_peak_bankroll(current_bankroll)?;

        let drawdown_pct = if peak > 0.0 {
            (peak - current_bankroll) / peak
        } else {
            0.0
        };

        let is_active = drawdown_pct >= threshold;

        if is_active {
            info!(
                "DRAWDOWN CIRCUIT BREAKER ACTIVE: {:.1}% drawdown (peak=${:.2}, current=${:.2})",
                drawdown_pct * 100.0,
                peak,
                current_bankroll,
            );
        }

        Ok(DrawdownState {
            peak_bankroll: peak,
            current_bankroll,
            drawdown_pct,
            is_circuit_breaker_active: is_active,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_manager() -> PositionManager {
        PositionManager::new(0.15, 0.90, 0.02, 3.0, 5000.0, 0.10, 0.25)
    }

    fn make_position(entry_price: f64, size: f64) -> PositionRow {
        PositionRow {
            market_condition_id: "0xtest".to_string(),
            token_id: "tok_yes".to_string(),
            side: "YES".to_string(),
            entry_price,
            size,
            status: "open".to_string(),
            current_price: None,
            unrealized_pnl: 0.0,
            estimated_probability: None,
            question: None,
        }
    }

    fn make_weather_position(city: &str, entry_price: f64, size: f64) -> PositionRow {
        PositionRow {
            market_condition_id: format!("0x{}", city.to_lowercase()),
            token_id: format!("tok_{}", city.to_lowercase()),
            side: "YES".to_string(),
            entry_price,
            size,
            status: "open".to_string(),
            current_price: None,
            unrealized_pnl: 0.0,
            estimated_probability: None,
            question: Some(format!(
                "Will the high temperature in {} on February 20, 2026 be between 40\u{00b0}F and 42\u{00b0}F?",
                match city {
                    "NYC" => "New York City",
                    "PHL" => "Philadelphia",
                    "BOS" => "Boston",
                    "CHI" => "Chicago",
                    "MIA" => "Miami",
                    _ => city,
                }
            )),
        }
    }

    // ── Stop-loss tests ──

    #[test]
    fn test_stop_loss_triggered() {
        let mgr = make_manager();
        let pos = make_position(0.60, 10.0);
        // Price dropped from 0.60 to 0.50 → loss = 16.7% > 15%
        let action = mgr.evaluate_position(&pos, 0.50);
        assert!(matches!(action, PositionAction::Exit { .. }));
        if let PositionAction::Exit { reason } = action {
            assert!(reason.contains("Stop-loss"));
        }
    }

    #[test]
    fn test_stop_loss_not_triggered() {
        let mgr = make_manager();
        let pos = make_position(0.60, 10.0);
        // Price dropped from 0.60 to 0.55 → loss = 8.3% < 15%
        let action = mgr.evaluate_position(&pos, 0.55);
        assert_eq!(action, PositionAction::Hold);
    }

    #[test]
    fn test_stop_loss_exact_threshold() {
        let mgr = make_manager();
        let pos = make_position(1.00, 10.0);
        // Price at exactly 15% loss: 1.0 * (1 - 0.15) = 0.85
        // Due to floating-point precision, 0.85 may trigger slightly above 0.15
        // Using price that gives exactly 14.9% loss to stay below threshold
        let action = mgr.evaluate_position(&pos, 0.851);
        // At 14.9% loss, should not trigger stop-loss (< 15%)
        assert_eq!(action, PositionAction::Hold);
    }

    #[test]
    fn test_stop_loss_just_over_threshold() {
        let mgr = make_manager();
        let pos = make_position(1.00, 10.0);
        // Just over: 0.849 → loss = 15.1%
        let action = mgr.evaluate_position(&pos, 0.849);
        assert!(matches!(action, PositionAction::Exit { .. }));
    }

    // ── Take-profit tests ──

    #[test]
    fn test_take_profit_triggered() {
        let mgr = make_manager();
        let pos = make_position(0.50, 10.0);
        // Max profit = 1.0 - 0.50 = 0.50
        // At price 0.96: captured = 0.46/0.50 = 92% > 90%
        let action = mgr.evaluate_position(&pos, 0.96);
        assert!(matches!(action, PositionAction::Exit { .. }));
        if let PositionAction::Exit { reason } = action {
            assert!(reason.contains("Take-profit"));
        }
    }

    #[test]
    fn test_take_profit_not_triggered() {
        let mgr = make_manager();
        let pos = make_position(0.50, 10.0);
        // At price 0.90: captured = 0.40/0.50 = 80% < 90%
        let action = mgr.evaluate_position(&pos, 0.90);
        assert_eq!(action, PositionAction::Hold);
    }

    #[test]
    fn test_take_profit_at_exact_threshold() {
        let mgr = make_manager();
        let pos = make_position(0.50, 10.0);
        // 90% of max profit = 0.45 → price = 0.95
        // captured = 0.45/0.50 = 0.90 >= 0.90
        // Due to floating-point precision, use slightly higher price to ensure >= 0.90
        let action = mgr.evaluate_position(&pos, 0.951);
        if let PositionAction::Exit { reason } = action {
            assert!(reason.contains("Take-profit"));
        } else {
            panic!("Expected Exit, got {:?}", action);
        }
    }

    // ── Edge decay tests ──

    #[test]
    fn test_edge_decay_triggered() {
        let mgr = make_manager();
        let mut pos = make_position(0.50, 10.0);
        pos.estimated_probability = Some(0.75);
        // Current price 0.74 → edge = |0.75 - 0.74| = 0.01 < 0.02
        let action = mgr.evaluate_position(&pos, 0.74);
        assert!(matches!(action, PositionAction::Exit { .. }));
        if let PositionAction::Exit { reason } = action {
            assert!(reason.contains("Edge decay"));
        }
    }

    #[test]
    fn test_edge_decay_not_triggered() {
        let mgr = make_manager();
        let mut pos = make_position(0.50, 10.0);
        pos.estimated_probability = Some(0.75);
        // Current price 0.60 → edge = |0.75 - 0.60| = 0.15 > 0.02
        let action = mgr.evaluate_position(&pos, 0.60);
        assert_eq!(action, PositionAction::Hold);
    }

    #[test]
    fn test_edge_decay_skipped_without_estimate() {
        let mgr = make_manager();
        let pos = make_position(0.50, 10.0);
        // No estimated_probability → skip edge decay check → hold
        let action = mgr.evaluate_position(&pos, 0.51);
        assert_eq!(action, PositionAction::Hold);
    }

    // ── Weather markets skip price-based exits ──

    #[test]
    fn test_weather_market_no_stop_loss() {
        let mgr = make_manager();
        let mut pos = make_weather_position("NYC", 0.036, 80.0);
        pos.estimated_probability = Some(0.75);
        // Price dropped 67% (0.036 → 0.012) — would trigger stop-loss on non-weather
        // but weather markets hold to resolution
        let action = mgr.evaluate_position(&pos, 0.012);
        assert_eq!(action, PositionAction::Hold);
    }

    #[test]
    fn test_weather_market_no_take_profit() {
        let mgr = make_manager();
        let mut pos = make_weather_position("NYC", 0.036, 80.0);
        pos.estimated_probability = Some(0.75);
        // Price rose to 0.95 — would trigger take-profit on non-weather
        // but weather markets hold to resolution
        let action = mgr.evaluate_position(&pos, 0.95);
        assert_eq!(action, PositionAction::Hold);
    }

    #[test]
    fn test_weather_market_still_exits_on_edge_decay() {
        let mgr = make_manager();
        let mut pos = make_weather_position("NYC", 0.036, 80.0);
        pos.estimated_probability = Some(0.04);
        // Model now says 4%, market at 3.5% → edge = 0.5% < 2% threshold
        let action = mgr.evaluate_position(&pos, 0.035);
        assert!(matches!(action, PositionAction::Exit { .. }));
        if let PositionAction::Exit { reason } = action {
            assert!(reason.contains("Edge decay"));
        }
    }

    // ── Stop-loss takes priority over edge decay ──

    #[test]
    fn test_stop_loss_priority_over_edge_decay() {
        let mgr = make_manager();
        let mut pos = make_position(0.60, 10.0);
        pos.estimated_probability = Some(0.75);
        // Price dropped to 0.40 → both stop-loss (33%) and edge decay would trigger
        // Stop-loss should fire first
        let action = mgr.evaluate_position(&pos, 0.40);
        if let PositionAction::Exit { reason } = action {
            assert!(reason.contains("Stop-loss"));
        } else {
            panic!("Expected exit action");
        }
    }

    // ── Volume spike tests ──

    #[test]
    fn test_volume_spike_detected() {
        let mgr = make_manager();
        assert!(mgr.check_volume_spike(9000.0, 2500.0)); // 3.6x > 3.0
    }

    #[test]
    fn test_volume_spike_not_detected() {
        let mgr = make_manager();
        assert!(!mgr.check_volume_spike(5000.0, 2500.0)); // 2.0x < 3.0
    }

    #[test]
    fn test_volume_spike_zero_average() {
        let mgr = make_manager();
        assert!(!mgr.check_volume_spike(5000.0, 0.0)); // zero avg → no spike
    }

    // ── Whale monitoring stub ──

    #[test]
    fn test_whale_monitoring_stub_empty() {
        let mgr = make_manager();
        let alerts = mgr.check_whale_activity("0xtest");
        assert!(alerts.is_empty());
    }

    // ── Correlation checks ──

    #[test]
    fn test_correlated_exposure_within_limit() {
        let mgr = make_manager();
        let positions = vec![make_weather_position("NYC", 0.50, 5.0)];
        // Exposure = 0.50 * 5.0 = 2.50
        // Limit = 0.10 * 100.0 = 10.0
        let alerts = mgr.check_correlated_exposure(&positions, 100.0);
        assert!(alerts.is_empty());
    }

    #[test]
    fn test_correlated_exposure_exceeds_limit() {
        let mgr = make_manager();
        let positions = vec![
            make_weather_position("NYC", 0.50, 12.0), // 6.0
            make_weather_position("PHL", 0.50, 12.0), // 6.0
        ];
        // Total NE exposure = 12.0
        // Limit = 0.10 * 100.0 = 10.0
        let alerts = mgr.check_correlated_exposure(&positions, 100.0);
        assert_eq!(alerts.len(), 1);
        assert!(alerts[0].details.contains("Northeast"));
    }

    #[test]
    fn test_correlated_exposure_different_groups() {
        let mgr = make_manager();
        let positions = vec![
            make_weather_position("NYC", 0.50, 10.0), // NE: 5.0
            make_weather_position("CHI", 0.50, 10.0), // MW: 5.0
        ];
        // Each group at 5.0, limit = 10.0 → both within limit
        let alerts = mgr.check_correlated_exposure(&positions, 100.0);
        assert!(alerts.is_empty());
    }

    #[test]
    fn test_is_correlated_group_over_limit() {
        let mgr = make_manager();
        let positions = vec![
            make_weather_position("NYC", 0.50, 12.0),
            make_weather_position("BOS", 0.50, 12.0),
        ];
        // NE exposure = 12.0, limit = 0.10 * 100 = 10.0
        let question = "Will the high temperature in Philadelphia on February 20, 2026 be between 40\u{00b0}F and 42\u{00b0}F?";
        assert!(mgr.is_correlated_group_over_limit(question, &positions, 100.0));
    }

    #[test]
    fn test_is_correlated_group_not_over_limit() {
        let mgr = make_manager();
        let positions = vec![make_weather_position("NYC", 0.50, 5.0)];
        // NE exposure = 2.5, limit = 10.0
        let question = "Will the high temperature in Philadelphia on February 20, 2026 be between 40\u{00b0}F and 42\u{00b0}F?";
        assert!(!mgr.is_correlated_group_over_limit(question, &positions, 100.0));
    }

    // ── Total weather exposure cap ──

    #[test]
    fn test_total_weather_exposure_within_limit() {
        let mgr = make_manager();
        let positions = vec![
            make_weather_position("NYC", 0.03, 100.0), // 3.0
            make_weather_position("CHI", 0.03, 100.0), // 3.0
        ];
        // Total weather = 6.0, limit = 0.25 * 100 = 25.0
        assert!(!mgr.is_total_weather_over_limit(&positions, 100.0));
    }

    #[test]
    fn test_total_weather_exposure_exceeds_limit() {
        let mgr = make_manager();
        let positions = vec![
            make_weather_position("NYC", 0.50, 30.0), // 15.0
            make_weather_position("CHI", 0.50, 30.0), // 15.0
        ];
        // Total weather = 30.0, limit = 0.25 * 100 = 25.0
        assert!(mgr.is_total_weather_over_limit(&positions, 100.0));
    }

    #[test]
    fn test_non_weather_market_not_blocked() {
        let mgr = make_manager();
        let positions = vec![make_weather_position("NYC", 0.50, 100.0)];
        let question = "Will Bitcoin reach $100k?";
        assert!(!mgr.is_correlated_group_over_limit(question, &positions, 100.0));
    }

    // ── Drawdown tests ──

    #[test]
    fn test_drawdown_inactive() {
        let db = Database::open_in_memory().unwrap();
        db.update_peak_bankroll(100.0).unwrap();

        let state = PositionManager::check_drawdown(&db, 80.0, 0.30).unwrap();
        // 20% drawdown < 30% threshold
        assert!(!state.is_circuit_breaker_active);
        assert!((state.drawdown_pct - 0.20).abs() < 0.01);
    }

    #[test]
    fn test_drawdown_active() {
        let db = Database::open_in_memory().unwrap();
        db.update_peak_bankroll(100.0).unwrap();

        let state = PositionManager::check_drawdown(&db, 65.0, 0.30).unwrap();
        // 35% drawdown > 30% threshold
        assert!(state.is_circuit_breaker_active);
        assert!((state.drawdown_pct - 0.35).abs() < 0.01);
    }

    #[test]
    fn test_drawdown_at_exact_threshold() {
        let db = Database::open_in_memory().unwrap();
        db.update_peak_bankroll(100.0).unwrap();

        let state = PositionManager::check_drawdown(&db, 70.0, 0.30).unwrap();
        // 30% drawdown >= 30% threshold → active
        assert!(state.is_circuit_breaker_active);
    }

    #[test]
    fn test_drawdown_new_peak() {
        let db = Database::open_in_memory().unwrap();
        // First time: peak = 100
        let state = PositionManager::check_drawdown(&db, 100.0, 0.30).unwrap();
        assert!((state.peak_bankroll - 100.0).abs() < f64::EPSILON);
        assert!((state.drawdown_pct - 0.0).abs() < f64::EPSILON);
        assert!(!state.is_circuit_breaker_active);

        // New high: peak = 120
        let state = PositionManager::check_drawdown(&db, 120.0, 0.30).unwrap();
        assert!((state.peak_bankroll - 120.0).abs() < f64::EPSILON);
    }
}
