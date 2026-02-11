# Phase 6: Position Management & Risk

## Status: COMPLETE

### Checklist

- [x] `CLAUDE.md` — Updated "Current Phase" to Phase 6
- [x] `src/config.rs` — +9 config fields (stop_loss, take_profit, drawdown, etc.)
- [x] `.env.example` — +9 env vars for position management
- [x] `src/market_scanner.rs` — test_config() updated with new fields
- [x] `src/db.rs` — 2 new tables (peak_bankroll, position_alerts), PositionRow extension, 6+ new methods
- [x] `src/position_manager.rs` — NEW: core position management module + tests
- [x] `src/lib.rs` — +2 module declarations (position_manager, data_sources)
- [x] `src/executor.rs` — +exit_position/exit_paper/exit_live + tests
- [x] `sidecar/server.py` — order_type field on OrderRequest
- [x] `src/data_sources/mod.rs` — NEW: module declaration
- [x] `src/data_sources/openclaw.rs` — NEW: stub interface
- [x] `src/main.rs` — Wire position management into cycle loop
- [x] `tests/integration.rs` — +4 integration tests
- [x] `cargo build` — passes
- [x] `cargo test` — 149 tests pass (134 unit + 15 integration)
- [x] `cargo clippy` — 0 warnings
- [x] `cd sidecar && python -m pytest -v` — 30 tests pass
- [x] `tasks/lessons.md` — Phase 6 learnings added

### Summary

Added the defensive layer that prevents unmanaged losing positions:
- **Stop-loss** (15% threshold), **take-profit** (90% captured value), **edge decay** (2% min edge)
- **Drawdown circuit breaker**: reduces Kelly fraction 50% when bankroll drops >30% from peak
- **Correlation groups**: 5 geographic regions, 15% max exposure per group
- **Position exits**: paper + live mode via existing sidecar `/order` endpoint
- **OpenClaw stub**: interface established for Phase 7+ news integration
- **DB schema**: peak_bankroll tracking, position_alerts audit log, estimated_probability on positions
