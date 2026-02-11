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
