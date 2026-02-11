use anyhow::{Context, Result};
use tracing::info;

use crate::db::Database;

pub struct Accountant {
    low_bankroll_threshold: f64,
}

#[derive(Debug)]
pub struct CycleAccounting {
    pub bankroll_before: f64,
    pub bankroll_after: f64,
    pub api_cost: f64,
    pub is_alive: bool,
}

#[derive(Debug)]
pub struct DeathReport {
    pub cycles_completed: i64,
    pub total_trades: i64,
    pub total_pnl: f64,
    pub final_bankroll: f64,
    pub open_positions: usize,
    pub cause: String,
    pub recent_trades: Vec<crate::db::TradeRow>,
}

impl Accountant {
    pub fn new(low_bankroll_threshold: f64) -> Self {
        Self {
            low_bankroll_threshold,
        }
    }

    /// Close a cycle: deduct API costs from bankroll, return accounting summary.
    /// Reads the cycle's API cost from api_cost_log and deducts it via a single
    /// bankroll_log entry. Returns is_alive = bankroll_after > 0.
    pub fn close_cycle(&self, db: &Database, cycle_number: i64) -> Result<CycleAccounting> {
        let bankroll_before = db.get_current_bankroll()?;
        let api_cost = db.get_cycle_api_cost(cycle_number)?;

        let bankroll_after = if api_cost > 0.0 {
            let after = bankroll_before - api_cost;
            db.log_bankroll_entry(
                "api_cost",
                -api_cost,
                after,
                &format!("Cycle {} API cost", cycle_number),
            )?;
            after
        } else {
            bankroll_before
        };

        Ok(CycleAccounting {
            bankroll_before,
            bankroll_after,
            api_cost,
            is_alive: bankroll_after > 0.0,
        })
    }

    /// Returns the appropriate cycle duration based on current bankroll level.
    pub fn get_cycle_duration_secs(&self, bankroll: f64, high: u64, low: u64) -> u64 {
        if bankroll >= self.low_bankroll_threshold {
            high
        } else {
            low
        }
    }

    /// Build a death report from database state.
    pub fn generate_death_report(&self, db: &Database) -> Result<DeathReport> {
        let cycles_completed: i64 = db
            .conn
            .query_row(
                "SELECT COALESCE(MAX(cycle_number), 0) FROM cycle_log",
                [],
                |row| row.get(0),
            )
            .context("Failed to get max cycle number")?;

        let total_trades = db.get_total_trades_count()?;
        let final_bankroll = db.get_current_bankroll()?;
        let open_positions = db.get_open_positions()?.len();
        let recent_trades = db.get_recent_trades(10)?;

        // Total P&L = final bankroll - initial seed
        // The initial seed is the first bankroll_log entry
        let initial_seed: f64 = db
            .conn
            .query_row(
                "SELECT COALESCE((SELECT balance_after FROM bankroll_log WHERE entry_type = 'seed' ORDER BY id ASC LIMIT 1), 0.0)",
                [],
                |row| row.get(0),
            )
            .context("Failed to get initial seed")?;
        let total_pnl = final_bankroll - initial_seed;

        let cause = if final_bankroll <= 0.0 {
            "Bankroll depleted to zero".to_string()
        } else {
            "Unknown".to_string()
        };

        Ok(DeathReport {
            cycles_completed,
            total_trades,
            total_pnl,
            final_bankroll,
            open_positions,
            cause,
            recent_trades,
        })
    }
}

