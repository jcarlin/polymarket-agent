use tracing::info;

use crate::edge_detector::{EdgeOpportunity, TradeSide};

pub struct PositionSizer {
    pub kelly_fraction: f64,
    pub max_position_pct: f64,
    pub max_total_exposure_pct: f64,
}

#[derive(Debug, Clone)]
pub struct SizingResult {
    pub raw_kelly: f64,
    pub adjusted_kelly: f64,
    pub position_usd: f64,
    pub shares: f64,
    pub limit_price: f64,
    pub reject_reason: Option<String>,
}

impl SizingResult {
    pub fn is_rejected(&self) -> bool {
        self.reject_reason.is_some()
    }

    fn rejected(reason: &str) -> Self {
        SizingResult {
            raw_kelly: 0.0,
            adjusted_kelly: 0.0,
            position_usd: 0.0,
            shares: 0.0,
            limit_price: 0.0,
            reject_reason: Some(reason.to_string()),
        }
    }
}

impl PositionSizer {
    pub fn new(kelly_fraction: f64, max_position_pct: f64, max_total_exposure_pct: f64) -> Self {
        PositionSizer {
            kelly_fraction,
            max_position_pct,
            max_total_exposure_pct,
        }
    }

    /// Size a position for a binary prediction market opportunity.
    ///
    /// Kelly formula for binary markets:
    ///   buy_price = market_price (YES) or 1 - market_price (NO)
    ///   win_prob  = estimated_probability (YES) or 1 - estimated_probability (NO)
    ///   kelly     = (win_prob - buy_price) / (1 - buy_price)
    ///   adjusted  = kelly * kelly_fraction (half-Kelly)
    ///   position  = min(adjusted * bankroll, max_position_pct * bankroll, remaining_exposure)
    ///   shares    = position / buy_price
    ///
    /// `days_until_resolution`: if Some, applies a time-based multiplier for weather markets.
    ///   1-2 days: 1.0x, 3-4 days: 0.7x, 5-7 days: 0.4x, >7 days: 0.2x
    pub fn size_position(
        &self,
        opp: &EdgeOpportunity,
        bankroll: f64,
        current_exposure: f64,
    ) -> SizingResult {
        self.size_position_with_time(opp, bankroll, current_exposure, None)
    }

