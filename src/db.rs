use anyhow::{Context, Result};
use rusqlite::Connection;
use std::path::Path;

#[derive(Debug, Clone)]
pub struct TradeRow {
    pub trade_id: String,
    pub market_condition_id: String,
    pub side: String,
    pub price: f64,
    pub size: f64,
    pub status: String,
    pub paper: bool,
    pub created_at: String,
    pub question: Option<String>,
    pub realized_pnl: Option<f64>,
    pub unrealized_pnl: Option<f64>,
    pub position_status: Option<String>,
    pub entry_fee: f64,
}

#[derive(Debug, Clone)]
pub struct PositionRow {
    pub market_condition_id: String,
    pub token_id: String,
    pub side: String,
    pub entry_price: f64,
    pub size: f64,
    pub status: String,
    pub current_price: Option<f64>,
    pub unrealized_pnl: f64,
    pub estimated_probability: Option<f64>,
    pub question: Option<String>,
}

#[derive(Debug, Clone)]
pub struct WeatherSnapshotRow {
    pub cycle_number: i64,
    pub city: String,
    pub forecast_date: String,
    pub ensemble_mean: f64,
    pub ensemble_std: f64,
    pub gefs_count: i32,
    pub ecmwf_count: i32,
    pub bucket_data: String,
    pub created_at: String,
}

#[derive(Debug, Clone)]
pub struct WeatherActualRow {
    pub city: String,
    pub forecast_date: String,
    pub wu_actual_high: Option<f64>,
    pub nws_forecast_high: Option<f64>,
    pub ensemble_mean: Option<f64>,
    pub predicted_bucket: Option<String>,
    pub actual_bucket: Option<String>,
    pub prediction_error: Option<f64>,
    pub created_at: String,
}

#[derive(Debug, Clone)]
pub struct OpportunityRow {
    pub cycle_number: i64,
    pub condition_id: String,
    pub question: String,
    pub side: String,
    pub market_price: f64,
    pub estimated_probability: f64,
    pub edge: f64,
    pub confidence: f64,
    pub status: String,
    pub reject_reason: Option<String>,
    pub created_at: String,
}

pub struct Database {
    pub conn: Connection,
}

