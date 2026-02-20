mod api;
mod arbitrage;
mod auth;
mod btc5min;
mod models;
mod notifications;
mod orders;
mod paper;
mod portfolio;
mod strategy;
mod signals;
mod weather;

use anyhow::Result;
use clap::{Parser, Subcommand};
use tracing::{info, warn};

#[derive(Parser)]
#[command(name = "polymarket-bot", about = "Automated Polymarket trading bot")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// List active markets with volume and pricing
    Markets {
        /// Filter by keyword
        #[arg(short, long)]
        query: Option<String>,
        /// Number of markets to show
        #[arg(short, long, default_value = "20")]
        limit: usize,
    },
    /// Get detailed info about a specific market
    Market {
        /// Market slug or condition ID
        id: String,
    },
    /// Get the order book for a market
    Book {
        /// Token ID
        token_id: String,
    },
    /// Stream live price updates via WebSocket
    Stream {
        /// Market slugs to watch (comma-separated)
        #[arg(short, long)]
        markets: Option<String>,
    },
    /// Show account balance and positions
    Account,
    /// Run the automated strategy engine
    Run {
        /// Strategy to use
        #[arg(short, long, default_value = "value")]
        strategy: String,
        /// Force dry run (no real trades)
        #[arg(long)]
        dry_run: bool,
    },
    /// Buy shares on a market (real order)
    Buy {
        /// Market slug
        market_slug: String,
        /// Token side: yes or no
        side: String,
        /// Amount in USD to spend
        amount_usd: f64,
        /// Dry run - build and sign but don't post
        #[arg(long)]
        dry_run: bool,
    },
    /// Sell shares on a market (real order)
    Sell {
        /// Market slug
        market_slug: String,
        /// Token side: yes or no
        side: String,
        /// Number of shares to sell
        amount_shares: f64,
        /// Dry run - build and sign but don't post
        #[arg(long)]
        dry_run: bool,
    },
    /// Paper trading commands
    Paper {
        #[command(subcommand)]
        action: PaperCommands,
    },
    /// Show real portfolio: open positions, resolved, P/L
    Portfolio,
    /// BTC 5-minute Polymarket trading bot
    Btc5min {
        #[command(subcommand)]
        action: Btc5minCommands,
    },
    /// Run the arbitrage scanner (buys YES+NO when sum < $1)
    Arb {
        /// Force dry run (no real trades)
        #[arg(long)]
        dry_run: bool,
    },
    /// Run the weather arbitrage strategy
    Weather {
        /// Force dry run (no real trades)
        #[arg(long)]
        dry_run: bool,
        /// Run once (don't loop)
        #[arg(long)]
        once: bool,
    },
}

#[derive(Subcommand)]
enum Btc5minCommands {
    /// Run the BTC 5-min trading loop
    Run {
        /// Force dry run
        #[arg(long)]
        dry_run: bool,
    },
    /// Check the next available BTC 5-min market
    Check,
    /// Get a prediction from the ML ensemble
    Predict,
    /// Show trade results and stats
    Stats,
}

