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
    /// EOA wallet address (the signing key's address)
    pub wallet_address: String,
    /// Proxy/funder wallet that holds the funds (optional)
    pub funder_address: Option<String>,
    /// Signature type: 0=EOA, 1=Magic/email proxy, 2=browser proxy
    pub signature_type: u8,
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
        let funder_address = std::env::var("POLY_PROXY_WALLET").ok();
        let signature_type = std::env::var("POLY_SIGNATURE_TYPE")
            .unwrap_or_else(|_| {
                // Auto-detect: if funder is set, use type 1 (Magic/proxy)
                if funder_address.is_some() { "1".into() } else { "0".into() }
            })
            .parse::<u8>()
            .unwrap_or(0);

        Ok(Self {
            api_key,
            api_secret,
            passphrase,
            wallet_address,
            funder_address,
            signature_type,
        })
    }

    /// The address that holds funds (funder if set, otherwise EOA wallet)
    pub fn funding_address(&self) -> &str {
        self.funder_address.as_deref().unwrap_or(&self.wallet_address)
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
