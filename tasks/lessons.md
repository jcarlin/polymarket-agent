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