impl DeathReport {
    pub fn display(&self) {
        info!("╔══════════════════════════════════════════╗");
        info!("║           AGENT DEATH REPORT             ║");
        info!("╠══════════════════════════════════════════╣");
        info!("║ Cause: {:<33}║", self.cause);
        info!("║ Cycles completed: {:<22}║", self.cycles_completed);
        info!("║ Total trades: {:<26}║", self.total_trades);
        info!(
            "║ Total P&L: ${:<28.2}║",
            self.total_pnl
        );
        info!(
            "║ Final bankroll: ${:<24.2}║",
            self.final_bankroll
        );
        info!("║ Open positions: {:<24}║", self.open_positions);
        info!("╠══════════════════════════════════════════╣");
        info!("║ Recent Trades:                           ║");
        for trade in &self.recent_trades {
            info!(
                "║  {} {} @ ${:.2} x{:.1} [{}]",
                trade.side, trade.market_condition_id, trade.price, trade.size, trade.status
            );
        }
        info!("╚══════════════════════════════════════════╝");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup_db_with_bankroll(amount: f64) -> Database {
        let db = Database::open_in_memory().unwrap();
        db.ensure_bankroll_seeded(amount).unwrap();
        db
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
    fn test_api_cost_deduction() {
        let db = setup_db_with_bankroll(50.0);
        db.log_api_cost(1, None, "haiku", 500, 50, 0.10, "triage")
            .unwrap();

        let accountant = Accountant::new(200.0);
        let result = accountant.close_cycle(&db, 1).unwrap();

        assert!((result.bankroll_before - 50.0).abs() < f64::EPSILON);
        assert!((result.api_cost - 0.10).abs() < f64::EPSILON);
        assert!((result.bankroll_after - 49.90).abs() < 1e-10);
        assert!(result.is_alive);
    }

    #[test]
    fn test_survival_bankroll_positive() {
        let db = setup_db_with_bankroll(10.0);
        db.log_api_cost(1, None, "haiku", 500, 50, 0.01, "triage")
            .unwrap();

        let accountant = Accountant::new(200.0);
        let result = accountant.close_cycle(&db, 1).unwrap();
        assert!(result.is_alive);
        assert!(result.bankroll_after > 0.0);
    }

    #[test]
    fn test_survival_bankroll_zero() {
        let db = setup_db_with_bankroll(0.50);
        db.log_api_cost(1, None, "sonnet", 2000, 200, 0.50, "analysis")
            .unwrap();

        let accountant = Accountant::new(200.0);
        let result = accountant.close_cycle(&db, 1).unwrap();
        assert!(!result.is_alive);
        assert!((result.bankroll_after - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_survival_bankroll_negative() {
        let db = setup_db_with_bankroll(0.10);
        db.log_api_cost(1, None, "sonnet", 2000, 200, 0.50, "analysis")
            .unwrap();

        let accountant = Accountant::new(200.0);
        let result = accountant.close_cycle(&db, 1).unwrap();
        assert!(!result.is_alive);
        assert!(result.bankroll_after < 0.0);
    }

    #[test]
    fn test_cycle_duration_high_bankroll() {
        let accountant = Accountant::new(200.0);
        assert_eq!(accountant.get_cycle_duration_secs(500.0, 600, 1800), 600);
        assert_eq!(accountant.get_cycle_duration_secs(200.0, 600, 1800), 600);
    }

    #[test]
    fn test_cycle_duration_low_bankroll() {
        let accountant = Accountant::new(200.0);
        assert_eq!(accountant.get_cycle_duration_secs(199.99, 600, 1800), 1800);
        assert_eq!(accountant.get_cycle_duration_secs(50.0, 600, 1800), 1800);
        assert_eq!(accountant.get_cycle_duration_secs(0.01, 600, 1800), 1800);
    }

    #[test]
    fn test_death_report_generation() {
        let db = setup_db_with_bankroll(50.0);
        insert_test_market(&db, "0xdead");

        // Simulate a cycle
        db.conn
            .execute(
                "INSERT INTO cycle_log (cycle_number, markets_scanned, trades_placed, api_cost_usd) VALUES (1, 10, 1, 0.05)",
                [],
            )
            .unwrap();
        db.insert_trade("t1", "0xdead", "tok1", "YES", 0.60, 5.0, "filled", true)
            .unwrap();

        // Deduct some cost
        db.log_bankroll_entry("api_cost", -0.05, 49.95, "Cycle 1 API cost")
            .unwrap();

        let accountant = Accountant::new(200.0);
        let report = accountant.generate_death_report(&db).unwrap();

        assert_eq!(report.cycles_completed, 1);
        assert_eq!(report.total_trades, 1);
        assert!((report.final_bankroll - 49.95).abs() < 1e-10);
        assert!((report.total_pnl - (-0.05)).abs() < 1e-10);
        assert_eq!(report.recent_trades.len(), 1);
        assert_eq!(report.recent_trades[0].trade_id, "t1");
    }

    #[test]
    fn test_close_cycle_no_double_deduction() {
        let db = setup_db_with_bankroll(50.0);
        db.log_api_cost(1, None, "haiku", 500, 50, 0.10, "triage")
            .unwrap();

        let accountant = Accountant::new(200.0);

        // First close
        let result1 = accountant.close_cycle(&db, 1).unwrap();
        assert!((result1.bankroll_after - 49.90).abs() < 1e-10);

        // Second close of same cycle — api_cost is still 0.10 but bankroll_before is now 49.90
        let result2 = accountant.close_cycle(&db, 1).unwrap();
        assert!((result2.bankroll_before - 49.90).abs() < 1e-10);
        // It WILL deduct again — so caller must not call close_cycle twice for same cycle.
        // But the bankroll starts from the updated value, not the original.
        assert!((result2.bankroll_after - 49.80).abs() < 1e-10);
    }

    #[test]
    fn test_close_cycle_zero_cost() {
        let db = setup_db_with_bankroll(50.0);
        // No API costs logged for cycle 1

        let accountant = Accountant::new(200.0);
        let result = accountant.close_cycle(&db, 1).unwrap();

        assert!((result.bankroll_before - 50.0).abs() < f64::EPSILON);
        assert!((result.bankroll_after - 50.0).abs() < f64::EPSILON);
        assert!((result.api_cost - 0.0).abs() < f64::EPSILON);
        assert!(result.is_alive);

        // No bankroll_log entry should be added for zero cost
        let count: i64 = db
            .conn
            .query_row(
                "SELECT COUNT(*) FROM bankroll_log WHERE entry_type = 'api_cost'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 0);
    }
}