#[derive(Subcommand)]
enum PaperCommands {
    /// Buy tokens with paper money
    Buy {
        /// Market slug
        market_slug: String,
        /// Token side: yes or no
        side: String,
        /// Amount in USD to spend
        amount: f64,
    },
    /// Sell tokens
    Sell {
        /// Market slug
        market_slug: String,
        /// Token side: yes or no
        side: String,
        /// Amount in USD worth to sell
        amount: f64,
    },
    /// Show portfolio with positions and P/L
    Portfolio,
    /// Show trade history
    History,
    /// Reset account to $1000
    Reset,
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "polymarket_bot=info".into()),
        )
        .init();

    // Load .env if present (override system env vars)
    dotenvy::dotenv_override().ok();

    let cli = Cli::parse();
    let client = api::client::PolymarketClient::new()?;

    match cli.command {
        Commands::Markets { query, limit } => {
            let markets = client.get_markets(query.as_deref(), limit).await?;
            println!("\n{:<50} {:>10} {:>8} {:>8}", "Market", "Volume", "Yes", "No");
            println!("{}", "-".repeat(80));
            for m in &markets {
                println!(
                    "{:<50} {:>10} {:>7.1}% {:>7.1}%",
                    truncate(&m.question, 48),
                    format_volume(m.volume),
                    m.yes_price * 100.0,
                    m.no_price * 100.0,
                );
            }
            println!("\nShowing {} markets", markets.len());
        }
        Commands::Market { id } => {
            let market = client.get_market(&id).await?;
            println!("\n📊 {}", market.question);
            println!("   Volume: {}", format_volume(market.volume));
            println!("   Yes: {:.1}%  |  No: {:.1}%", market.yes_price * 100.0, market.no_price * 100.0);
            println!("   End: {}", market.end_date.unwrap_or_default());
            if let Some(desc) = &market.description {
                println!("\n   {}", truncate(desc, 200));
            }
        }
        Commands::Book { token_id } => {
            let book = client.get_order_book(&token_id).await?;
            println!("\n📖 Order Book for {}", truncate(&token_id, 20));
            println!("\n{:>10} {:>10}  |  {:>10} {:>10}", "Bid Size", "Bid", "Ask", "Ask Size");
            println!("{}", "-".repeat(50));
            let max_rows = book.bids.len().max(book.asks.len()).min(10);
            for i in 0..max_rows {
                let bid = book.bids.get(i);
                let ask = book.asks.get(i);
                println!(
                    "{:>10} {:>10}  |  {:>10} {:>10}",
                    bid.map(|b| format!("{:.0}", b.size)).unwrap_or_default(),
                    bid.map(|b| format!("{:.2}", b.price)).unwrap_or_default(),
                    ask.map(|a| format!("{:.2}", a.price)).unwrap_or_default(),
                    ask.map(|a| format!("{:.0}", a.size)).unwrap_or_default(),
                );
            }
        }
        Commands::Stream { markets } => {
            info!("Starting WebSocket stream...");
            warn!("WebSocket streaming not yet implemented");
            // TODO: Implement WebSocket streaming
        }
        Commands::Account => {
            // Show wallet addresses
            if let Ok(addr) = std::env::var("POLY_WALLET_ADDRESS") {
                println!("\n👤 EOA Wallet: {}", addr);
            }
            if let Ok(funder) = std::env::var("POLY_PROXY_WALLET") {
                println!("💳 Proxy Wallet (funds): {}", funder);
            }

            match client.get_profile().await {
                Ok(profile) => {
                    println!("\n👤 Account Profile");
                    if let Some(obj) = profile.as_object() {
                        for (k, v) in obj {
                            println!("   {}: {}", k, v);
                        }
                    } else {
                        println!("   {}", profile);
                    }
                }
                Err(e) => {
                    warn!("Failed to fetch profile: {}", e);
                    println!("\n⚠️  Could not fetch profile: {}", e);
                }
            }

            match client.get_balance().await {
                Ok(balance) => {
                    println!("\n💰 USDC Balance: ${:.2}", balance);
                }
                Err(e) => {
                    warn!("Failed to fetch balance: {}", e);
                    println!("⚠️  Could not fetch balance: {}", e);
                }
            }

            match client.get_positions().await {
                Ok(positions) => {
                    println!("\n📊 Open Positions:");
                    if positions.is_empty() {
                        println!("   No open positions");
                    } else {
                        for pos in &positions {
                            println!("   {}", pos);
                        }
                    }
                }
                Err(e) => {
                    warn!("Failed to fetch positions: {}", e);
                    println!("⚠️  Could not fetch positions: {}", e);
                }
            }
        }
        Commands::Run { strategy, dry_run } => {
            info!("Starting bot with strategy: {}", strategy);
            let config = strategy::config::StrategyConfig::load()?;
            let mut engine = strategy::engine::StrategyEngine::new(config, dry_run)?;
            engine.run().await?;
        }
        Commands::Buy { market_slug, side, amount_usd, dry_run } => {
            let market = client.get_market(&market_slug).await?;
            let tokens = market.tokens.as_ref()
                .ok_or_else(|| anyhow::anyhow!("Market has no token IDs"))?;
            let token_idx = match side.to_lowercase().as_str() {
                "yes" => 0,
                "no" => 1,
                _ => anyhow::bail!("Side must be 'yes' or 'no'"),
            };
            let token_id = tokens.get(token_idx)
                .ok_or_else(|| anyhow::anyhow!("Token ID not found for {} side", side))?;

            // Get best ask price from order book
            let book = client.get_order_book(token_id).await?;
            let price = book.asks.first()
                .map(|a| a.price)
                .ok_or_else(|| anyhow::anyhow!("No asks in order book"))?;

            let size = amount_usd / price;
            let neg_risk = client.get_neg_risk(&market_slug).await.unwrap_or(true);

            println!("📊 {} - {}", market.question, side.to_uppercase());
            println!("   Price: ${:.4}  Size: {:.2} shares  Cost: ${:.2}", price, size, amount_usd);
            println!("   Neg risk: {}  Token: {}...{}", neg_risk, &token_id[..8], &token_id[token_id.len()-4..]);

            let result = orders::place_order(&client, token_id, orders::Side::Buy, price, size, neg_risk, dry_run).await?;
            if !dry_run {
                println!("\n✅ Order placed: {}", serde_json::to_string_pretty(&result)?);
            }
        }
        Commands::Sell { market_slug, side, amount_shares, dry_run } => {
            let market = client.get_market(&market_slug).await?;
            let tokens = market.tokens.as_ref()
                .ok_or_else(|| anyhow::anyhow!("Market has no token IDs"))?;
            let token_idx = match side.to_lowercase().as_str() {
                "yes" => 0,
                "no" => 1,
                _ => anyhow::bail!("Side must be 'yes' or 'no'"),
            };
            let token_id = tokens.get(token_idx)
                .ok_or_else(|| anyhow::anyhow!("Token ID not found for {} side", side))?;

            // Get best bid price from order book
            let book = client.get_order_book(token_id).await?;
            let price = book.bids.first()
                .map(|b| b.price)
                .ok_or_else(|| anyhow::anyhow!("No bids in order book"))?;

            let neg_risk = client.get_neg_risk(&market_slug).await.unwrap_or(true);

            println!("📊 {} - {} SELL", market.question, side.to_uppercase());
            println!("   Price: ${:.4}  Size: {:.2} shares  Value: ${:.2}", price, amount_shares, amount_shares * price);
            println!("   Neg risk: {}  Token: {}...{}", neg_risk, &token_id[..8], &token_id[token_id.len()-4..]);

            let result = orders::place_order(&client, token_id, orders::Side::Sell, price, amount_shares, neg_risk, dry_run).await?;
            if !dry_run {
                println!("\n✅ Order placed: {}", serde_json::to_string_pretty(&result)?);
            }
        }
        Commands::Paper { action } => {
            handle_paper(action, &client).await?;
        }
        Commands::Btc5min { action } => {
            handle_btc5min(action, &client).await?;
        }
        Commands::Arb { dry_run } => {
            let mut scanner = arbitrage::ArbScanner::new(dry_run);
            scanner.run().await?;
        }
        Commands::Weather { dry_run, once } => {
            // Load weather config from config.toml [weather] section
            #[derive(serde::Deserialize, Default)]
            struct FullConfig {
                #[serde(default)]
                weather: weather::WeatherConfig,
            }
            let weather_config = match std::fs::read_to_string("config.toml") {
                Ok(toml_str) => {
                    toml::from_str::<FullConfig>(&toml_str)
                        .map(|c| c.weather)
                        .unwrap_or_default()
                }
                Err(_) => weather::WeatherConfig::default(),
            };
            println!("Weather config: max_per_bucket=${:.0}, max_total_exposure=${:.0}, kelly={:.0}%",
                weather_config.max_per_bucket, weather_config.max_total_exposure, weather_config.kelly_fraction * 100.0);
            let mut strategy = weather::strategy::WeatherStrategy::new(weather_config, dry_run);
            if once {
                strategy.run_once().await?;
            } else {
                strategy.run_loop().await?;
            }
        }
        Commands::Portfolio => {
            let mut state = portfolio::PortfolioState::load()?;
            // Sync any trades from strategy log
            portfolio::sync_from_trade_log(&mut state)?;
            // Update current prices
            portfolio::update_prices(&mut state, &client).await?;
            // Check for resolutions
            let resolved = portfolio::check_resolutions(&mut state, &client).await?;
            if !resolved.is_empty() {
                let notifier = notifications::TelegramNotifier::new();
                portfolio::alert_resolutions(&resolved, &notifier).await;
            }
            state.save()?;
            // Print summary
            portfolio::print_summary(&state);
        }
    }

    Ok(())
}

