use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use base64::{engine::general_purpose::URL_SAFE as BASE64_URL, Engine};
use hmac::{Hmac, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

/// CLOB L2 authentication for Polymarket API
pub struct ClobAuth {
    pub api_key: String,
    pub api_secret: String,
    pub passphrase: String,
    pub wallet_address: String,
}

impl ClobAuth {
    /// Load credentials from environment variables
    pub fn from_env() -> Result<Self> {
        let api_key = std::env::var("POLY_API_KEY")
            .context("POLY_API_KEY not set. Add it to .env file.")?;
        let api_secret = std::env::var("POLY_API_SECRET")
            .context("POLY_API_SECRET not set. Add it to .env file.")?;
        let passphrase = std::env::var("POLY_PASSPHRASE")
            .context("POLY_PASSPHRASE not set. Add it to .env file.")?;
        let wallet_address = std::env::var("POLY_WALLET_ADDRESS")
            .context("POLY_WALLET_ADDRESS not set. Add it to .env file.")?;

        Ok(Self {
            api_key,
            api_secret,
            passphrase,
            wallet_address,
        })
    }

    /// Generate L2 auth headers for a CLOB API request.
    /// Message format: timestamp + method + requestPath [+ body]
    /// Signature: HMAC-SHA256 with url-safe base64 decoded secret, url-safe base64 encoded output
    pub fn sign_request(
        &self,
        method: &str,
        path: &str,
        body: Option<&str>,
    ) -> Result<HashMap<String, String>> {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
            .to_string();

        // Build message: timestamp + method + requestPath [+ body]
        let mut message = format!("{}{}{}", timestamp, method, path);
        if let Some(b) = body {
            message.push_str(b);
        }

        // HMAC-SHA256 sign with url-safe base64 decoded secret
        let secret_bytes = BASE64_URL.decode(&self.api_secret)
            .context("Failed to base64-decode API secret")?;
        let mut mac = HmacSha256::new_from_slice(&secret_bytes)
            .context("Invalid HMAC key")?;
        mac.update(message.as_bytes());
        let signature = BASE64_URL.encode(mac.finalize().into_bytes());

        let mut headers = HashMap::new();
        headers.insert("POLY_ADDRESS".into(), self.wallet_address.clone());
        headers.insert("POLY_SIGNATURE".into(), signature);
        headers.insert("POLY_TIMESTAMP".into(), timestamp);
        headers.insert("POLY_API_KEY".into(), self.api_key.clone());
        headers.insert("POLY_PASSPHRASE".into(), self.passphrase.clone());

        Ok(headers)
    }
}
