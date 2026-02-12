use serde::{Deserialize, Serialize};
use tracing::info;

use crate::estimator::AnalysisResult;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum TradeSide {
    Yes,
    No,
}

impl std::fmt::Display for TradeSide {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TradeSide::Yes => write!(f, "YES"),
            TradeSide::No => write!(f, "NO"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct EdgeOpportunity {
    pub market_id: String,
    pub question: String,
    pub side: TradeSide,
    pub estimated_probability: f64,
    pub market_price: f64,
    pub edge: f64,
    pub net_edge: f64,
    pub confidence: f64,
    pub data_quality: String,
    pub reasoning: String,
    pub analysis_cost: f64,
}

pub struct EdgeDetector {
    pub min_edge_threshold: f64,
    pub min_confidence: f64,
    pub fee_rate: f64,
}

impl EdgeDetector {
    pub fn new(min_edge_threshold: f64, fee_rate: f64) -> Self {
        EdgeDetector {
            min_edge_threshold,
            min_confidence: 0.50,
            fee_rate,
        }
    }

    pub fn detect(&self, analysis: &AnalysisResult) -> Option<EdgeOpportunity> {
        let estimated_yes = analysis.estimate.probability;
        let market_yes = analysis.market_yes_price;

        let yes_edge = estimated_yes - market_yes;
        let no_edge = market_yes - estimated_yes;

        let (side, edge) = if yes_edge >= no_edge {
            (TradeSide::Yes, yes_edge)
        } else {
            (TradeSide::No, no_edge)
        };

        // Subtract round-trip fees from edge
        let net_edge = edge - 2.0 * self.fee_rate;

        if net_edge < self.min_edge_threshold {
            info!(
                "No edge on '{}': est={:.2}, mkt={:.2}, edge={:.1}%, net_edge={:.1}% < {:.1}%",
                analysis.question,
                estimated_yes,
                market_yes,
                edge * 100.0,
                net_edge * 100.0,
                self.min_edge_threshold * 100.0,
            );
            return None;
        }

        if analysis.estimate.confidence < self.min_confidence {
            info!(
                "Low confidence on '{}': conf={:.2} < {:.2}",
                analysis.question, analysis.estimate.confidence, self.min_confidence,
            );
            return None;
        }

        info!(
            "EDGE FOUND on '{}': {} side, est={:.2}, mkt={:.2}, edge={:.1}%, net={:.1}%, conf={:.2}",
            analysis.question,
            side,
            estimated_yes,
            market_yes,
            edge * 100.0,
            net_edge * 100.0,
            analysis.estimate.confidence,
        );

        Some(EdgeOpportunity {
            market_id: analysis.market_id.clone(),
            question: analysis.question.clone(),
            side,
            estimated_probability: estimated_yes,
            market_price: market_yes,
            edge,
            net_edge,
            confidence: analysis.estimate.confidence,
            data_quality: analysis.estimate.data_quality.clone(),
            reasoning: analysis.estimate.reasoning.clone(),
            analysis_cost: analysis.total_cost,
        })
    }

    pub fn detect_batch(&self, analyses: &[AnalysisResult]) -> Vec<EdgeOpportunity> {
        let mut opportunities: Vec<EdgeOpportunity> =
            analyses.iter().filter_map(|a| self.detect(a)).collect();

        opportunities.sort_by(|a, b| {
            b.net_edge
                .partial_cmp(&a.net_edge)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        info!(
            "Edge detection: {} analyses -> {} opportunities",
            analyses.len(),
            opportunities.len(),
        );

        opportunities
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::estimator::{AnalysisResult, FairValueEstimate};

    fn make_analysis(est_prob: f64, market_price: f64, confidence: f64) -> AnalysisResult {
        AnalysisResult {
            market_id: "0xtest".to_string(),
            question: "Test market?".to_string(),
            estimate: FairValueEstimate {
                probability: est_prob,
                confidence,
                reasoning: "Test reasoning".to_string(),
                data_quality: "high".to_string(),
            },
            market_yes_price: market_price,
            total_cost: 0.01,
            api_calls: vec![],
        }
    }

    #[test]
    fn test_detect_yes_edge_above_threshold() {
        // fee_rate=0.02 → round-trip = 0.04, net_edge = 0.20 - 0.04 = 0.16
        let detector = EdgeDetector::new(0.08, 0.02);
        let analysis = make_analysis(0.75, 0.55, 0.85);
        let opp = detector.detect(&analysis).unwrap();
        assert_eq!(opp.side, TradeSide::Yes);
        assert!((opp.edge - 0.20).abs() < 0.001);
        assert!((opp.net_edge - 0.16).abs() < 0.001);
    }

    #[test]
    fn test_detect_no_edge_above_threshold() {
        let detector = EdgeDetector::new(0.08, 0.02);
        let analysis = make_analysis(0.30, 0.55, 0.85);
        let opp = detector.detect(&analysis).unwrap();
        assert_eq!(opp.side, TradeSide::No);
        assert!((opp.edge - 0.25).abs() < 0.001);
        assert!((opp.net_edge - 0.21).abs() < 0.001);
    }

    #[test]
    fn test_detect_below_threshold() {
        // edge=0.05, net_edge=0.05-0.04=0.01 < 0.08
        let detector = EdgeDetector::new(0.08, 0.02);
        let analysis = make_analysis(0.60, 0.55, 0.85);
        assert!(detector.detect(&analysis).is_none());
    }

    #[test]
    fn test_detect_low_confidence() {
        let detector = EdgeDetector::new(0.08, 0.02);
        let analysis = make_analysis(0.75, 0.55, 0.30);
        assert!(detector.detect(&analysis).is_none());
    }

    #[test]
    fn test_detect_edge_at_exact_threshold() {
        // With fee_rate=0.0: edge=0.08 exactly → net_edge=0.08 >= 0.08
        let detector = EdgeDetector::new(0.08, 0.0);
        let analysis = make_analysis(0.68, 0.60, 0.85);
        let opp = detector.detect(&analysis);
        assert!(opp.is_some());
    }

    #[test]
    fn test_fees_can_eliminate_edge() {
        // edge=0.10, fee_rate=0.05 → net_edge=0.10-0.10=0.00 < 0.08
        let detector = EdgeDetector::new(0.08, 0.05);
        let analysis = make_analysis(0.65, 0.55, 0.85);
        assert!(detector.detect(&analysis).is_none());
    }

    #[test]
    fn test_detect_batch_sorts_by_net_edge() {
        let detector = EdgeDetector::new(0.08, 0.02);
        let analyses = vec![
            make_analysis(0.65, 0.55, 0.85), // edge 0.10, net 0.06 → rejected (< 0.08)
            make_analysis(0.80, 0.55, 0.85), // edge 0.25, net 0.21
            make_analysis(0.70, 0.55, 0.85), // edge 0.15, net 0.11
        ];
        let opps = detector.detect_batch(&analyses);
        assert_eq!(opps.len(), 2); // first one rejected due to net_edge < threshold
        assert!((opps[0].net_edge - 0.21).abs() < 0.001);
        assert!((opps[1].net_edge - 0.11).abs() < 0.001);
    }

    #[test]
    fn test_detect_batch_empty() {
        let detector = EdgeDetector::new(0.08, 0.02);
        let opps = detector.detect_batch(&[]);
        assert!(opps.is_empty());
    }
}
