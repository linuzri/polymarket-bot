mod api;
mod models;
mod strategy;
mod signals;

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
    /// Run the trading bot
    Run {
        /// Strategy to use
        #[arg(short, long, default_value = "simple")]
        strategy: String,
    },
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

    // Load .env if present
    dotenvy::dotenv().ok();

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
            info!("Checking account...");
            warn!("Account management requires API key setup. See config.toml");
        }
        Commands::Run { strategy } => {
            info!("Starting bot with strategy: {}", strategy);
            warn!("Trading bot not yet implemented. Phase 1: data collection only.");
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
