use super::scanner::CandidateMarket;

/// Signal: which side to bet and why
#[derive(Debug, Clone)]
pub struct Signal {
    pub market: CandidateMarket,
    pub side: SignalSide,
    pub estimated_probability: f64,
    pub confidence: f64,
    pub edge: f64,
    pub reason: String,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SignalSide {
    Yes,
    No,
}

impl std::fmt::Display for SignalSide {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SignalSide::Yes => write!(f, "YES"),
            SignalSide::No => write!(f, "NO"),
        }
    }
}

pub struct Evaluator {
    pub min_edge: f64,
}

impl Evaluator {
    pub fn new(min_edge: f64) -> Self {
        Self { min_edge }
    }

    /// Evaluate a candidate market for potential edge
    pub fn evaluate(&self, market: &CandidateMarket) -> Option<Signal> {
        let question = market.question.to_lowercase();

        // Skip markets near 50/50 — too efficient
        if market.yes_price > 0.40 && market.yes_price < 0.60 {
            return None;
        }

        // Determine category efficiency
        let is_politics = question.contains("president") || question.contains("election")
            || question.contains("trump") || question.contains("biden")
            || question.contains("congress") || question.contains("senate");
        let is_sports = question.contains("win") && (question.contains("game")
            || question.contains("match") || question.contains("nba")
            || question.contains("nfl") || question.contains("mlb")
            || question.contains("ufc") || question.contains("fight"));
        let is_crypto = question.contains("bitcoin") || question.contains("btc")
            || question.contains("eth") || question.contains("crypto")
            || question.contains("price");

        // Politics markets tend to be very efficient — raise threshold
        let edge_multiplier = if is_politics {
            0.5 // need 2x more edge for politics
        } else if is_sports {
            1.2 // sports can be stale
        } else if is_crypto {
            0.8 // crypto markets moderately efficient
        } else {
            1.0
        };

        // --- Heuristic: Extreme prices (>0.90 or <0.10) ---
        // Markets at extreme prices often have edge on the dominant side
        // because resolution is near-certain but small holders keep selling

        let (side, estimated_prob, confidence, reason) = if market.yes_price > 0.90 {
            // Very likely YES — check if there's value
            let has_uncertainty = question.contains("if") || question.contains("could")
                || question.contains("might") || question.contains("?");

            if has_uncertainty && market.yes_price < 0.95 {
                // Uncertainty words + high price = potential NO value
                let est = market.yes_price - 0.05; // we think it's slightly lower
                (SignalSide::No, est, 0.3, "High YES price with uncertainty language → NO value".to_string())
            } else if market.yes_price >= 0.95 {
                // Near certain — ride the wave
                (SignalSide::Yes, 0.98, 0.4, format!("Near-certain YES at {:.0}% → value in YES resolution", market.yes_price * 100.0))
            } else {
                return None;
            }
        } else if market.yes_price < 0.10 {
            // Very unlikely YES
            let has_certainty = question.contains("will") && !question.contains("not");

            if has_certainty && market.yes_price > 0.05 {
                // Maybe underpriced YES
                (SignalSide::Yes, market.yes_price + 0.08, 0.3, "Low YES price may be undervalued".to_string())
            } else {
                // Likely NO is correct — ride it
                (SignalSide::No, 0.03, 0.4, format!("Near-certain NO at {:.0}% → value in NO resolution", market.no_price * 100.0))
            }
        } else if market.yes_price > 0.75 && market.yes_price <= 0.90 {
            // Moderately high YES
            if is_sports {
                // Sports favorites can be stale
                let est = market.yes_price + 0.05;
                let conf = 0.25;
                (SignalSide::Yes, est.min(0.98), conf, "Sports favorite — odds may be stale".to_string())
            } else {
                return None;
            }
        } else if market.yes_price >= 0.10 && market.yes_price <= 0.25 {
            // Low-ish YES
            if is_sports {
                let est = market.yes_price + 0.08;
                let conf = 0.2;
                (SignalSide::Yes, est, conf, "Sports underdog — potential value".to_string())
            } else {
                return None;
            }
        } else if market.yes_price >= 0.60 && market.yes_price <= 0.75 {
            // Moderate YES lean — look for sports/event value
            if is_sports {
                let est = market.yes_price + 0.06;
                (SignalSide::Yes, est.min(0.95), 0.2, "Moderate favorite — potential stale odds".to_string())
            } else {
                return None;
            }
        } else if market.yes_price >= 0.25 && market.yes_price <= 0.40 {
            // Moderate NO lean
            if is_sports {
                let est = market.yes_price + 0.10;
                (SignalSide::Yes, est, 0.2, "Moderate underdog — sports value".to_string())
            } else {
                return None;
            }
        } else {
            return None;
        };

        // Calculate edge
        let market_price = match side {
            SignalSide::Yes => market.yes_price,
            SignalSide::No => market.no_price,
        };
        let edge = match side {
            SignalSide::Yes => estimated_prob - market.yes_price,
            SignalSide::No => (1.0 - estimated_prob) - market.no_price,
        };

        // Apply edge multiplier and check minimum
        let adjusted_edge = edge * edge_multiplier;
        if adjusted_edge < self.min_edge {
            return None;
        }

        Some(Signal {
            market: market.clone(),
            side,
            estimated_probability: estimated_prob,
            confidence,
            edge: adjusted_edge,
            reason,
        })
    }
}