async fn handle_btc5min(action: Btc5minCommands, client: &api::client::PolymarketClient) -> anyhow::Result<()> {
    match action {
        Btc5minCommands::Run { dry_run } => {
            let mut config = btc5min::load_config()?;
            if dry_run {
                config.dry_run = true;
            }
            btc5min::run_loop(config).await?;
        }
        Btc5minCommands::Check => {
            println!("Searching for BTC 5-min markets...");
            match btc5min::find_btc5min_market(client).await? {
                Some(market) => {
                    println!("\nFound: {}", market.question);
                    println!("  Slug: {}", market.slug);
                    println!("  Condition ID: {}", market.condition_id);
                    println!("  Up token:   {}", market.up_token_id);
                    println!("  Down token: {}", market.down_token_id);
                    println!("  Neg risk: {}", market.neg_risk);
                }
                None => {
                    println!("No BTC 5-min market found. Markets may not be created yet for the next window.");
                }
            }
        }
        Btc5minCommands::Predict => {
            println!("Getting BTC prediction from ML ensemble...");
            match btc5min::get_prediction().await {
                Ok(pred) => {
                    if let Some(err) = &pred.error {
                        println!("Prediction error: {}", err);
                    } else {
                        println!("\nSignal: {}", pred.signal.as_deref().unwrap_or("?"));
                        println!("Confidence: {:.1}%", pred.confidence.unwrap_or(0.0) * 100.0);
                        if let Some(models) = &pred.models {
                            println!("Models: RF={} XGB={} LGB={}",
                                models.get("rf").map(|s| s.as_str()).unwrap_or("?"),
                                models.get("xgb").map(|s| s.as_str()).unwrap_or("?"),
                                models.get("lgb").map(|s| s.as_str()).unwrap_or("?"));
                        }
                        if let Some(confs) = &pred.model_confidences {
                            println!("Confidences: RF={:.1}% XGB={:.1}% LGB={:.1}%",
                                confs.get("rf").unwrap_or(&0.0) * 100.0,
                                confs.get("xgb").unwrap_or(&0.0) * 100.0,
                                confs.get("lgb").unwrap_or(&0.0) * 100.0);
                        }
                    }
                }
                Err(e) => println!("Failed: {}", e),
            }
        }
        Btc5minCommands::Stats => {
            let tracker = btc5min::ResultsTracker::load();
            println!("\nBTC 5-min Trading Stats");
            println!("=======================");
            println!("Total trades: {}", tracker.total_trades);
            println!("Wins: {}", tracker.wins);
            println!("Losses: {}", tracker.losses);
            println!("Skipped: {}", tracker.skipped);
            println!("Win rate: {:.1}%", tracker.win_rate() * 100.0);
            println!("\nRecent trades:");
            for trade in tracker.trades.iter().rev().take(10) {
                let result = trade.resolved.as_deref().unwrap_or("pending");
                let mode = if trade.dry_run { "DRY" } else { "LIVE" };
                println!("  [{}] {} {} -> {} @ ${:.4} (conf: {:.0}%) [{}]",
                    mode, trade.timestamp.chars().take(16).collect::<String>(),
                    trade.signal, trade.side, trade.price,
                    trade.confidence * 100.0, result);
            }
        }
    }
    Ok(())
}

