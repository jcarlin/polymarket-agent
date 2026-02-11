use anyhow::{Context, Result};
use rusqlite::Connection;
use std::path::Path;

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
