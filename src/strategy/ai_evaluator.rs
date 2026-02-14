use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tracing::warn;

use super::evaluator::{Signal, SignalSide};
use super::scanner::CandidateMarket;

#[derive(Debug, Deserialize)]
struct ClaudeResponse {
    content: Vec<ClaudeContent>,
}

#[derive(Debug, Deserialize)]
struct ClaudeContent {
    text: String,
}

#[derive(Debug, Deserialize, Serialize)]
struct AiEstimate {
    probability: f64,
    confidence: f64,
    reasoning: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiEvaluatorConfig {
    pub enabled: bool,
    pub model: String,
    #[serde(default = "default_tier2_model")]
    pub tier2_model: String,
    #[serde(default)]
    pub tier2_enabled: bool,
    pub max_markets_per_cycle: usize,
    pub min_confidence: f64,
    pub delay_between_calls_ms: u64,
}

fn default_tier2_model() -> String {
    "claude-sonnet-4-20250514".to_string()
}

impl Default for AiEvaluatorConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            model: "claude-3-5-haiku-20241022".to_string(),
            tier2_model: "claude-sonnet-4-20250514".to_string(),
            tier2_enabled: true,
            max_markets_per_cycle: 20,
            min_confidence: 0.3,
            delay_between_calls_ms: 200,
        }
    }
}

pub struct AiEvaluator {
    http: reqwest::Client,
    api_key: String,
    min_edge: f64,
    pub config: AiEvaluatorConfig,
}

impl AiEvaluator {
    pub fn new(api_key: String, min_edge: f64, config: AiEvaluatorConfig) -> Self {
        Self {
            http: reqwest::Client::new(),
            api_key,
            min_edge,
            config,
        }
    }

    /// Evaluate a batch of candidates, returning signals for those with edge
    /// Two-tier system: Haiku screens all candidates, Sonnet deep-evaluates flagged ones
    pub async fn evaluate_batch(&self, candidates: &[CandidateMarket]) -> Vec<Signal> {
        let mut signals = Vec::new();

        // Sort: fast-resolving markets first (< 48h), then by volume
        let now = chrono::Utc::now();
        let mut sorted: Vec<&CandidateMarket> = candidates.iter().collect();
        sorted.sort_by(|a, b| {
            let hours_a = a.end_date.map(|d| (d - now).num_hours()).unwrap_or(9999);
            let hours_b = b.end_date.map(|d| (d - now).num_hours()).unwrap_or(9999);
            let fast_a = hours_a < 48;
            let fast_b = hours_b < 48;
            match (fast_a, fast_b) {
                (true, false) => std::cmp::Ordering::Less,
                (false, true) => std::cmp::Ordering::Greater,
                _ => b.volume.partial_cmp(&a.volume).unwrap_or(std::cmp::Ordering::Equal),
            }
        });
        let batch = &sorted[..sorted.len().min(self.config.max_markets_per_cycle)];

        // Tier 1: Haiku screens all candidates
        let mut tier1_signals = Vec::new();
        for (i, candidate) in batch.iter().enumerate() {
            if i > 0 {
                tokio::time::sleep(std::time::Duration::from_millis(self.config.delay_between_calls_ms)).await;
            }

            match self.evaluate_one(candidate, &self.config.model).await {
                Ok(Some(signal)) => {
                    println!("  \u{1f9e0} Tier 1 (Haiku): \"{}\"", truncate(&signal.market.question, 55));
                    println!("     Market: YES ${:.2} | AI estimate: {:.0}% (conf: {:.0}%)",
                        signal.market.yes_price, signal.estimated_probability * 100.0, signal.confidence * 100.0);
                    println!("     Edge: {:.0}% -> BUY {}", signal.edge * 100.0, signal.side);
                    println!("     Reason: \"{}\"\n", signal.reason);
                    tier1_signals.push(signal);
                }
                Ok(None) => {}
                Err(e) => {
                    warn!("Tier 1 evaluation failed for \"{}\": {}", truncate(&candidate.question, 40), e);
                }
            }
        }

        // Tier 2: Sonnet deep-evaluates Haiku's picks
        if self.config.tier2_enabled && !tier1_signals.is_empty() {
            println!("\n  \u{1f50d} Tier 2 (Sonnet) deep evaluation on {} candidate(s)...\n", tier1_signals.len());
            for signal in &tier1_signals {
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;

                match self.evaluate_one(&signal.market, &self.config.tier2_model).await {
                    Ok(Some(tier2_signal)) => {
                        println!("  \u{2705} Tier 2 CONFIRMED: \"{}\"", truncate(&tier2_signal.market.question, 55));
                        println!("     Haiku: {:.0}% prob, {:.0}% edge -> {} | Sonnet: {:.0}% prob, {:.0}% edge -> {}",
                            signal.estimated_probability * 100.0, signal.edge * 100.0, signal.side,
                            tier2_signal.estimated_probability * 100.0, tier2_signal.edge * 100.0, tier2_signal.side);
                        println!("     Sonnet reason: \"{}\"\n", tier2_signal.reason);
                        // Use Sonnet's evaluation (more accurate)
                        signals.push(tier2_signal);
                    }
                    Ok(None) => {
                        println!("  \u{274c} Tier 2 REJECTED: \"{}\"", truncate(&signal.market.question, 55));
                        println!("     Sonnet found no edge (Haiku was too optimistic)\n");
                    }
                    Err(e) => {
                        warn!("Tier 2 evaluation failed for \"{}\": {}", truncate(&signal.market.question, 40), e);
                        // Fall back to Haiku's signal if Sonnet fails
                        println!("  \u{26a0}\u{fe0f} Tier 2 failed, using Haiku signal as fallback\n");
                        signals.push(signal.clone());
                    }
                }
            }
        } else if !tier1_signals.is_empty() {
            // Tier 2 disabled, use Haiku signals directly
            signals = tier1_signals;
        }

        signals
    }

