# Phase 4: Accounting & Survival

## Status: COMPLETE

### Checklist

- [x] `src/config.rs` — 4 new fields (cycle_frequency_high_secs, cycle_frequency_low_secs, low_bankroll_threshold, death_exit_code)
- [x] `.env.example` — Phase 4 vars added
- [x] `src/market_scanner.rs` — test_config() updated with new fields
- [x] `src/db.rs` — TradeRow struct + 3 new methods (get_next_cycle_number, get_total_trades_count, get_recent_trades) + 4 tests
- [x] `src/accounting.rs` — Accountant, CycleAccounting, DeathReport structs + close_cycle, get_cycle_duration_secs, generate_death_report methods + 9 tests
- [x] `src/lib.rs` — accounting module registered
- [x] `src/main.rs` — Recurring loop with: cycle numbering from DB, API cost deduction, survival check, death report + exit(42), adaptive sleep, Ctrl+C graceful shutdown
- [x] `tests/integration.rs` — 3 new tests (api_cost_deduction, death_condition, cycle_number_persistence)
- [x] `cargo build` — passes, zero warnings
- [x] `cargo test` — all 93 tests pass (85 unit + 8 integration)
- [x] `cargo clippy -- -W clippy::all` — zero warnings