    /// Size a position with optional time-based multiplier for weather markets.
    pub fn size_position_with_time(
        &self,
        opp: &EdgeOpportunity,
        bankroll: f64,
        current_exposure: f64,
        days_until_resolution: Option<i64>,
    ) -> SizingResult {
        // Determine buy price and win probability based on side
        let (buy_price, win_prob) = match opp.side {
            TradeSide::Yes => (opp.market_price, opp.estimated_probability),
            TradeSide::No => (1.0 - opp.market_price, 1.0 - opp.estimated_probability),
        };

        // Guard against buy_price at or above 1.0 (division by zero)
        if buy_price >= 1.0 {
            return SizingResult::rejected("buy price >= 1.0");
        }

        // Kelly criterion for binary outcome
        let raw_kelly = (win_prob - buy_price) / (1.0 - buy_price);

        if raw_kelly <= 0.0 {
            return SizingResult::rejected("negative Kelly — no edge");
        }

        let mut adjusted_kelly = raw_kelly * self.kelly_fraction;

        // Time-based multiplier for weather markets
        if let Some(days) = days_until_resolution {
            let time_multiplier = match days {
                0..=2 => 1.0,
                3..=4 => 0.7,
                5..=7 => 0.4,
                _ => 0.2,
            };
            if time_multiplier < 1.0 {
                info!(
                    "Weather time multiplier: {:.1}x ({}d until resolution)",
                    time_multiplier, days,
                );
            }
            adjusted_kelly *= time_multiplier;
        }

        // Position caps
        let max_exposure = self.max_total_exposure_pct * bankroll;
        let remaining_exposure = (max_exposure - current_exposure).max(0.0);

        if remaining_exposure <= 0.0 {
            return SizingResult::rejected("exposure limit reached");
        }

        let position_usd = (adjusted_kelly * bankroll)
            .min(self.max_position_pct * bankroll)
            .min(remaining_exposure);

        if position_usd < 1.0 {
            return SizingResult::rejected(&format!(
                "position too small: ${:.2} < $1.00 minimum",
                position_usd
            ));
        }

        let shares = position_usd / buy_price;

        info!(
            "Sized {} {}: kelly={:.3}, adj={:.3}, ${:.2} ({:.1} shares @ {:.2})",
            opp.side, opp.question, raw_kelly, adjusted_kelly, position_usd, shares, buy_price,
        );

        SizingResult {
            raw_kelly,
            adjusted_kelly,
            position_usd,
            shares,
            limit_price: buy_price,
            reject_reason: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_opportunity(
        side: TradeSide,
        estimated_prob: f64,
        market_price: f64,
        edge: f64,
    ) -> EdgeOpportunity {
        EdgeOpportunity {
            market_id: "0xtest".to_string(),
            question: "Test market?".to_string(),
            side,
            estimated_probability: estimated_prob,
            market_price,
            edge,
            confidence: 0.85,
            data_quality: "high".to_string(),
            reasoning: "Test reasoning".to_string(),
            analysis_cost: 0.01,
        }
    }

    #[test]
    fn test_positive_edge_yes_side() {
        let sizer = PositionSizer::new(0.5, 0.06, 0.40);
        // est=0.75, market=0.55 → buy YES at 0.55
        // kelly = (0.75 - 0.55) / (1 - 0.55) = 0.20 / 0.45 = 0.4444
        // adjusted = 0.4444 * 0.5 = 0.2222
        // position = min(0.2222*50, 0.06*50, 20) = min(11.11, 3.0, 20) = 3.0
        let opp = make_opportunity(TradeSide::Yes, 0.75, 0.55, 0.20);
        let result = sizer.size_position(&opp, 50.0, 0.0);
        assert!(!result.is_rejected());
        assert!((result.raw_kelly - 0.4444).abs() < 0.001);
        assert!((result.adjusted_kelly - 0.2222).abs() < 0.001);
        assert!((result.position_usd - 3.0).abs() < 0.01); // capped by max_position_pct
        assert!((result.limit_price - 0.55).abs() < f64::EPSILON);
        assert!(result.shares > 0.0);
    }

    #[test]
    fn test_positive_edge_no_side() {
        let sizer = PositionSizer::new(0.5, 0.06, 0.40);
        // est=0.30, market=0.55 → buy NO at 1-0.55=0.45, win_prob=1-0.30=0.70
        // kelly = (0.70 - 0.45) / (1 - 0.45) = 0.25 / 0.55 = 0.4545
        // adjusted = 0.4545 * 0.5 = 0.2273
        // position = min(0.2273*50, 0.06*50, 20) = min(11.36, 3.0, 20) = 3.0
        let opp = make_opportunity(TradeSide::No, 0.30, 0.55, 0.25);
        let result = sizer.size_position(&opp, 50.0, 0.0);
        assert!(!result.is_rejected());
        assert!((result.raw_kelly - 0.4545).abs() < 0.001);
        assert!((result.limit_price - 0.45).abs() < f64::EPSILON);
    }

    #[test]
    fn test_negative_kelly_rejected() {
        let sizer = PositionSizer::new(0.5, 0.06, 0.40);
        // est=0.50, market=0.55 → buy YES at 0.55, win_prob=0.50
        // kelly = (0.50 - 0.55) / (1 - 0.55) = -0.05/0.45 = -0.111 → reject
        let opp = make_opportunity(TradeSide::Yes, 0.50, 0.55, 0.0);
        let result = sizer.size_position(&opp, 50.0, 0.0);
        assert!(result.is_rejected());
        assert!(result.reject_reason.unwrap().contains("negative Kelly"));
    }

    #[test]
    fn test_half_kelly_applied() {
        let sizer = PositionSizer::new(0.5, 1.0, 1.0); // no caps for this test
        let opp = make_opportunity(TradeSide::Yes, 0.80, 0.50, 0.30);
        // kelly = (0.80 - 0.50) / (1 - 0.50) = 0.60
        // adjusted = 0.60 * 0.5 = 0.30
        let result = sizer.size_position(&opp, 100.0, 0.0);
        assert!(!result.is_rejected());
        assert!((result.raw_kelly - 0.60).abs() < 1e-10);
        assert!((result.adjusted_kelly - 0.30).abs() < 1e-10);
        // position = 0.30 * 100 = 30.0
        assert!((result.position_usd - 30.0).abs() < 0.01);
    }

    #[test]
    fn test_position_capped_by_max_pct() {
        let sizer = PositionSizer::new(1.0, 0.06, 1.0); // full Kelly, 6% cap
                                                        // Large kelly → capped at 6% of bankroll
        let opp = make_opportunity(TradeSide::Yes, 0.95, 0.50, 0.45);
        // kelly = (0.95 - 0.50) / (1 - 0.50) = 0.90
        // position = min(0.90*100, 0.06*100, 100) = min(90, 6, 100) = 6
        let result = sizer.size_position(&opp, 100.0, 0.0);
        assert!(!result.is_rejected());
        assert!((result.position_usd - 6.0).abs() < 0.01);
    }

    #[test]
    fn test_exposure_limit_constrains_position() {
        let sizer = PositionSizer::new(0.5, 0.06, 0.40);
        // With $50 bankroll, max exposure = 0.40*50 = 20
        // Current exposure = 19.5, remaining = 0.5 → too small
        let opp = make_opportunity(TradeSide::Yes, 0.75, 0.55, 0.20);
        let result = sizer.size_position(&opp, 50.0, 19.5);
        // remaining = 20.0 - 19.5 = 0.5, which is < $1.00
        assert!(result.is_rejected());
        assert!(result.reject_reason.unwrap().contains("too small"));
    }

    #[test]
    fn test_exposure_limit_partially_constrains() {
        let sizer = PositionSizer::new(0.5, 0.06, 0.40);
        // bankroll=50, max_exposure=20, current=18, remaining=2
        // Kelly wants 3.0 but remaining is 2.0
        let opp = make_opportunity(TradeSide::Yes, 0.75, 0.55, 0.20);
        let result = sizer.size_position(&opp, 50.0, 18.0);
        assert!(!result.is_rejected());
        assert!((result.position_usd - 2.0).abs() < 0.01);
    }

    #[test]
    fn test_min_trade_size_rejected() {
        let sizer = PositionSizer::new(0.5, 0.06, 0.40);
        // Very small bankroll → position too small
        let opp = make_opportunity(TradeSide::Yes, 0.75, 0.55, 0.20);
        // bankroll=5, max_pos=0.06*5=0.30 < $1.00
        let result = sizer.size_position(&opp, 5.0, 0.0);
        assert!(result.is_rejected());
        assert!(result.reject_reason.unwrap().contains("too small"));
    }

    #[test]
    fn test_shares_calculation() {
        let sizer = PositionSizer::new(0.5, 1.0, 1.0); // no caps
        let opp = make_opportunity(TradeSide::Yes, 0.80, 0.50, 0.30);
        let result = sizer.size_position(&opp, 100.0, 0.0);
        // position = 30.0, buy_price = 0.50
        // shares = 30.0 / 0.50 = 60.0
        assert!((result.shares - 60.0).abs() < 0.01);
    }

    #[test]
    fn test_zero_bankroll_rejects() {
        let sizer = PositionSizer::new(0.5, 0.06, 0.40);
        let opp = make_opportunity(TradeSide::Yes, 0.75, 0.55, 0.20);
        let result = sizer.size_position(&opp, 0.0, 0.0);
        assert!(result.is_rejected());
    }

    #[test]
    fn test_time_based_sizing() {
        let sizer = PositionSizer::new(0.5, 1.0, 1.0); // no caps for clarity
        let opp = make_opportunity(TradeSide::Yes, 0.80, 0.50, 0.30);
        // kelly = (0.80 - 0.50) / (1 - 0.50) = 0.60, adjusted = 0.30
        // position = 0.30 * 100 = 30.0

        // 2-day market: 1.0x → $30.0
        let r2 = sizer.size_position_with_time(&opp, 100.0, 0.0, Some(2));
        assert!(!r2.is_rejected());
        assert!((r2.position_usd - 30.0).abs() < 0.01);

        // 3-day market: 0.7x → $21.0
        let r3 = sizer.size_position_with_time(&opp, 100.0, 0.0, Some(3));
        assert!(!r3.is_rejected());
        assert!((r3.position_usd - 21.0).abs() < 0.01);

        // 6-day market: 0.4x → $12.0
        let r6 = sizer.size_position_with_time(&opp, 100.0, 0.0, Some(6));
        assert!(!r6.is_rejected());
        assert!((r6.position_usd - 12.0).abs() < 0.01);

        // 10-day market: 0.2x → $6.0
        let r10 = sizer.size_position_with_time(&opp, 100.0, 0.0, Some(10));
        assert!(!r10.is_rejected());
        assert!((r10.position_usd - 6.0).abs() < 0.01);

        // None (non-weather): same as no multiplier → $30.0
        let rn = sizer.size_position_with_time(&opp, 100.0, 0.0, None);
        assert!(!rn.is_rejected());
        assert!((rn.position_usd - 30.0).abs() < 0.01);
    }
}
