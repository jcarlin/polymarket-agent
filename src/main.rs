use anyhow::Result;
use tracing::{error, info};

use polymarket_agent::config::Config;
use polymarket_agent::db::Database;
use polymarket_agent::market_scanner::MarketScanner;
use polymarket_agent::sidecar::SidecarProcess;

#[tokio::main]
async fn main() -> Result<()> {
    // Load configuration
    let config = Config::from_env()?;

    // Initialize logging
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("polymarket_agent=info")),
        )
        .init();

    info!("Polymarket Agent starting in {} mode", config.trading_mode);

    // Open database
    let _db = Database::open(&config.database_path)?;
    info!("Database initialized at {}", config.database_path);

    // Spawn Python sidecar (non-fatal if it fails)
    let mut sidecar = match SidecarProcess::spawn(&config).await {
        Ok(s) => {
            info!("Sidecar spawned and healthy");
            Some(s)
        }
        Err(e) => {
            error!("Failed to spawn sidecar (continuing without it): {}", e);
            None
        }
    };

    // Scan markets
    let scanner = MarketScanner::new(&config)?;
    match scanner.scan_and_filter().await {
        Ok(markets) => {
            info!("Found {} markets after filtering", markets.len());
            for (i, market) in markets.iter().take(5).enumerate() {
                info!(
                    "  [{}] {} (vol: {:.0}, liq: {:.0})",
                    i + 1,
                    market.question,
                    market.volume.unwrap_or(0.0),
                    market.liquidity.unwrap_or(0.0),
                );
            }
        }
        Err(e) => {
            error!("Market scan failed: {}", e);
        }
    }

    // Shutdown
    if let Some(ref mut s) = sidecar {
        s.shutdown();
    }
    info!("Polymarket Agent shut down cleanly");

    Ok(())
}
