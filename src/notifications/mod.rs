use tracing::warn;

/// Telegram notification client. If token/chat_id are missing, all methods are no-ops.
#[derive(Clone)]
pub struct TelegramNotifier {
    bot_token: Option<String>,
    chat_id: Option<String>,
    client: reqwest::Client,
}

impl TelegramNotifier {
    pub fn new() -> Self {
        let bot_token = std::env::var("TELEGRAM_BOT_TOKEN").ok().filter(|s| !s.is_empty());
        let chat_id = std::env::var("TELEGRAM_CHAT_ID").ok().filter(|s| !s.is_empty());

        if bot_token.is_some() && chat_id.is_some() {
            tracing::info!("Telegram notifications enabled");
        } else {
            tracing::info!("Telegram notifications disabled (missing TELEGRAM_BOT_TOKEN or TELEGRAM_CHAT_ID)");
        }

        Self {
            bot_token,
            chat_id,
            client: reqwest::Client::new(),
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.bot_token.is_some() && self.chat_id.is_some()
    }

    /// Send a message. Silently skips if not configured.
    pub async fn send(&self, text: &str) {
        let (Some(token), Some(chat_id)) = (&self.bot_token, &self.chat_id) else {
            return;
        };

        let url = format!("https://api.telegram.org/bot{}/sendMessage", token);
        let body = serde_json::json!({
            "chat_id": chat_id,
            "text": text,
            "parse_mode": "HTML",
            "disable_web_page_preview": true
        });

        match self.client.post(&url).json(&body).send().await {
            Ok(resp) if !resp.status().is_success() => {
                warn!("Telegram API error: {}", resp.status());
            }
            Err(e) => {
                warn!("Telegram send failed: {}", e);
            }
            _ => {}
        }
    }

    /// Notify about a signal found
    pub async fn notify_signal(&self, market: &str, side: &str, edge: f64, confidence: f64, size_usd: f64, reason: &str) {
        let msg = format!(
            "<b>Signal Found</b>\n\
             Market: {}\n\
             Side: {} | Edge: {:.1}% | Confidence: {:.1}%\n\
             Proposed size: ${:.2}\n\
             Reason: {}",
            html_escape(market), side, edge * 100.0, confidence * 100.0, size_usd, html_escape(reason)
        );
        self.send(&msg).await;
    }

    /// Notify about a trade placed (or dry run)
    pub async fn notify_trade(&self, market: &str, side: &str, price: f64, size_usd: f64, shares: f64, dry_run: bool) {
        let mode = if dry_run { "DRY RUN" } else { "LIVE" };
        let msg = format!(
            "<b>Trade Placed ({})</b>\n\
             Market: {}\n\
             BUY {} @ ${:.4} | ${:.2} ({:.2} shares)",
            mode, html_escape(market), side, price, size_usd, shares
        );
        self.send(&msg).await;
    }

    /// Notify about an error
    pub async fn notify_error(&self, context: &str, error: &str) {
        let msg = format!(
            "<b>Error</b>\n\
             Context: {}\n\
             {}",
            html_escape(context), html_escape(error)
        );
        self.send(&msg).await;
    }
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}
