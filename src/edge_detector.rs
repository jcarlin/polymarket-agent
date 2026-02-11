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
    pub confidence: f64,
    pub data_quality: String,
    pub reasoning: String,
    pub analysis_cost: f64,
}

pub struct EdgeDetector {
    pub min_edge_threshold: f64,
    pub min_confidence: f64,
}

impl EdgeDetector {
    pub fn new(min_edge_threshold: f64) -> Self {
        EdgeDetector {
            min_edge_threshold,
            min_confidence: 0.50,
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

        if edge < self.min_edge_threshold {
            info!(
                "No edge on '{}': est={:.2}, mkt={:.2}, edge={:.1}% < {:.1}%",
                analysis.question,
                estimated_yes,
                market_yes,
                edge * 100.0,
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
            "EDGE FOUND on '{}': {} side, est={:.2}, mkt={:.2}, edge={:.1}%, conf={:.2}",
            analysis.question,
            side,
            estimated_yes,
            market_yes,
            edge * 100.0,
            analysis.estimate.confidence,
        );

        Some(EdgeOpportunity {
            market_id: analysis.market_id.clone(),
            question: analysis.question.clone(),
            side,
            estimated_probability: estimated_yes,
            market_price: market_yes,
            edge,
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
            b.edge
                .partial_cmp(&a.edge)
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
        let detector = EdgeDetector::new(0.08);
        let analysis = make_analysis(0.75, 0.55, 0.85);
        let opp = detector.detect(&analysis).unwrap();
        assert_eq!(opp.side, TradeSide::Yes);
        assert!((opp.edge - 0.20).abs() < 0.001);
    }

    #[test]
    fn test_detect_no_edge_above_threshold() {
        let detector = EdgeDetector::new(0.08);
        let analysis = make_analysis(0.30, 0.55, 0.85);
        let opp = detector.detect(&analysis).unwrap();
        assert_eq!(opp.side, TradeSide::No);
        assert!((opp.edge - 0.25).abs() < 0.001);
    }

    #[test]
    fn test_detect_below_threshold() {
        let detector = EdgeDetector::new(0.08);
        let analysis = make_analysis(0.60, 0.55, 0.85);
        assert!(detector.detect(&analysis).is_none());
    }

    #[test]
    fn test_detect_low_confidence() {
        let detector = EdgeDetector::new(0.08);
        let analysis = make_analysis(0.75, 0.55, 0.30);
        assert!(detector.detect(&analysis).is_none());
    }

    #[test]
    fn test_detect_edge_at_exact_threshold() {
        let detector = EdgeDetector::new(0.08);
        // Use 0.68 - 0.60 = 0.08 exactly (avoids f64 rounding with 0.63 - 0.55)
        let analysis = make_analysis(0.68, 0.60, 0.85);
        let opp = detector.detect(&analysis);
        assert!(opp.is_some()); // edge 0.08 >= threshold 0.08
    }

    #[test]
    fn test_detect_batch_sorts_by_edge() {
        let detector = EdgeDetector::new(0.08);
        let analyses = vec![
            make_analysis(0.65, 0.55, 0.85), // edge 0.10
            make_analysis(0.80, 0.55, 0.85), // edge 0.25
            make_analysis(0.70, 0.55, 0.85), // edge 0.15
        ];
        let opps = detector.detect_batch(&analyses);
        assert_eq!(opps.len(), 3);
        assert!((opps[0].edge - 0.25).abs() < 0.001);
        assert!((opps[1].edge - 0.15).abs() < 0.001);
        assert!((opps[2].edge - 0.10).abs() < 0.001);
    }

    #[test]
    fn test_detect_batch_empty() {
        let detector = EdgeDetector::new(0.08);
        let opps = detector.detect_batch(&[]);
        assert!(opps.is_empty());
    }
}