impl Database {
    pub fn open(path: &str) -> Result<Self> {
        // Create parent directories if needed
        if let Some(parent) = Path::new(path).parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).with_context(|| {
                    format!("Failed to create database directory: {}", parent.display())
                })?;
            }
        }

        let conn =
            Connection::open(path).with_context(|| format!("Failed to open database: {}", path))?;

        let db = Database { conn };
        db.run_migrations()?;
        db.enable_wal()?;
        Ok(db)
    }

    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory().context("Failed to open in-memory database")?;
        let db = Database { conn };
        db.run_migrations()?;
        Ok(db)
    }

    fn enable_wal(&self) -> Result<()> {
        self.conn
            .pragma_update(None, "journal_mode", "WAL")
            .context("Failed to enable WAL mode")?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn log_api_cost(
        &self,
        cycle_number: i64,
        market_condition_id: Option<&str>,
        model: &str,
        input_tokens: u64,
        output_tokens: u64,
        cost_usd: f64,
        call_type: &str,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO api_cost_log (cycle_number, market_condition_id, model, input_tokens, output_tokens, cost_usd, call_type) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![cycle_number, market_condition_id, model, input_tokens as i64, output_tokens as i64, cost_usd, call_type],
        ).context("Failed to log API cost")?;
        Ok(())
    }

    pub fn get_cycle_api_cost(&self, cycle_number: i64) -> Result<f64> {
        let cost: f64 = self
            .conn
            .query_row(
                "SELECT COALESCE(SUM(cost_usd), 0.0) FROM api_cost_log WHERE cycle_number = ?1",
                [cycle_number],
                |row| row.get(0),
            )
            .context("Failed to get cycle API cost")?;
        Ok(cost)
    }

    pub fn get_api_cost_since(&self, hours: u32) -> Result<f64> {
        let cost: f64 = self
            .conn
            .query_row(
                "SELECT COALESCE(SUM(cost_usd), 0.0) FROM api_cost_log WHERE created_at >= datetime('now', ?1)",
                [format!("-{} hours", hours)],
                |row| row.get(0),
            )
            .context("Failed to get API cost since")?;
        Ok(cost)
    }

    /// Insert or update a market record. Called after scanning so that FK constraints
    /// on trades/positions are satisfied when executing.
    pub fn upsert_market(&self, market: &crate::market_scanner::GammaMarket) -> Result<()> {
        let yes_token = market.tokens.iter().find(|t| t.outcome == "Yes");
        let no_token = market.tokens.iter().find(|t| t.outcome == "No");
        self.conn.execute(
            "INSERT INTO markets (condition_id, question, slug, yes_token_id, no_token_id, volume, liquidity, end_date, active)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT(condition_id) DO UPDATE SET
                question = excluded.question,
                volume = excluded.volume,
                liquidity = excluded.liquidity,
                active = excluded.active,
                updated_at = datetime('now')",
            rusqlite::params![
                market.condition_id.as_deref().unwrap_or(""),
                market.question,
                market.slug,
                yes_token.map(|t| t.token_id.as_str()),
                no_token.map(|t| t.token_id.as_str()),
                market.volume,
                market.liquidity,
                market.end_date,
                market.active,
            ],
        ).context("Failed to upsert market")?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn insert_trade(
        &self,
        trade_id: &str,
        market_condition_id: &str,
        token_id: &str,
        side: &str,
        price: f64,
        size: f64,
        status: &str,
        paper: bool,
        entry_fee: f64,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO trades (trade_id, market_condition_id, token_id, side, price, size, status, paper, entry_fee) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            rusqlite::params![trade_id, market_condition_id, token_id, side, price, size, status, paper, entry_fee],
        ).context("Failed to insert trade")?;
        Ok(())
    }

    pub fn upsert_position(
        &self,
        market_condition_id: &str,
        token_id: &str,
        side: &str,
        entry_price: f64,
        size: f64,
    ) -> Result<()> {
        // Check if an open position already exists for this market/side
        let existing: Option<(i64, f64, f64)> = self
            .conn
            .query_row(
                "SELECT id, entry_price, size FROM positions WHERE market_condition_id = ?1 AND side = ?2 AND status = 'open'",
                rusqlite::params![market_condition_id, side],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .ok();

        if let Some((id, old_price, old_size)) = existing {
            // Average entry price, aggregate size
            let total_size = old_size + size;
            let avg_price = (old_price * old_size + entry_price * size) / total_size;
            self.conn.execute(
                "UPDATE positions SET entry_price = ?1, size = ?2, token_id = ?3, updated_at = datetime('now') WHERE id = ?4",
                rusqlite::params![avg_price, total_size, token_id, id],
            ).context("Failed to update position")?;
        } else {
            self.conn.execute(
                "INSERT INTO positions (market_condition_id, token_id, side, entry_price, size, status) VALUES (?1, ?2, ?3, ?4, ?5, 'open')",
                rusqlite::params![market_condition_id, token_id, side, entry_price, size],
            ).context("Failed to insert position")?;
        }
        Ok(())
    }

    pub fn log_bankroll_entry(
        &self,
        entry_type: &str,
        amount: f64,
        balance_after: f64,
        description: &str,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO bankroll_log (entry_type, amount, balance_after, description) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![entry_type, amount, balance_after, description],
        ).context("Failed to log bankroll entry")?;
        Ok(())
    }

    pub fn get_current_bankroll(&self) -> Result<f64> {
        let balance: f64 = self
            .conn
            .query_row(
                "SELECT COALESCE((SELECT balance_after FROM bankroll_log ORDER BY id DESC LIMIT 1), 0.0)",
                [],
                |row| row.get(0),
            )
            .context("Failed to get current bankroll")?;
        Ok(balance)
    }

    pub fn get_total_exposure(&self) -> Result<f64> {
        let exposure: f64 = self
            .conn
            .query_row(
                "SELECT COALESCE(SUM(entry_price * size), 0.0) FROM positions WHERE status = 'open'",
                [],
                |row| row.get(0),
            )
            .context("Failed to get total exposure")?;
        Ok(exposure)
    }

    pub fn get_open_positions(&self) -> Result<Vec<PositionRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT market_condition_id, token_id, side, entry_price, size, status, current_price, unrealized_pnl, estimated_probability FROM positions WHERE status = 'open'",
        ).context("Failed to prepare open positions query")?;
        let rows = stmt
            .query_map([], |row| {
                Ok(PositionRow {
                    market_condition_id: row.get(0)?,
                    token_id: row.get(1)?,
                    side: row.get(2)?,
                    entry_price: row.get(3)?,
                    size: row.get(4)?,
                    status: row.get(5)?,
                    current_price: row.get(6)?,
                    unrealized_pnl: row.get::<_, Option<f64>>(7)?.unwrap_or(0.0),
                    estimated_probability: row.get(8)?,
                    question: None,
                })
            })
            .context("Failed to query open positions")?;
        let mut positions = Vec::new();
        for row in rows {
            positions.push(row.context("Failed to read position row")?);
        }
        Ok(positions)
    }

    /// Get open positions with market question text (JOIN with markets table).
    pub fn get_open_positions_with_market(&self) -> Result<Vec<PositionRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT p.market_condition_id, p.token_id, p.side, p.entry_price, p.size, p.status, \
             p.current_price, p.unrealized_pnl, p.estimated_probability, m.question \
             FROM positions p \
             LEFT JOIN markets m ON p.market_condition_id = m.condition_id \
             WHERE p.status = 'open'",
        ).context("Failed to prepare open positions with market query")?;
        let rows = stmt
            .query_map([], |row| {
                Ok(PositionRow {
                    market_condition_id: row.get(0)?,
                    token_id: row.get(1)?,
                    side: row.get(2)?,
                    entry_price: row.get(3)?,
                    size: row.get(4)?,
                    status: row.get(5)?,
                    current_price: row.get(6)?,
                    unrealized_pnl: row.get::<_, Option<f64>>(7)?.unwrap_or(0.0),
                    estimated_probability: row.get(8)?,
                    question: row.get(9)?,
                })
            })
            .context("Failed to query open positions with market")?;
        let mut positions = Vec::new();
        for row in rows {
            positions.push(row.context("Failed to read position row")?);
        }
        Ok(positions)
    }

    /// Update the current price for an open position and recompute unrealized P&L.
    pub fn update_position_price(
        &self,
        market_condition_id: &str,
        side: &str,
        current_price: f64,
    ) -> Result<()> {
        // For YES positions: pnl = (current - entry) * size
        // For NO positions: pnl = (entry - current) * size (we profit when price drops)
        // Actually in binary markets: pnl = (current_value - cost) where cost = entry_price * size
        // YES: value = current_price * size, cost = entry_price * size
        // NO: value = (1 - current_price) * size, cost = (1 - entry_price) is already entry_price for NO
        // Simpler: unrealized_pnl = (current_price - entry_price) * size for any side
        // since entry_price already accounts for side (buy_price)
        self.conn
            .execute(
                "UPDATE positions SET current_price = ?1, \
             unrealized_pnl = (?1 - entry_price) * size, \
             updated_at = datetime('now') \
             WHERE market_condition_id = ?2 AND side = ?3 AND status = 'open'",
                rusqlite::params![current_price, market_condition_id, side],
            )
            .context("Failed to update position price")?;
        Ok(())
    }

    /// Close an open position. Sets status='closed' and returns realized P&L.
    pub fn close_position(
        &self,
        market_condition_id: &str,
        side: &str,
        exit_price: f64,
    ) -> Result<f64> {
        // Get the position details first
        let (entry_price, size): (f64, f64) = self
            .conn
            .query_row(
                "SELECT entry_price, size FROM positions WHERE market_condition_id = ?1 AND side = ?2 AND status = 'open'",
                rusqlite::params![market_condition_id, side],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .context("Failed to find open position to close")?;

        let realized_pnl = (exit_price - entry_price) * size;

        self.conn
            .execute(
                "UPDATE positions SET status = 'closed', current_price = ?1, \
             realized_pnl = ?2, unrealized_pnl = 0.0, updated_at = datetime('now') \
             WHERE market_condition_id = ?3 AND side = ?4 AND status = 'open'",
                rusqlite::params![exit_price, realized_pnl, market_condition_id, side],
            )
            .context("Failed to close position")?;

        Ok(realized_pnl)
    }

    /// Get or update the peak bankroll. Returns the (possibly updated) peak.
    pub fn update_peak_bankroll(&self, current: f64) -> Result<f64> {
        let existing_peak: Option<f64> = self
            .conn
            .query_row(
                "SELECT peak_value FROM peak_bankroll ORDER BY id DESC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .ok();

        let peak = match existing_peak {
            Some(p) if current > p => {
                self.conn
                    .execute(
                        "INSERT INTO peak_bankroll (peak_value) VALUES (?1)",
                        [current],
                    )
                    .context("Failed to insert new peak")?;
                current
            }
            Some(p) => p,
            None => {
                self.conn
                    .execute(
                        "INSERT INTO peak_bankroll (peak_value) VALUES (?1)",
                        [current],
                    )
                    .context("Failed to insert initial peak")?;
                current
            }
        };

        Ok(peak)
    }

    /// Get the current peak bankroll without updating.
    pub fn get_peak_bankroll(&self) -> Result<f64> {
        let peak: f64 = self
            .conn
            .query_row(
                "SELECT COALESCE((SELECT peak_value FROM peak_bankroll ORDER BY id DESC LIMIT 1), 0.0)",
                [],
                |row| row.get(0),
            )
            .context("Failed to get peak bankroll")?;
        Ok(peak)
    }

    /// Log a position management alert.
    pub fn log_position_alert(
        &self,
        market_condition_id: &str,
        alert_type: &str,
        details: &str,
        action_taken: &str,
        cycle_number: i64,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO position_alerts (market_condition_id, alert_type, details, action_taken, cycle_number) VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![market_condition_id, alert_type, details, action_taken, cycle_number],
        ).context("Failed to log position alert")?;
        Ok(())
    }

    /// Upsert a position with optional estimated_probability.
    pub fn upsert_position_with_estimate(
        &self,
        market_condition_id: &str,
        token_id: &str,
        side: &str,
        entry_price: f64,
        size: f64,
        estimated_probability: Option<f64>,
    ) -> Result<()> {
        // Check if an open position already exists for this market/side
        let existing: Option<(i64, f64, f64)> = self
            .conn
            .query_row(
                "SELECT id, entry_price, size FROM positions WHERE market_condition_id = ?1 AND side = ?2 AND status = 'open'",
                rusqlite::params![market_condition_id, side],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .ok();

        if let Some((id, old_price, old_size)) = existing {
            let total_size = old_size + size;
            let avg_price = (old_price * old_size + entry_price * size) / total_size;
            self.conn
                .execute(
                    "UPDATE positions SET entry_price = ?1, size = ?2, token_id = ?3, \
                 estimated_probability = COALESCE(?4, estimated_probability), \
                 updated_at = datetime('now') WHERE id = ?5",
                    rusqlite::params![avg_price, total_size, token_id, estimated_probability, id],
                )
                .context("Failed to update position with estimate")?;
        } else {
            self.conn.execute(
                "INSERT INTO positions (market_condition_id, token_id, side, entry_price, size, status, estimated_probability) \
                 VALUES (?1, ?2, ?3, ?4, ?5, 'open', ?6)",
                rusqlite::params![market_condition_id, token_id, side, entry_price, size, estimated_probability],
            ).context("Failed to insert position with estimate")?;
        }
        Ok(())
    }

    /// Insert a weather snapshot for the current cycle.
    #[allow(clippy::too_many_arguments)]
    pub fn insert_weather_snapshot(
        &self,
        cycle_number: i64,
        city: &str,
        forecast_date: &str,
        ensemble_mean: f64,
        ensemble_std: f64,
        gefs_count: i32,
        ecmwf_count: i32,
        bucket_data: &str,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO weather_snapshots (cycle_number, city, forecast_date, ensemble_mean, ensemble_std, gefs_count, ecmwf_count, bucket_data) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params![cycle_number, city, forecast_date, ensemble_mean, ensemble_std, gefs_count, ecmwf_count, bucket_data],
        ).context("Failed to insert weather snapshot")?;
        Ok(())
    }

    /// Get weather snapshots from the latest cycle.
    pub fn get_latest_weather_snapshots(&self) -> Result<Vec<WeatherSnapshotRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT cycle_number, city, forecast_date, ensemble_mean, ensemble_std, gefs_count, ecmwf_count, bucket_data, created_at \
             FROM weather_snapshots \
             WHERE cycle_number = (SELECT MAX(cycle_number) FROM weather_snapshots) \
             ORDER BY city",
        ).context("Failed to prepare weather snapshots query")?;
        let rows = stmt
            .query_map([], |row| {
                Ok(WeatherSnapshotRow {
                    cycle_number: row.get(0)?,
                    city: row.get(1)?,
                    forecast_date: row.get(2)?,
                    ensemble_mean: row.get(3)?,
                    ensemble_std: row.get(4)?,
                    gefs_count: row.get(5)?,
                    ecmwf_count: row.get(6)?,
                    bucket_data: row.get(7)?,
                    created_at: row.get(8)?,
                })
            })
            .context("Failed to query weather snapshots")?;
        let mut snapshots = Vec::new();
        for row in rows {
            snapshots.push(row.context("Failed to read weather snapshot row")?);
        }
        Ok(snapshots)
    }

    /// Insert or replace a weather actual observation.
    #[allow(clippy::too_many_arguments)]
    pub fn insert_weather_actual(
        &self,
        city: &str,
        forecast_date: &str,
        wu_actual_high: Option<f64>,
        nws_forecast_high: Option<f64>,
        ensemble_mean: Option<f64>,
        predicted_bucket: Option<&str>,
        actual_bucket: Option<&str>,
        prediction_error: Option<f64>,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO weather_actuals (city, forecast_date, wu_actual_high, nws_forecast_high, ensemble_mean, predicted_bucket, actual_bucket, prediction_error)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(city, forecast_date) DO UPDATE SET
                wu_actual_high = COALESCE(excluded.wu_actual_high, wu_actual_high),
                nws_forecast_high = COALESCE(excluded.nws_forecast_high, nws_forecast_high),
                ensemble_mean = COALESCE(excluded.ensemble_mean, ensemble_mean),
                predicted_bucket = COALESCE(excluded.predicted_bucket, predicted_bucket),
                actual_bucket = COALESCE(excluded.actual_bucket, actual_bucket),
                prediction_error = COALESCE(excluded.prediction_error, prediction_error)",
            rusqlite::params![city, forecast_date, wu_actual_high, nws_forecast_high, ensemble_mean, predicted_bucket, actual_bucket, prediction_error],
        ).context("Failed to insert weather actual")?;
        Ok(())
    }

    /// Get weather actuals for a city, ordered by date descending.
    pub fn get_weather_actuals(&self, city: &str, limit: i64) -> Result<Vec<WeatherActualRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT city, forecast_date, wu_actual_high, nws_forecast_high, ensemble_mean, predicted_bucket, actual_bucket, prediction_error, created_at \
             FROM weather_actuals \
             WHERE city = ?1 \
             ORDER BY forecast_date DESC \
             LIMIT ?2",
        ).context("Failed to prepare weather actuals query")?;
        let rows = stmt
            .query_map(rusqlite::params![city, limit], |row| {
                Ok(WeatherActualRow {
                    city: row.get(0)?,
                    forecast_date: row.get(1)?,
                    wu_actual_high: row.get(2)?,
                    nws_forecast_high: row.get(3)?,
                    ensemble_mean: row.get(4)?,
                    predicted_bucket: row.get(5)?,
                    actual_bucket: row.get(6)?,
                    prediction_error: row.get(7)?,
                    created_at: row.get(8)?,
                })
            })
            .context("Failed to query weather actuals")?;
        let mut actuals = Vec::new();
        for row in rows {
            actuals.push(row.context("Failed to read weather actual row")?);
        }
        Ok(actuals)
    }

    /// Check if there's already an open position for a given market condition_id (any side).
    pub fn has_open_position(&self, market_condition_id: &str) -> bool {
        let count: i64 = self
            .conn
            .query_row(
                "SELECT COUNT(*) FROM positions WHERE market_condition_id = ?1 AND status = 'open'",
                rusqlite::params![market_condition_id],
                |row| row.get(0),
            )
            .unwrap_or(0);
        count > 0
    }

    /// Update the estimated_probability for an open position.
    pub fn update_position_estimate(
        &self,
        market_condition_id: &str,
        estimated_probability: f64,
    ) -> Result<()> {
        self.conn
            .execute(
                "UPDATE positions SET estimated_probability = ?1, updated_at = datetime('now') \
                 WHERE market_condition_id = ?2 AND status = 'open'",
                rusqlite::params![estimated_probability, market_condition_id],
            )
            .context("Failed to update position estimate")?;
        Ok(())
    }

    /// Get total trading fees from bankroll_log.
    pub fn get_total_trading_fees(&self) -> f64 {
        let fees: f64 = self
            .conn
            .query_row(
                "SELECT COALESCE(SUM(ABS(amount)), 0.0) FROM bankroll_log WHERE entry_type = 'trading_fee'",
                [],
                |row| row.get(0),
            )
            .unwrap_or(0.0);
        fees
    }

    /// Get total weather losses for today (approximate via bankroll_log description matching).
    pub fn get_weather_losses_today(&self) -> f64 {
        let loss: f64 = self
            .conn
            .query_row(
                "SELECT COALESCE(SUM(ABS(amount)), 0.0) FROM bankroll_log \
                 WHERE entry_type = 'trade' AND amount < 0 \
                 AND (description LIKE '%temperature%' OR description LIKE '%weather%') \
                 AND DATE(created_at) = DATE('now')",
                [],
                |row| row.get(0),
            )
            .unwrap_or(0.0);
        loss
    }

    /// Insert an opportunity from edge detection.
    #[allow(clippy::too_many_arguments)]
    pub fn insert_opportunity(
        &self,
        cycle_number: i64,
        condition_id: &str,
        question: &str,
        side: &str,
        market_price: f64,
        estimated_probability: f64,
        edge: f64,
        confidence: f64,
        status: &str,
        reject_reason: Option<&str>,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO cycle_opportunities (cycle_number, condition_id, question, side, market_price, estimated_probability, edge, confidence, status, reject_reason) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            rusqlite::params![cycle_number, condition_id, question, side, market_price, estimated_probability, edge, confidence, status, reject_reason],
        ).context("Failed to insert opportunity")?;
        Ok(())
    }

    /// Get recent opportunities, sorted by cycle desc then edge desc.
    pub fn get_recent_opportunities(&self, limit: i64) -> Result<Vec<OpportunityRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT cycle_number, condition_id, question, side, market_price, estimated_probability, edge, confidence, status, reject_reason, created_at \
             FROM cycle_opportunities \
             ORDER BY cycle_number DESC, edge DESC \
             LIMIT ?1",
        ).context("Failed to prepare opportunities query")?;
        let rows = stmt
            .query_map([limit], |row| {
                Ok(OpportunityRow {
                    cycle_number: row.get(0)?,
                    condition_id: row.get(1)?,
                    question: row.get(2)?,
                    side: row.get(3)?,
                    market_price: row.get(4)?,
                    estimated_probability: row.get(5)?,
                    edge: row.get(6)?,
                    confidence: row.get(7)?,
                    status: row.get(8)?,
                    reject_reason: row.get(9)?,
                    created_at: row.get(10)?,
                })
            })
            .context("Failed to query opportunities")?;
        let mut opps = Vec::new();
        for row in rows {
            opps.push(row.context("Failed to read opportunity row")?);
        }
        Ok(opps)
    }

    pub fn get_next_cycle_number(&self) -> Result<i64> {
        let n: i64 = self
            .conn
            .query_row(
                "SELECT COALESCE(MAX(cycle_number), 0) + 1 FROM cycle_log",
                [],
                |row| row.get(0),
            )
            .context("Failed to get next cycle number")?;
        Ok(n)
    }

    pub fn get_total_trades_count(&self) -> Result<i64> {
        let count: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM trades", [], |row| row.get(0))
            .context("Failed to get total trades count")?;
        Ok(count)
    }

    pub fn get_recent_trades(&self, limit: i64) -> Result<Vec<TradeRow>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT t.trade_id, t.market_condition_id, t.side, t.price, t.size, t.status, t.paper, t.created_at, \
                 m.question, \
                 p.realized_pnl, p.unrealized_pnl, p.status, \
                 t.entry_fee \
                 FROM trades t \
                 LEFT JOIN markets m ON t.market_condition_id = m.condition_id \
                 LEFT JOIN positions p ON p.id = ( \
                   SELECT id FROM positions p2 \
                   WHERE p2.market_condition_id = t.market_condition_id AND p2.side = t.side \
                   ORDER BY p2.id DESC LIMIT 1 \
                 ) \
                 ORDER BY t.id DESC LIMIT ?1",
            )
            .context("Failed to prepare recent trades query")?;
        let rows = stmt
            .query_map([limit], |row| {
                Ok(TradeRow {
                    trade_id: row.get(0)?,
                    market_condition_id: row.get(1)?,
                    side: row.get(2)?,
                    price: row.get(3)?,
                    size: row.get(4)?,
                    status: row.get(5)?,
                    paper: row.get(6)?,
                    created_at: row.get(7)?,
                    question: row.get(8)?,
                    realized_pnl: row.get(9)?,
                    unrealized_pnl: row.get(10)?,
                    position_status: row.get(11)?,
                    entry_fee: row.get::<_, Option<f64>>(12)?.unwrap_or(0.0),
                })
            })
            .context("Failed to query recent trades")?;
        let mut trades = Vec::new();
        for row in rows {
            trades.push(row.context("Failed to read trade row")?);
        }
        Ok(trades)
    }

    pub fn ensure_bankroll_seeded(&self, initial: f64) -> Result<()> {
        let count: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM bankroll_log", [], |row| row.get(0))
            .context("Failed to count bankroll entries")?;
        if count == 0 {
            self.log_bankroll_entry("seed", initial, initial, "Initial seed funding")?;
        }
        Ok(())
    }

    fn run_migrations(&self) -> Result<()> {
        self.conn
            .execute_batch(
                "
            CREATE TABLE IF NOT EXISTS markets (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                condition_id TEXT NOT NULL UNIQUE,
                question TEXT NOT NULL,
                slug TEXT,
                category TEXT,
                yes_token_id TEXT,
                no_token_id TEXT,
                volume REAL,
                liquidity REAL,
                end_date TEXT,
                active BOOLEAN NOT NULL DEFAULT 1,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                updated_at TEXT NOT NULL DEFAULT (datetime('now'))
            );

            CREATE TABLE IF NOT EXISTS trades (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                trade_id TEXT NOT NULL UNIQUE,
                market_condition_id TEXT NOT NULL,
                token_id TEXT NOT NULL,
                side TEXT NOT NULL,
                price REAL NOT NULL,
                size REAL NOT NULL,
                status TEXT NOT NULL DEFAULT 'pending',
                paper BOOLEAN NOT NULL DEFAULT 1,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                FOREIGN KEY (market_condition_id) REFERENCES markets(condition_id)
            );

            CREATE TABLE IF NOT EXISTS positions (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                market_condition_id TEXT NOT NULL,
                token_id TEXT NOT NULL,
                side TEXT NOT NULL,
                entry_price REAL NOT NULL,
                current_price REAL,
                size REAL NOT NULL,
                unrealized_pnl REAL DEFAULT 0.0,
                realized_pnl REAL DEFAULT 0.0,
                status TEXT NOT NULL DEFAULT 'open',
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                updated_at TEXT NOT NULL DEFAULT (datetime('now')),
                FOREIGN KEY (market_condition_id) REFERENCES markets(condition_id)
            );

            CREATE TABLE IF NOT EXISTS bankroll_log (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                entry_type TEXT NOT NULL,
                amount REAL NOT NULL,
                balance_after REAL NOT NULL,
                description TEXT,
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            );

            CREATE TABLE IF NOT EXISTS cycle_log (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                cycle_number INTEGER NOT NULL,
                markets_scanned INTEGER NOT NULL DEFAULT 0,
                markets_filtered INTEGER NOT NULL DEFAULT 0,
                trades_placed INTEGER NOT NULL DEFAULT 0,
                api_cost_usd REAL NOT NULL DEFAULT 0.0,
                bankroll_before REAL,
                bankroll_after REAL,
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            );

            CREATE TABLE IF NOT EXISTS api_cost_log (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                cycle_number INTEGER,
                market_condition_id TEXT,
                model TEXT NOT NULL,
                input_tokens INTEGER NOT NULL,
                output_tokens INTEGER NOT NULL,
                cost_usd REAL NOT NULL,
                call_type TEXT NOT NULL,
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            );

            CREATE TABLE IF NOT EXISTS peak_bankroll (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                peak_value REAL NOT NULL,
                recorded_at TEXT NOT NULL DEFAULT (datetime('now'))
            );

            CREATE TABLE IF NOT EXISTS position_alerts (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                market_condition_id TEXT NOT NULL,
                alert_type TEXT NOT NULL,
                details TEXT,
                action_taken TEXT,
                cycle_number INTEGER,
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            );

            CREATE TABLE IF NOT EXISTS weather_snapshots (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                cycle_number INTEGER NOT NULL,
                city TEXT NOT NULL,
                forecast_date TEXT NOT NULL,
                ensemble_mean REAL NOT NULL,
                ensemble_std REAL NOT NULL,
                gefs_count INTEGER NOT NULL,
                ecmwf_count INTEGER NOT NULL,
                bucket_data TEXT NOT NULL,
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            );

            CREATE TABLE IF NOT EXISTS cycle_opportunities (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                cycle_number INTEGER NOT NULL,
                condition_id TEXT NOT NULL,
                question TEXT NOT NULL,
                side TEXT NOT NULL,
                market_price REAL NOT NULL,
                estimated_probability REAL NOT NULL,
                edge REAL NOT NULL,
                confidence REAL NOT NULL,
                status TEXT NOT NULL,
                reject_reason TEXT,
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            );

            CREATE TABLE IF NOT EXISTS weather_actuals (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                city TEXT NOT NULL,
                forecast_date TEXT NOT NULL,
                wu_actual_high REAL,
                nws_forecast_high REAL,
                ensemble_mean REAL,
                predicted_bucket TEXT,
                actual_bucket TEXT,
                prediction_error REAL,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                UNIQUE(city, forecast_date)
            );

            CREATE TABLE IF NOT EXISTS weather_calibration (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                city TEXT NOT NULL UNIQUE,
                bias_offset REAL NOT NULL DEFAULT 0.0,
                spread_factor REAL NOT NULL DEFAULT 1.0,
                sample_size INTEGER NOT NULL DEFAULT 0,
                updated_at TEXT NOT NULL DEFAULT (datetime('now'))
            );
            ",
            )
            .context("Failed to run database migrations")?;

        // Add entry_fee column to trades (idempotent)
        let _ = self.conn.execute(
            "ALTER TABLE trades ADD COLUMN entry_fee REAL DEFAULT 0.0",
            [],
        );

        // Phase 6: Add estimated_probability column to positions (idempotent)
        let _ = self.conn.execute(
            "ALTER TABLE positions ADD COLUMN estimated_probability REAL",
            [],
        );

        // Phase 5+: Add extra ensemble columns to weather_snapshots (idempotent)
        let _ = self.conn.execute(
            "ALTER TABLE weather_snapshots ADD COLUMN icon_count INTEGER DEFAULT 0",
            [],
        );
        let _ = self.conn.execute(
            "ALTER TABLE weather_snapshots ADD COLUMN gem_count INTEGER DEFAULT 0",
            [],
        );
        let _ = self.conn.execute(
            "ALTER TABLE weather_snapshots ADD COLUMN total_members INTEGER DEFAULT 0",
            [],
        );

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_open_in_memory() {
        let db = Database::open_in_memory().unwrap();
        // Verify all 6 tables exist
        let tables: Vec<String> = db
            .conn
            .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();

        assert!(tables.contains(&"markets".to_string()));
        assert!(tables.contains(&"trades".to_string()));
        assert!(tables.contains(&"positions".to_string()));
        assert!(tables.contains(&"bankroll_log".to_string()));
        assert!(tables.contains(&"cycle_log".to_string()));
        assert!(tables.contains(&"api_cost_log".to_string()));
    }

    #[test]
    fn test_insert_and_read_market() {
        let db = Database::open_in_memory().unwrap();

        db.conn
            .execute(
                "INSERT INTO markets (condition_id, question, slug, category, volume, liquidity, active)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                rusqlite::params![
                    "0xabc123",
                    "Will it rain?",
                    "will-it-rain",
                    "weather",
                    50000.0,
                    10000.0,
                    true
                ],
            )
            .unwrap();

        let question: String = db
            .conn
            .query_row(
                "SELECT question FROM markets WHERE condition_id = ?1",
                ["0xabc123"],
                |row| row.get(0),
            )
            .unwrap();

        assert_eq!(question, "Will it rain?");
    }

    #[test]
    fn test_insert_bankroll_entry() {
        let db = Database::open_in_memory().unwrap();

        db.conn
            .execute(
                "INSERT INTO bankroll_log (entry_type, amount, balance_after, description)
             VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params!["seed", 50.0, 50.0, "Initial seed funding"],
            )
            .unwrap();

        let balance: f64 = db
            .conn
            .query_row(
                "SELECT balance_after FROM bankroll_log ORDER BY id DESC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .unwrap();

        assert_eq!(balance, 50.0);
    }

    #[test]
    fn test_migrations_idempotent() {
        let db = Database::open_in_memory().unwrap();
        // Running migrations again should not fail
        db.run_migrations().unwrap();
    }

    #[test]
    fn test_log_and_get_api_cost() {
        let db = Database::open_in_memory().unwrap();
        db.log_api_cost(
            1,
            Some("0xabc"),
            "claude-haiku-4-5-20251001",
            500,
            50,
            0.00075,
            "triage",
        )
        .unwrap();
        db.log_api_cost(
            1,
            Some("0xabc"),
            "claude-sonnet-4-5-20250929",
            2000,
            200,
            0.009,
            "analysis",
        )
        .unwrap();
        let cost = db.get_cycle_api_cost(1).unwrap();
        assert!((cost - 0.00975).abs() < 0.0001);
    }

    fn insert_test_market(db: &Database, condition_id: &str) {
        db.conn
            .execute(
                "INSERT INTO markets (condition_id, question, active) VALUES (?1, ?2, 1)",
                rusqlite::params![condition_id, format!("Test market {}", condition_id)],
            )
            .unwrap();
    }

    #[test]
    fn test_insert_trade() {
        let db = Database::open_in_memory().unwrap();
        insert_test_market(&db, "0xcond1");
        db.insert_trade(
            "trade_1",
            "0xcond1",
            "tok_yes_1",
            "YES",
            0.65,
            10.0,
            "filled",
            true,
            0.0,
        )
        .unwrap();

        let (side, price, size): (String, f64, f64) = db
            .conn
            .query_row(
                "SELECT side, price, size FROM trades WHERE trade_id = ?1",
                ["trade_1"],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(side, "YES");
        assert!((price - 0.65).abs() < f64::EPSILON);
        assert!((size - 10.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_upsert_position_new() {
        let db = Database::open_in_memory().unwrap();
        insert_test_market(&db, "0xcond1");
        db.upsert_position("0xcond1", "tok_yes_1", "YES", 0.60, 5.0)
            .unwrap();

        let positions = db.get_open_positions().unwrap();
        assert_eq!(positions.len(), 1);
        assert_eq!(positions[0].market_condition_id, "0xcond1");
        assert!((positions[0].entry_price - 0.60).abs() < f64::EPSILON);
        assert!((positions[0].size - 5.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_upsert_position_aggregate() {
        let db = Database::open_in_memory().unwrap();
        insert_test_market(&db, "0xcond1");
        db.upsert_position("0xcond1", "tok_yes_1", "YES", 0.60, 10.0)
            .unwrap();
        db.upsert_position("0xcond1", "tok_yes_1", "YES", 0.70, 10.0)
            .unwrap();

        let positions = db.get_open_positions().unwrap();
        assert_eq!(positions.len(), 1);
        // avg price = (0.60*10 + 0.70*10) / 20 = 0.65
        assert!((positions[0].entry_price - 0.65).abs() < 1e-10);
        assert!((positions[0].size - 20.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_bankroll_entry_and_retrieval() {
        let db = Database::open_in_memory().unwrap();
        db.log_bankroll_entry("seed", 50.0, 50.0, "Initial seed")
            .unwrap();
        assert!((db.get_current_bankroll().unwrap() - 50.0).abs() < f64::EPSILON);

        db.log_bankroll_entry("trade", -3.25, 46.75, "Bought YES")
            .unwrap();
        assert!((db.get_current_bankroll().unwrap() - 46.75).abs() < f64::EPSILON);
    }

    #[test]
    fn test_get_current_bankroll_empty() {
        let db = Database::open_in_memory().unwrap();
        assert!((db.get_current_bankroll().unwrap() - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_get_total_exposure() {
        let db = Database::open_in_memory().unwrap();
        insert_test_market(&db, "0xcond1");
        insert_test_market(&db, "0xcond2");
        db.upsert_position("0xcond1", "tok1", "YES", 0.60, 10.0)
            .unwrap();
        db.upsert_position("0xcond2", "tok2", "NO", 0.40, 5.0)
            .unwrap();
        // exposure = 0.60*10 + 0.40*5 = 6.0 + 2.0 = 8.0
        let exposure = db.get_total_exposure().unwrap();
        assert!((exposure - 8.0).abs() < 1e-10);
    }

    #[test]
    fn test_get_total_exposure_empty() {
        let db = Database::open_in_memory().unwrap();
        assert!((db.get_total_exposure().unwrap() - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_ensure_bankroll_seeded_once() {
        let db = Database::open_in_memory().unwrap();
        db.ensure_bankroll_seeded(50.0).unwrap();
        assert!((db.get_current_bankroll().unwrap() - 50.0).abs() < f64::EPSILON);

        // Second call should be idempotent
        db.ensure_bankroll_seeded(50.0).unwrap();
        let count: i64 = db
            .conn
            .query_row("SELECT COUNT(*) FROM bankroll_log", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn test_get_open_positions_excludes_closed() {
        let db = Database::open_in_memory().unwrap();
        insert_test_market(&db, "0xcond1");
        db.upsert_position("0xcond1", "tok1", "YES", 0.60, 10.0)
            .unwrap();
        // Manually close the position
        db.conn
            .execute(
                "UPDATE positions SET status = 'closed' WHERE market_condition_id = '0xcond1'",
                [],
            )
            .unwrap();
        let positions = db.get_open_positions().unwrap();
        assert!(positions.is_empty());
    }

    #[test]
    fn test_get_next_cycle_number_empty() {
        let db = Database::open_in_memory().unwrap();
        assert_eq!(db.get_next_cycle_number().unwrap(), 1);
    }

    #[test]
    fn test_get_next_cycle_number_populated() {
        let db = Database::open_in_memory().unwrap();
        db.conn
            .execute(
                "INSERT INTO cycle_log (cycle_number, markets_scanned, trades_placed, api_cost_usd) VALUES (1, 10, 0, 0.01)",
                [],
            )
            .unwrap();
        db.conn
            .execute(
                "INSERT INTO cycle_log (cycle_number, markets_scanned, trades_placed, api_cost_usd) VALUES (2, 15, 1, 0.02)",
                [],
            )
            .unwrap();
        assert_eq!(db.get_next_cycle_number().unwrap(), 3);
    }

    #[test]
    fn test_get_recent_trades() {
        let db = Database::open_in_memory().unwrap();
        insert_test_market(&db, "0xcond1");
        db.insert_trade("t1", "0xcond1", "tok1", "YES", 0.60, 5.0, "filled", true, 0.0)
            .unwrap();
        db.insert_trade("t2", "0xcond1", "tok1", "NO", 0.40, 3.0, "filled", true, 0.0)
            .unwrap();
        db.insert_trade("t3", "0xcond1", "tok1", "YES", 0.70, 2.0, "filled", true, 0.0)
            .unwrap();

        let trades = db.get_recent_trades(2).unwrap();
        assert_eq!(trades.len(), 2);
        // Most recent first
        assert_eq!(trades[0].trade_id, "t3");
        assert_eq!(trades[1].trade_id, "t2");
    }

    #[test]
    fn test_get_total_trades_count() {
        let db = Database::open_in_memory().unwrap();
        assert_eq!(db.get_total_trades_count().unwrap(), 0);
        insert_test_market(&db, "0xcond1");
        db.insert_trade("t1", "0xcond1", "tok1", "YES", 0.60, 5.0, "filled", true, 0.0)
            .unwrap();
        assert_eq!(db.get_total_trades_count().unwrap(), 1);
    }

    #[test]
    fn test_peak_bankroll_tracking() {
        let db = Database::open_in_memory().unwrap();
        // First call sets initial peak
        let peak = db.update_peak_bankroll(50.0).unwrap();
        assert!((peak - 50.0).abs() < f64::EPSILON);

        // Higher value updates peak
        let peak = db.update_peak_bankroll(75.0).unwrap();
        assert!((peak - 75.0).abs() < f64::EPSILON);

        // Lower value doesn't update peak
        let peak = db.update_peak_bankroll(60.0).unwrap();
        assert!((peak - 75.0).abs() < f64::EPSILON);

        // Get peak without updating
        let peak = db.get_peak_bankroll().unwrap();
        assert!((peak - 75.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_peak_bankroll_empty() {
        let db = Database::open_in_memory().unwrap();
        let peak = db.get_peak_bankroll().unwrap();
        assert!((peak - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_update_position_price() {
        let db = Database::open_in_memory().unwrap();
        insert_test_market(&db, "0xcond1");
        db.upsert_position("0xcond1", "tok1", "YES", 0.60, 10.0)
            .unwrap();

        db.update_position_price("0xcond1", "YES", 0.75).unwrap();

        let positions = db.get_open_positions().unwrap();
        assert_eq!(positions.len(), 1);
        assert!((positions[0].current_price.unwrap() - 0.75).abs() < f64::EPSILON);
        // pnl = (0.75 - 0.60) * 10 = 1.50
        assert!((positions[0].unrealized_pnl - 1.50).abs() < 1e-10);
    }

    #[test]
    fn test_close_position() {
        let db = Database::open_in_memory().unwrap();
        insert_test_market(&db, "0xcond1");
        db.upsert_position("0xcond1", "tok1", "YES", 0.60, 10.0)
            .unwrap();

        let pnl = db.close_position("0xcond1", "YES", 0.80).unwrap();
        // pnl = (0.80 - 0.60) * 10 = 2.0
        assert!((pnl - 2.0).abs() < 1e-10);

        // Position should now be closed
        let positions = db.get_open_positions().unwrap();
        assert!(positions.is_empty());
    }

    #[test]
    fn test_get_open_positions_with_market() {
        let db = Database::open_in_memory().unwrap();
        insert_test_market(&db, "0xcond1");
        db.upsert_position("0xcond1", "tok1", "YES", 0.60, 10.0)
            .unwrap();

        let positions = db.get_open_positions_with_market().unwrap();
        assert_eq!(positions.len(), 1);
        assert!(positions[0].question.is_some());
        assert!(positions[0].question.as_ref().unwrap().contains("0xcond1"));
    }

    #[test]
    fn test_log_position_alert() {
        let db = Database::open_in_memory().unwrap();
        db.log_position_alert("0xcond1", "stop_loss", "Price dropped 20%", "exit", 5)
            .unwrap();

        let count: i64 = db
            .conn
            .query_row(
                "SELECT COUNT(*) FROM position_alerts WHERE alert_type = 'stop_loss'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn test_upsert_position_with_estimate() {
        let db = Database::open_in_memory().unwrap();
        insert_test_market(&db, "0xcond1");
        db.upsert_position_with_estimate("0xcond1", "tok1", "YES", 0.60, 10.0, Some(0.75))
            .unwrap();

        let positions = db.get_open_positions().unwrap();
        assert_eq!(positions.len(), 1);
        assert!((positions[0].estimated_probability.unwrap() - 0.75).abs() < f64::EPSILON);
    }

    #[test]
    fn test_new_tables_exist() {
        let db = Database::open_in_memory().unwrap();
        let tables: Vec<String> = db
            .conn
            .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();

        assert!(tables.contains(&"peak_bankroll".to_string()));
        assert!(tables.contains(&"position_alerts".to_string()));
    }

    #[test]
    fn test_get_cycle_api_cost_empty() {
        let db = Database::open_in_memory().unwrap();
        let cost = db.get_cycle_api_cost(99).unwrap();
        assert_eq!(cost, 0.0);
    }

    #[test]
    fn test_get_api_cost_since() {
        let db = Database::open_in_memory().unwrap();
        db.log_api_cost(
            1,
            None,
            "claude-haiku-4-5-20251001",
            500,
            50,
            0.001,
            "triage",
        )
        .unwrap();
        let cost = db.get_api_cost_since(1).unwrap();
        assert!(cost > 0.0);
    }

    #[test]
    fn test_has_open_position() {
        let db = Database::open_in_memory().unwrap();
        insert_test_market(&db, "0xcond1");
        assert!(!db.has_open_position("0xcond1"));

        db.upsert_position("0xcond1", "tok1", "YES", 0.60, 10.0)
            .unwrap();
        assert!(db.has_open_position("0xcond1"));

        // Close and check again
        db.close_position("0xcond1", "YES", 0.70).unwrap();
        assert!(!db.has_open_position("0xcond1"));
    }

    #[test]
    fn test_update_position_estimate() {
        let db = Database::open_in_memory().unwrap();
        insert_test_market(&db, "0xcond1");
        db.upsert_position_with_estimate("0xcond1", "tok1", "YES", 0.60, 10.0, Some(0.75))
            .unwrap();

        db.update_position_estimate("0xcond1", 0.82).unwrap();

        let positions = db.get_open_positions().unwrap();
        assert_eq!(positions.len(), 1);
        assert!((positions[0].estimated_probability.unwrap() - 0.82).abs() < f64::EPSILON);
    }

    #[test]
    fn test_get_weather_losses_today() {
        let db = Database::open_in_memory().unwrap();
        // No losses initially
        assert!((db.get_weather_losses_today() - 0.0).abs() < f64::EPSILON);

        // Add a weather-related loss
        db.log_bankroll_entry(
            "trade",
            -2.50,
            47.50,
            "Paper loss: NYC high temperature 34-35°F",
        )
        .unwrap();

        let losses = db.get_weather_losses_today();
        assert!((losses - 2.50).abs() < f64::EPSILON);

        // Non-weather loss should not count
        db.log_bankroll_entry("trade", -1.00, 46.50, "Paper loss: Bitcoin market")
            .unwrap();
        let losses = db.get_weather_losses_today();
        assert!((losses - 2.50).abs() < f64::EPSILON);
    }
}