async fn handle_paper(action: PaperCommands, client: &api::client::PolymarketClient) -> anyhow::Result<()> {
    let mut account = paper::PaperAccount::load()?;

    match action {
        PaperCommands::Buy { market_slug, side, amount } => {
            let token_side = match side.to_lowercase().as_str() {
                "yes" => paper::TokenSide::Yes,
                "no" => paper::TokenSide::No,
                _ => anyhow::bail!("Side must be 'yes' or 'no'"),
            };

            let market = client.get_market(&market_slug).await?;
            let tokens = market.tokens.as_ref()
                .ok_or_else(|| anyhow::anyhow!("Market has no token IDs"))?;

            let token_idx = match token_side {
                paper::TokenSide::Yes => 0,
                paper::TokenSide::No => 1,
            };

            let token_id = tokens.get(token_idx)
                .ok_or_else(|| anyhow::anyhow!("Token ID not found for {:?} side", token_side))?;

            let book = client.get_order_book(token_id).await?;
            let ask_price = book.asks.first()
                .map(|a| a.price)
                .ok_or_else(|| anyhow::anyhow!("No asks in order book"))?;

            let quantity = amount / ask_price;

            let trade = account.buy(token_id, &market.question, token_side, quantity, ask_price)?;
            println!("\n✅ Paper BUY executed!");
            println!("   Market: {}", trade.market_question);
            println!("   Side: {} {}", trade.token_side, trade.side);
            println!("   Quantity: {:.2} tokens @ ${:.4}", trade.quantity, trade.price);
            println!("   Total: ${:.2}", trade.total_cost);
            println!("   Balance: ${:.2}", account.balance);
        }
        PaperCommands::Sell { market_slug, side, amount } => {
            let token_side = match side.to_lowercase().as_str() {
                "yes" => paper::TokenSide::Yes,
                "no" => paper::TokenSide::No,
                _ => anyhow::bail!("Side must be 'yes' or 'no'"),
            };

            let market = client.get_market(&market_slug).await?;
            let tokens = market.tokens.as_ref()
                .ok_or_else(|| anyhow::anyhow!("Market has no token IDs"))?;

            let token_idx = match token_side {
                paper::TokenSide::Yes => 0,
                paper::TokenSide::No => 1,
            };

            let token_id = tokens.get(token_idx)
                .ok_or_else(|| anyhow::anyhow!("Token ID not found for {:?} side", token_side))?;

            let book = client.get_order_book(token_id).await?;
            let bid_price = book.bids.first()
                .map(|b| b.price)
                .ok_or_else(|| anyhow::anyhow!("No bids in order book"))?;

            let quantity = amount / bid_price;

            let trade = account.sell(token_id, quantity, bid_price)?;
            println!("\n✅ Paper SELL executed!");
            println!("   Market: {}", trade.market_question);
            println!("   Side: {} {}", trade.token_side, trade.side);
            println!("   Quantity: {:.2} tokens @ ${:.4}", trade.quantity, trade.price);
            println!("   Total: ${:.2}", trade.total_cost);
            println!("   P/L: {}", trade.pnl.map(|p| format!("${:.2}", p)).unwrap_or("N/A".into()));
            println!("   Balance: ${:.2}", account.balance);
        }
        PaperCommands::Portfolio => {
            // Update current prices for all positions
            for (token_id, _pos) in account.positions.clone().iter() {
                if let Ok(book) = client.get_order_book(token_id).await {
                    account.update_position_price(token_id, book.mid_price);
                }
            }
            account.save()?;

            println!("\n💰 Paper Trading Portfolio");
            println!("   Balance: ${:.2}", account.balance);
            println!("   Created: {}", account.created_at.format("%Y-%m-%d %H:%M UTC"));
            println!("\n{:<40} {:>6} {:>8} {:>8} {:>8} {:>10}",
                "Market", "Side", "Qty", "Entry", "Current", "P/L");
            println!("{}", "-".repeat(84));

            if account.positions.is_empty() {
                println!("   No open positions");
            } else {
                for pos in account.positions.values() {
                    let pnl = pos.unrealized_pnl();
                    let pnl_str = if pnl >= 0.0 {
                        format!("+${:.2}", pnl)
                    } else {
                        format!("-${:.2}", pnl.abs())
                    };
                    println!(
                        "{:<40} {:>6} {:>8.2} {:>7.4} {:>7.4} {:>10}",
                        truncate(&pos.market_question, 38),
                        pos.side,
                        pos.quantity,
                        pos.avg_entry_price,
                        pos.current_price,
                        pnl_str,
                    );
                }
            }

            println!("\n   Portfolio Value: ${:.2}", account.portfolio_value());
            println!("   Unrealized P/L: ${:.2}", account.unrealized_pnl());
            println!("   Realized P/L:   ${:.2}", account.realized_pnl());
            println!("   Total P/L:      ${:.2}", account.unrealized_pnl() + account.realized_pnl());
        }
        PaperCommands::History => {
            println!("\n📜 Trade History");
            println!("{:<20} {:<6} {:<4} {:>8} {:>8} {:>10} {:>10}",
                "Time", "Side", "Tkn", "Qty", "Price", "Total", "P/L");
            println!("{}", "-".repeat(70));

            if account.trade_history.is_empty() {
                println!("   No trades yet");
            } else {
                for trade in account.trade_history.iter().rev() {
                    let pnl_str = trade.pnl
                        .map(|p| format!("${:.2}", p))
                        .unwrap_or_else(|| "-".to_string());
                    println!(
                        "{:<20} {:<6} {:<4} {:>8.2} {:>7.4} {:>9.2} {:>10}",
                        trade.timestamp.format("%m-%d %H:%M"),
                        trade.side,
                        trade.token_side,
                        trade.quantity,
                        trade.price,
                        trade.total_cost,
                        pnl_str,
                    );
                }
            }
            println!("\nTotal trades: {}", account.trade_history.len());
        }
        PaperCommands::Reset => {
            account.reset()?;
            println!("\n🔄 Paper account reset to $1000.00");
        }
    }

    Ok(())
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}...", &s[..max - 3])
    }
}

fn format_volume(vol: f64) -> String {
    if vol >= 1_000_000.0 {
        format!("${:.1}M", vol / 1_000_000.0)
    } else if vol >= 1_000.0 {
        format!("${:.0}K", vol / 1_000.0)
    } else {
        format!("${:.0}", vol)
    }
}
