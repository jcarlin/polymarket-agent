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
}

#[derive(Debug, Clone)]
pub struct PositionRow {
    pub market_condition_id: String,
    pub token_id: String,
    pub side: String,
    pub entry_price: f64,
    pub size: f64,
    pub status: String,
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
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO trades (trade_id, market_condition_id, token_id, side, price, size, status, paper) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params![trade_id, market_condition_id, token_id, side, price, size, status, paper],
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
            "SELECT market_condition_id, token_id, side, entry_price, size, status FROM positions WHERE status = 'open'",
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
                })
            })
            .context("Failed to query open positions")?;
        let mut positions = Vec::new();
        for row in rows {
            positions.push(row.context("Failed to read position row")?);
        }
        Ok(positions)
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
                "SELECT trade_id, market_condition_id, side, price, size, status, paper, created_at FROM trades ORDER BY id DESC LIMIT ?1",
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
            ",
            )
            .context("Failed to run database migrations")?;

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
        db.insert_trade("trade_1", "0xcond1", "tok_yes_1", "YES", 0.65, 10.0, "filled", true)
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
        db.insert_trade("t1", "0xcond1", "tok1", "YES", 0.60, 5.0, "filled", true)
            .unwrap();
        db.insert_trade("t2", "0xcond1", "tok1", "NO", 0.40, 3.0, "filled", true)
            .unwrap();
        db.insert_trade("t3", "0xcond1", "tok1", "YES", 0.70, 2.0, "filled", true)
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
        db.insert_trade("t1", "0xcond1", "tok1", "YES", 0.60, 5.0, "filled", true)
            .unwrap();
        assert_eq!(db.get_total_trades_count().unwrap(), 1);
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
}