    pub async fn evaluate_one(&self, market: &CandidateMarket, model: &str) -> Result<Option<Signal>> {
        let category = market.category.as_deref().unwrap_or("Unknown");
        let end_date_str = market.end_date
            .map(|d| {
                let hours = (d - chrono::Utc::now()).num_hours();
                format!("End date: {} ({} hours from now)", d.format("%Y-%m-%d %H:%M UTC"), hours)
            })
            .unwrap_or_else(|| "End date: Unknown".to_string());

        let prompt = format!(
r#"You are a prediction market analyst. Estimate the probability that the following event will happen.

Market: "{}"
Current YES price: {:.2} (market thinks {:.0}% likely)
Current NO price: {:.2}
Volume: ${:.0}
Category: {}
{}

IMPORTANT GUIDELINES:
- Markets resolving within 48 hours (sports, crypto daily prices, esports) are HIGH PRIORITY — be MORE confident on these since outcomes are more predictable with current data
- Sports and esports outcomes: you can be more confident (0.6-0.9) when you have clear knowledge
- Long-dated political/geopolitical markets (weeks/months away): be LESS confident (0.2-0.5) since uncertainty is higher
- Prefer actionable calls on fast-resolving markets over cautious calls on slow ones

Based on your knowledge, what is the TRUE probability this event resolves YES?

Respond with ONLY a JSON object:
{{"probability": 0.XX, "confidence": 0.XX, "reasoning": "brief explanation"}}

Where:
- probability: your estimate of true YES probability (0.0 to 1.0)
- confidence: how confident you are in your estimate (0.0 to 1.0)
- reasoning: 1-2 sentence explanation"#,
            market.question,
            market.yes_price, market.yes_price * 100.0,
            market.no_price,
            market.volume,
            category,
            end_date_str,
        );

        let body = serde_json::json!({
            "model": model,
            "max_tokens": 200,
            "messages": [{"role": "user", "content": prompt}]
        });

        let resp = self.http
            .post("https://api.anthropic.com/v1/messages")
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await
            .context("Claude API request failed")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("Claude API error {}: {}", status, truncate(&text, 200));
        }

        let claude_resp: ClaudeResponse = resp.json().await.context("Failed to parse Claude response")?;
        let text = claude_resp.content.first()
            .map(|c| c.text.as_str())
            .unwrap_or("");

        // Try to parse JSON from response (handle markdown code blocks too)
        let json_str = if let Some(start) = text.find('{') {
            if let Some(end) = text.rfind('}') {
                &text[start..=end]
            } else {
                text
            }
        } else {
            text
        };

        let estimate: AiEstimate = serde_json::from_str(json_str)
            .context(format!("Failed to parse AI response: {}", truncate(text, 100)))?;

        // Validate
        if estimate.probability < 0.0 || estimate.probability > 1.0
            || estimate.confidence < 0.0 || estimate.confidence > 1.0 {
            anyhow::bail!("Invalid probability/confidence values");
        }

        if estimate.confidence < self.config.min_confidence {
            return Ok(None);
        }

        // Calculate edge for both sides
        let yes_edge = estimate.probability - market.yes_price;
        let no_edge = (1.0 - estimate.probability) - market.no_price;

        let (side, edge) = if yes_edge > no_edge {
            (SignalSide::Yes, yes_edge)
        } else {
            (SignalSide::No, no_edge)
        };

        if edge < self.min_edge {
            return Ok(None);
        }

        Ok(Some(Signal {
            market: market.clone(),
            side,
            estimated_probability: estimate.probability,
            confidence: estimate.confidence,
            edge,
            reason: estimate.reasoning,
        }))
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        let mut end = max.saturating_sub(3);
        while end > 0 && !s.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}...", &s[..end])
    }
}
