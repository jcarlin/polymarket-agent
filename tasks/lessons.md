# Lessons Learned

Patterns, pitfalls, and gotchas discovered during development.
Read this at the start of every session.

---

## Phase 1

- **Gamma API volume/liquidity as strings:** The API sometimes returns these as strings instead of numbers, or as null. Use a custom `deserialize_optional_f64` that handles `f64 | String | null` via `#[serde(untagged)]` enum.
- **Cargo not in default PATH:** On this machine, cargo lives at `~/.cargo/bin/cargo` and is NOT in the shell PATH by default. Subagents need `export PATH="$HOME/.cargo/bin:$PATH"`.
- **web3 pinning:** `web3==6.14.0` is required to avoid `eth-typing` dependency conflicts with `py-clob-client`.
- **Sidecar venv:** The Python sidecar venv lives at `sidecar/.venv/`. Phase 1 only installs the lightweight deps (fastapi, uvicorn, httpx, pytest, ruff) — heavy deps (py-clob-client, scipy, numpy) deferred until needed.
- **Database WAL mode:** Enable WAL mode for SQLite to allow concurrent reads during writes. In-memory test DBs skip WAL since it's not needed.
- **Sidecar startup is non-fatal:** `main.rs` catches sidecar spawn failures and continues without it. This is intentional — the agent can still scan markets without the sidecar.

## Phase 2

- **CLOB API returns prices as strings:** All CLOB endpoints (`/midpoint`, `/price`, `/book`) return prices as JSON strings, not numbers. Parse with `.parse::<f64>()`.
- **Claude may fence JSON in markdown:** Claude sometimes wraps JSON output in ` ```json ... ``` ` code fences. The `parse_estimate` method must strip these before parsing.
- **IEEE 754 float precision in tests:** `0.63 - 0.55` produces `0.07999...`, not `0.08`. Use values that compute cleanly or add epsilon tolerance in assertions.
- **Anthropic API version header required:** Must send `anthropic-version: 2023-06-01` header or get 400 errors.
- **Estimator gracefully handles missing API key:** When `ANTHROPIC_API_KEY` is empty, main.rs skips Claude analysis entirely rather than failing.
- **Parallel agent coordination:** When 3+ agents modify shared files (lib.rs, config.rs), earlier agents may create stubs that later agents overwrite. Design tasks so only one agent owns each file, or accept last-write-wins for additive changes like module declarations.
- **Team of 4 agents for Phase 2:** Streams A (config+db), B (clob_client), C (estimator) ran in parallel; Stream D (edge_detector + integration) ran after. Total wall time significantly reduced vs sequential.

## Phase 3

- **SQLite foreign key enforcement is ON:** The `trades` and `positions` tables have FK constraints referencing `markets(condition_id)`. Test code must insert a market row first before inserting trades/positions, or the INSERT will fail with "FOREIGN KEY constraint failed".
- **Kelly formula for binary markets:** `(win_prob - buy_price) / (1 - buy_price)` where `buy_price = market_price` for YES, `1 - market_price` for NO. Simple and correct — no need for the more complex multi-outcome Kelly.
- **Paper mode needs no sidecar:** Paper execution generates a UUID, logs to DB, updates bankroll — all in-process. Live mode just adds one HTTP POST to sidecar before the same DB writes. This separation keeps testing fast and isolated.

## Phase 4

- **Config struct literals in tests:** When adding fields to `Config`, the `test_config()` helper in `market_scanner.rs` must be updated too — it constructs `Config` directly (not via `from_env()`). Grep for `Config {` to find all literal constructors.
- **Accountant is stateless by design:** Reads all state from DB each cycle. This means the agent can crash and restart cleanly — cycle number is seeded from `MAX(cycle_number)` in `cycle_log`.
- **Zero API cost skip:** When a cycle has no API calls, `close_cycle()` skips writing a bankroll_log entry entirely. This avoids noise in the ledger and prevents divide-by-zero edge cases.
- **`tokio::select!` for Ctrl+C:** Always use `tokio::select!` between sleep and `ctrl_c()` — even with zero-duration sleep. Avoids needing the `futures` crate for `now_or_never()`.

