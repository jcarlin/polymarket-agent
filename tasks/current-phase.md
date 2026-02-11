# Phase 3: Position Sizing & Execution

## Status: COMPLETE

### Checklist

- [x] `src/config.rs` — 5 new fields (kelly_fraction, max_position_pct, max_total_exposure_pct, initial_bankroll, executor_request_timeout_secs)
- [x] `.env.example` — Phase 3 vars uncommented
- [x] `src/db.rs` — PositionRow struct + 7 new methods (insert_trade, upsert_position, log_bankroll_entry, get_current_bankroll, get_total_exposure, get_open_positions, ensure_bankroll_seeded) + 9 tests
- [x] `src/position_sizer.rs` — Kelly Criterion with half-Kelly, position cap, exposure limit (10 tests)
- [x] `src/executor.rs` — Paper + live mode execution, DB logging (8 tests)
- [x] `src/lib.rs` — position_sizer + executor modules registered
- [x] `src/main.rs` — Full Phase 3 pipeline: sizing → execution → bankroll update
- [x] `sidecar/polymarket_client.py` — place_order method via py-clob-client
- [x] `sidecar/server.py` — POST /order endpoint (503 if not initialized)
- [x] `sidecar/test_server.py` — 3 new tests (503, validation, endpoint exists)
- [x] `tests/integration.rs` — 2 new tests (kelly basic, paper trade e2e)
- [x] `cargo build` — passes, zero warnings
- [x] `cargo test` — all 77 tests pass (72 unit + 5 integration)
- [x] `cargo clippy -- -W clippy::all` — zero warnings
- [x] `cd sidecar && python -m pytest` — all 7 tests pass
