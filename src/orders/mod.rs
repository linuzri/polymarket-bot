use anyhow::{Context, Result};
use alloy::primitives::{Address, FixedBytes, U256, keccak256};
use alloy::signers::local::PrivateKeySigner;
use alloy::signers::Signer;
use serde_json::json;
use tracing::{debug, info, warn};

/// Contract addresses
const EXCHANGE: &str = "0x4bFb41d5B3570DeFd03C39a9A4D8dE6Bd8B8982E";
const NEG_RISK_EXCHANGE: &str = "0xC5d563A36AE78145C45a50134d48A1215220f80a";

/// EIP-712 type hash for the Order struct
fn order_type_hash() -> FixedBytes<32> {
    keccak256(
        b"Order(uint256 salt,address maker,address signer,address taker,uint256 tokenId,uint256 makerAmount,uint256 takerAmount,uint256 expiration,uint256 nonce,uint256 feeRateBps,uint8 side,uint8 signatureType)"
    )
}

/// EIP-712 domain separator
fn domain_separator(neg_risk: bool) -> FixedBytes<32> {
    let verifying_contract: Address = if neg_risk {
        NEG_RISK_EXCHANGE.parse().unwrap()
    } else {
        EXCHANGE.parse().unwrap()
    };

    let domain_type_hash = keccak256(
        b"EIP712Domain(string name,string version,uint256 chainId,address verifyingContract)"
    );
    let name_hash = keccak256(b"Polymarket CTF Exchange");
    let version_hash = keccak256(b"1");
    let chain_id = U256::from(137);

    let mut buf = Vec::with_capacity(160);
    buf.extend_from_slice(domain_type_hash.as_slice());
    buf.extend_from_slice(name_hash.as_slice());
    buf.extend_from_slice(version_hash.as_slice());
    buf.extend_from_slice(&chain_id.to_be_bytes::<32>());
    buf.extend_from_slice(&{
        let mut padded = [0u8; 32];
        padded[12..].copy_from_slice(verifying_contract.as_slice());
        padded
    });

    keccak256(&buf)
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Side {
    Buy,
    Sell,
}

impl Side {
    pub fn as_u8(&self) -> u8 {
        match self {
            Side::Buy => 0,
            Side::Sell => 1,
        }
    }

    pub fn as_str(&self) -> &str {
        match self {
            Side::Buy => "BUY",
            Side::Sell => "SELL",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Order {
    pub salt: U256,
    pub maker: Address,
    pub signer: Address,
    pub taker: Address,
    pub token_id: U256,
    pub maker_amount: U256,
    pub taker_amount: U256,
    pub expiration: U256,
    pub nonce: U256,
    pub fee_rate_bps: U256,
    pub side: Side,
    pub signature_type: u8,
}

impl Order {
    /// Build a new order with tick-size-aware rounding
    /// tick_size: 0.1, 0.01, 0.001, or 0.0001
    pub fn new(
        maker: Address,
        signer: Address,
        token_id: &str,
        side: Side,
        price: f64,
        size: f64,
        fee_rate_bps: u64,
    ) -> Result<Self> {
        Self::new_with_tick(maker, signer, token_id, side, price, size, fee_rate_bps, 0.01)
    }

    pub fn new_with_tick(
        maker: Address,
        signer: Address,
        token_id: &str,
        side: Side,
        price: f64,
        size: f64,
        fee_rate_bps: u64,
        tick_size: f64,
    ) -> Result<Self> {
        // Determine decimal precision from tick size
        // tick 0.1 → price_decimals=1, amount_decimals=3
        // tick 0.01 → price_decimals=2, amount_decimals=4
        // tick 0.001 → price_decimals=3, amount_decimals=5
        // tick 0.0001 → price_decimals=4, amount_decimals=6
        let price_decimals = if tick_size <= 0.0001 { 4 }
            else if tick_size <= 0.001 { 3 }
            else if tick_size <= 0.01 { 2 }
            else { 1 };
        let amount_decimals = price_decimals + 2; // per ROUNDING_CONFIG

        let price_mult = 10_f64.powi(price_decimals);
        let price = (price * price_mult).round() / price_mult;
        let size = (size * 100.0).floor() / 100.0; // size always 2 decimals

        if size <= 0.0 {
            anyhow::bail!("Size too small after rounding: {}", size);
        }

        let amount_mult = 10_f64.powi(amount_decimals);
        // USDC has 6 decimal base (1 USDC = 1_000_000 units)
        let usdc_raw_mult = 1_000_000.0;
        // Rounding step: round to `amount_decimals` decimals in USDC terms
        let round_step = usdc_raw_mult / amount_mult;

        let (maker_amount, taker_amount) = match side {
            Side::Buy => {
                let usdc = size * price;
                let maker = ((usdc * usdc_raw_mult / round_step).round() as u64) * (round_step as u64);
                let taker = ((size * 100.0).floor() as u64) * 10_000;
                (U256::from(maker), U256::from(taker))
            }
            Side::Sell => {
                let usdc = size * price;
                let maker = ((size * 100.0).floor() as u64) * 10_000;
                let taker = ((usdc * usdc_raw_mult / round_step).round() as u64) * (round_step as u64);
                (U256::from(maker), U256::from(taker))
            }
        };

        // Parse token_id as U256
        let token_id = U256::from_str_radix(token_id, 10)
            .context("Failed to parse token_id as U256")?;

        // Salt: timestamp * random [0,1) like Python's py_order_utils
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let rng: f64 = rand::random();
        let salt = U256::from((now as f64 * rng) as u64);

        Ok(Self {
            salt,
            maker,
            signer,
            taker: Address::ZERO,
            token_id,
            maker_amount,
            taker_amount,
            expiration: U256::ZERO,
            nonce: U256::ZERO,
            fee_rate_bps: U256::from(fee_rate_bps),
            side,
            signature_type: 1, // POLY_PROXY
        })
    }

    /// EIP-712 struct hash
    fn struct_hash(&self) -> FixedBytes<32> {
        let mut buf = Vec::with_capacity(32 * 13);
        buf.extend_from_slice(order_type_hash().as_slice());
        buf.extend_from_slice(&self.salt.to_be_bytes::<32>());
        buf.extend_from_slice(&addr_to_bytes32(self.maker));
        buf.extend_from_slice(&addr_to_bytes32(self.signer));
        buf.extend_from_slice(&addr_to_bytes32(self.taker));
        buf.extend_from_slice(&self.token_id.to_be_bytes::<32>());
        buf.extend_from_slice(&self.maker_amount.to_be_bytes::<32>());
        buf.extend_from_slice(&self.taker_amount.to_be_bytes::<32>());
        buf.extend_from_slice(&self.expiration.to_be_bytes::<32>());
        buf.extend_from_slice(&self.nonce.to_be_bytes::<32>());
        buf.extend_from_slice(&self.fee_rate_bps.to_be_bytes::<32>());
        buf.extend_from_slice(&U256::from(self.side.as_u8()).to_be_bytes::<32>());
        buf.extend_from_slice(&U256::from(self.signature_type).to_be_bytes::<32>());
        keccak256(&buf)
    }

    /// Sign the order with EIP-712
    pub async fn sign(&self, signer: &PrivateKeySigner, neg_risk: bool) -> Result<String> {
        let domain_sep = domain_separator(neg_risk);
        let struct_hash = self.struct_hash();

        // \x19\x01 + domain separator + struct hash
        let mut digest = Vec::with_capacity(66);
        digest.push(0x19);
        digest.push(0x01);
        digest.extend_from_slice(domain_sep.as_slice());
        digest.extend_from_slice(struct_hash.as_slice());
        let hash = keccak256(&digest);

        let sig = signer.sign_hash(&hash).await
            .context("Failed to sign order")?;

        // r (32) + s (32) + v (1)
        let mut sig_bytes = Vec::with_capacity(65);
        sig_bytes.extend_from_slice(&sig.r().to_be_bytes::<32>());
        sig_bytes.extend_from_slice(&sig.s().to_be_bytes::<32>());
        sig_bytes.push(if sig.v() { 28 } else { 27 });

        Ok(format!("0x{}", hex::encode(&sig_bytes)))
    }

    /// Serialize to JSON for POST /order
    pub fn to_json(&self, signature: &str, owner: &str, order_type: &str) -> serde_json::Value {
        json!({
            "order": {
                "salt": self.salt.to::<u128>(),
                "maker": format!("{}", self.maker),
                "signer": format!("{}", self.signer),
                "taker": format!("{}", self.taker),
                "tokenId": self.token_id.to_string(),
                "makerAmount": self.maker_amount.to_string(),
                "takerAmount": self.taker_amount.to_string(),
                "expiration": self.expiration.to_string(),
                "nonce": self.nonce.to_string(),
                "feeRateBps": self.fee_rate_bps.to_string(),
                "side": self.side.as_str(),
                "signatureType": self.signature_type,
                "signature": signature
            },
            "owner": owner,
            "orderType": order_type,
            "postOnly": false
        })
    }
}

fn to_token_decimals(x: f64) -> U256 {
    U256::from((x * 1_000_000.0) as u64)
}

fn addr_to_bytes32(addr: Address) -> [u8; 32] {
    let mut padded = [0u8; 32];
    padded[12..].copy_from_slice(addr.as_slice());
    padded
}

/// Build, sign, and optionally post an order (default tick_size 0.01)
pub async fn place_order(
    client: &crate::api::client::PolymarketClient,
    token_id: &str,
    side: Side,
    price: f64,
    size: f64,
    neg_risk: bool,
    dry_run: bool,
) -> Result<serde_json::Value> {
    place_order_with_tick(client, token_id, side, price, size, neg_risk, dry_run, 0.01).await
}

/// Build, sign, and optionally post an order with specific tick size
pub async fn place_order_with_tick(
    client: &crate::api::client::PolymarketClient,
    token_id: &str,
    side: Side,
    price: f64,
    size: f64,
    neg_risk: bool,
    dry_run: bool,
    tick_size: f64,
) -> Result<serde_json::Value> {
    let private_key = std::env::var("POLY_PRIVATE_KEY")
        .context("POLY_PRIVATE_KEY not set in .env")?;
    let proxy_wallet: Address = std::env::var("POLY_PROXY_WALLET")
        .context("POLY_PROXY_WALLET not set")?
        .parse()
        .context("Invalid POLY_PROXY_WALLET address")?;
    let eoa_wallet: Address = std::env::var("POLY_WALLET_ADDRESS")
        .context("POLY_WALLET_ADDRESS not set")?
        .parse()
        .context("Invalid POLY_WALLET_ADDRESS address")?;
    let api_key = std::env::var("POLY_API_KEY")
        .context("POLY_API_KEY not set")?;

    let signer: PrivateKeySigner = private_key.parse()
        .context("Failed to parse private key")?;

    // Fetch fee rate (default to 0)
    let fee_rate_bps = match client.get_fee_rate(&format!("{:#x}", proxy_wallet)).await {
        Ok(rate) => rate,
        Err(e) => {
            debug!("Failed to fetch fee rate, defaulting to 0: {}", e);
            0
        }
    };
    info!("Fee rate: {} bps", fee_rate_bps);

    // Minimum trade size check: skip trades smaller than $0.50
    let trade_value = price * size;
    if trade_value < 0.50 {
        warn!("Trade value ${:.4} is below minimum $0.50 — skipping (price={}, size={})", trade_value, price, size);
        anyhow::bail!("Trade value ${:.4} is below minimum $0.50", trade_value);
    }

    let order = Order::new_with_tick(proxy_wallet, eoa_wallet, token_id, side, price, size, fee_rate_bps, tick_size)?;

    // Verify maker and taker amounts are > 0
    if order.maker_amount == U256::ZERO || order.taker_amount == U256::ZERO {
        warn!("Order amounts rounded to zero — skipping (maker={}, taker={})", order.maker_amount, order.taker_amount);
        anyhow::bail!("Order amounts rounded to zero (maker={}, taker={})", order.maker_amount, order.taker_amount);
    }

    info!(
        "Order: {} {} shares @ ${:.4} (maker={}, taker={})",
        side.as_str(),
        size,
        price,
        order.maker_amount,
        order.taker_amount
    );

    let signature = order.sign(&signer, neg_risk).await?;
    let body = order.to_json(&signature, &api_key, "GTC");

    if dry_run {
        println!("\n🔍 DRY RUN - Order would be posted:");
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(body);
    }

    info!("Posting order to CLOB API...");
    let result = client.post_order(&body).await?;
    info!("Order posted successfully");
    Ok(result)
}
