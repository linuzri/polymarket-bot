use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use uuid::Uuid;

const DEFAULT_BALANCE: f64 = 1000.0;
const STATE_FILE: &str = "paper_account.json";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum TokenSide {
    Yes,
    No,
}

impl std::fmt::Display for TokenSide {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TokenSide::Yes => write!(f, "Yes"),
            TokenSide::No => write!(f, "No"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum TradeSide {
    Buy,
    Sell,
}

impl std::fmt::Display for TradeSide {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TradeSide::Buy => write!(f, "Buy"),
            TradeSide::Sell => write!(f, "Sell"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Position {
    pub token_id: String,
    pub market_question: String,
    pub side: TokenSide,
    pub quantity: f64,
    pub avg_entry_price: f64,
    pub current_price: f64,
}

impl Position {
    pub fn market_value(&self) -> f64 {
        self.quantity * self.current_price
    }

    pub fn unrealized_pnl(&self) -> f64 {
        self.quantity * (self.current_price - self.avg_entry_price)
    }

    pub fn cost_basis(&self) -> f64 {
        self.quantity * self.avg_entry_price
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaperTrade {
    pub id: String,
    pub timestamp: DateTime<Utc>,
    pub token_id: String,
    pub market_question: String,
    pub side: TradeSide,
    pub token_side: TokenSide,
    pub quantity: f64,
    pub price: f64,
    pub total_cost: f64,
    pub pnl: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaperAccount {
    pub balance: f64,
    pub positions: HashMap<String, Position>,
    pub trade_history: Vec<PaperTrade>,
    pub created_at: DateTime<Utc>,
}

impl PaperAccount {
    pub fn new() -> Self {
        Self {
            balance: DEFAULT_BALANCE,
            positions: HashMap::new(),
            trade_history: Vec::new(),
            created_at: Utc::now(),
        }
    }

    /// Load from file or create new
    pub fn load() -> Result<Self> {
        let path = Path::new(STATE_FILE);
        if path.exists() {
            let data = std::fs::read_to_string(path)
                .context("Failed to read paper account state")?;
            let account: PaperAccount = serde_json::from_str(&data)
                .context("Failed to parse paper account state")?;
            Ok(account)
        } else {
            Ok(Self::new())
        }
    }

    /// Save state to file
    pub fn save(&self) -> Result<()> {
        let data = serde_json::to_string_pretty(self)
            .context("Failed to serialize paper account")?;
        std::fs::write(STATE_FILE, data)
            .context("Failed to write paper account state")?;
        Ok(())
    }

    /// Buy tokens
    pub fn buy(
        &mut self,
        token_id: &str,
        market_question: &str,
        side: TokenSide,
        quantity: f64,
        price: f64,
    ) -> Result<&PaperTrade> {
        let total_cost = quantity * price;

        if total_cost > self.balance {
            bail!(
                "Insufficient balance: need ${:.2} but only have ${:.2}",
                total_cost,
                self.balance
            );
        }

        self.balance -= total_cost;

        // Update or create position
        if let Some(pos) = self.positions.get_mut(token_id) {
            let total_qty = pos.quantity + quantity;
            pos.avg_entry_price =
                (pos.avg_entry_price * pos.quantity + price * quantity) / total_qty;
            pos.quantity = total_qty;
            pos.current_price = price;
        } else {
            self.positions.insert(
                token_id.to_string(),
                Position {
                    token_id: token_id.to_string(),
                    market_question: market_question.to_string(),
                    side: side.clone(),
                    quantity,
                    avg_entry_price: price,
                    current_price: price,
                },
            );
        }

        let trade = PaperTrade {
            id: Uuid::new_v4().to_string(),
            timestamp: Utc::now(),
            token_id: token_id.to_string(),
            market_question: market_question.to_string(),
            side: TradeSide::Buy,
            token_side: side,
            quantity,
            price,
            total_cost,
            pnl: None,
        };

        self.trade_history.push(trade);
        self.save()?;

        Ok(self.trade_history.last().unwrap())
    }

    /// Sell tokens
    pub fn sell(
        &mut self,
        token_id: &str,
        quantity: f64,
        price: f64,
    ) -> Result<&PaperTrade> {
        let pos = self.positions.get(token_id)
            .ok_or_else(|| anyhow::anyhow!("No position found for token {}", token_id))?;

        if quantity > pos.quantity {
            bail!(
                "Insufficient quantity: trying to sell {:.2} but only hold {:.2}",
                quantity,
                pos.quantity
            );
        }

        let pnl = quantity * (price - pos.avg_entry_price);
        let total_cost = quantity * price;
        let market_question = pos.market_question.clone();
        let token_side = pos.side.clone();

        self.balance += total_cost;

        // Update position
        let pos = self.positions.get_mut(token_id).unwrap();
        pos.quantity -= quantity;
        pos.current_price = price;

        if pos.quantity < 0.0001 {
            self.positions.remove(token_id);
        }

        let trade = PaperTrade {
            id: Uuid::new_v4().to_string(),
            timestamp: Utc::now(),
            token_id: token_id.to_string(),
            market_question,
            side: TradeSide::Sell,
            token_side,
            quantity,
            price,
            total_cost,
            pnl: Some(pnl),
        };

        self.trade_history.push(trade);
        self.save()?;

        Ok(self.trade_history.last().unwrap())
    }

    /// Total portfolio value (balance + positions)
    pub fn portfolio_value(&self) -> f64 {
        self.balance + self.positions.values().map(|p| p.market_value()).sum::<f64>()
    }

    /// Total unrealized P/L across all positions
    pub fn unrealized_pnl(&self) -> f64 {
        self.positions.values().map(|p| p.unrealized_pnl()).sum()
    }

    /// Total realized P/L from trade history
    pub fn realized_pnl(&self) -> f64 {
        self.trade_history
            .iter()
            .filter_map(|t| t.pnl)
            .sum()
    }

    /// Reset account
    pub fn reset(&mut self) -> Result<()> {
        *self = Self::new();
        self.save()?;
        Ok(())
    }

    /// Update current prices for all positions
    pub fn update_position_price(&mut self, token_id: &str, price: f64) {
        if let Some(pos) = self.positions.get_mut(token_id) {
            pos.current_price = price;
        }
    }
}