## Phase 5

- **Weather is additive context, not a replacement.** Ensemble probabilities are passed as extra data in the Claude prompt. Claude weighs them alongside other factors. Non-weather markets proceed normally with `weather: None`.
- **WeatherContext borrows from cache.** The `WeatherContext<'a>` struct holds a reference to `WeatherProbabilities` from the per-cycle HashMap cache. Cache lives for the entire loop iteration, so borrows are safe.
- **Sidecar response doesn't include raw member temps.** The `/weather/probabilities` endpoint returns bucket probabilities, mean, and std — not the 82 raw member temperatures. The prompt renders what we have (bucket probs + statistics). Raw temps could be added in a Tier 2 upgrade.
- **HashMap entry API for clippy.** Using `contains_key()` + `insert()` triggers clippy's `map_entry` lint. Use `Entry::Vacant(entry)` pattern instead.
- **`parse_weather_market` is best-effort regex.** If it can't parse the question, returns None and the market goes through normal non-weather analysis. No trades are missed.
- **Parallel Python + Rust agent streams work well.** Phase 5 used 3 streams: Python sidecar (weather module + endpoint), Rust (weather_client + config), Integration (estimator + main.rs). Python and Rust ran in parallel, integration after both completed.
- **WU is an Angular SPA — HTML scraping returns empty shell.** Use Weather.com JSON API instead: `api.weather.com/v1/location/{ICAO}:9:US/observations/historical.json` for actuals, `/v3/wx/forecast/daily/5day` for forecast. Daily high = max(temp) from hourly observations.
- **NWS weight of 0.85 dominates the blend.** Ensemble spread still matters for uncertainty estimation, but the NWS anchor prevents the systematic cold bias of raw GEFS/ECMWF ensembles.
- **Four-layer correction order matters:** NWS → calibration → HRRR → WU forecast. Each shift is relative to the current (already-shifted) mean, so reordering changes the result.
- **Calibration needs min 5 observations per city before it activates.** Below that threshold, `WEATHER_DEFAULT_BIAS_OFFSET=4.0` bridges the NWS→WU gap.
- **HRRR only helps for same-day markets.** Longer-range markets should skip it (`same_day=false`). HRRR has an 18-48hr forecast horizon.
- **WU forecast API runs ~4°F cold vs WU website.** The `WEATHER_WU_FORECAST_WEIGHT=0.25` is set conservatively because of this discrepancy.

## Phase 6

- **f64 boundary tests need epsilon slack.** `0.45/0.50 = 0.8999...` not `0.90`, so `>= 0.90` fails at exact boundary. Use values like `0.951` instead of `0.95` to avoid IEEE 754 precision traps in threshold tests. Same pattern as Phase 2 lesson but easy to forget.
- **`map_or(false, ...)` → `is_some_and(...)`:** Clippy now flags the older pattern. Use `is_some_and()` for cleaner Option/Result boolean checks.
- **`new()` requires `Default` impl.** If a struct has `pub fn new() -> Self` with no args, clippy demands `impl Default`. Add it or derive it.
- **Drawdown-adjusted sizing in main loop.** When drawdown circuit breaker is active, construct a *new* `PositionSizer` with reduced `kelly_fraction` (`kelly * drawdown_reduction`). The original sizer stays available for when drawdown clears.
- **Position exit reuses `/order` endpoint.** No new sidecar endpoint needed — existing `/order` with side="SELL" handles exits. Keep the sidecar surface area minimal.
- **`ALTER TABLE ADD COLUMN` for existing tables.** Wrap in `let _ =` to make idempotent — SQLite errors if column already exists. This pattern works for incremental migrations without a migration framework.
