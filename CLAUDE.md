# Polymarket Autonomous Trading Agent

## Current Phase: Phase 2 — Claude Analysis Engine & Edge Detection

Build this project phase by phase. Do NOT skip ahead. Complete each phase fully,
ensure all tests pass and `cargo build` succeeds, then STOP and let me review
before moving to the next phase.

After completing a phase, run `/next-phase` to advance.

**Before starting work:** Read `tasks/lessons.md` for known patterns and pitfalls.
**During a phase:** Track granular progress in `tasks/current-phase.md`.
**Workflow rules:** See `docs/claude-code-workflow.md` for plan mode, subagent, and session management guidance.

---

## Tech Stack

- **Rust** (core agent: scheduler, analysis orchestration, risk management, dashboard)
- **Python sidecar** (FastAPI on localhost:9090 — Polymarket trading via `py-clob-client`, weather data via Open-Meteo + GEFS)
- **SQLite** (trade journal, position history, bankroll ledger)
- **Single-file HTML dashboard** (vanilla JS + Chart.js CDN, no build step)

## Project Goal

Autonomous trading agent for Polymarket prediction markets with a survival mechanic:
starts with $50 USDC on Polygon, pays its own Claude API costs from profits, shuts
down ("dies") if bankroll hits $0. Zero human intervention after deployment.

## Architecture: Rust Core + Python Sidecar

Polymarket has NO official Rust client (only Python, TypeScript, Go). The CLOB API
requires EIP-712 order signing. Rather than reimplementing crypto signing in Rust,
we use Polymarket's official `py-clob-client` as a thin FastAPI sidecar. This sidecar
also handles weather data fetching (Open-Meteo API, GEFS ensemble parsing).

**Rust ↔ Python communication:** HTTP on localhost:9090. Sidecar is stateless — all
state lives in Rust (SQLite + in-memory bankroll). Rust spawns sidecar as subprocess.

```
polymarket-agent/
├── Cargo.toml
├── CLAUDE.md                           ← You are here
├── src/                                # Rust core
│   ├── main.rs                         # Entry point, scheduler, lifecycle
│   ├── config.rs                       # All configurable params from .env
│   ├── market_scanner.rs               # Gamma API client (market discovery, no auth)
│   ├── data_sources/
│   │   ├── mod.rs
│   │   ├── openclaw.rs                 # OpenClaw integration (research tasks)
│   │   ├── sports.rs                   # Sports data consumer
│   │   └── crypto.rs                   # On-chain + sentiment data
│   ├── estimator.rs                    # Claude API integration (analysis brain)
│   ├── edge_detector.rs                # Fair value vs market price comparison
│   ├── position_sizer.rs               # Kelly Criterion with bankroll cap
│   ├── executor.rs                     # Calls Python sidecar for order execution
│   ├── position_manager.rs             # Stop-loss, take-profit, whale/volume monitoring
│   ├── accounting.rs                   # Bankroll, API costs, survival check
│   ├── logger.rs                       # Structured logging to file + SQLite
│   ├── dashboard.rs                    # Axum HTTP server for web dashboard
│   └── websocket.rs                    # WebSocket for live dashboard updates
├── sidecar/                            # Python sidecar
│   ├── requirements.txt                # py-clob-client, fastapi, uvicorn, requests, numpy, scipy
│   ├── server.py                       # FastAPI app on localhost:9090
│   ├── polymarket_client.py            # py-clob-client wrapper (order signing, positions)
│   └── weather/
│       ├── open_meteo.py               # Open-Meteo Ensemble API client (GEFS + ECMWF)
│       └── probability_model.py        # Ensemble members → 2°F bucket probabilities (scipy KDE)
├── prompts/
│   └── fair_value.md                   # Claude prompt template
├── static/
│   └── dashboard.html                  # Single-file frontend
├── docs/
│   ├── weather-research.md             # Weather data feasibility research
│   ├── api-reference.md                # Polymarket API endpoints + examples
│   ├── openclaw-integration.md         # OpenClaw setup, Simmer SDK context, research layer
│   └── claude-code-workflow.md         # Plan mode, subagents, session management rules
├── tasks/
│   ├── lessons.md                      # Lessons learned (read at session start)
│   └── current-phase.md               # Granular checklist for active phase
├── .claude/
│   ├── agents/
│   │   ├── rust-builder.md             # Rust build/test specialist subagent
│   │   └── python-sidecar.md           # Python sidecar specialist subagent
│   └── commands/
│       ├── next-phase.md               # Phase transition command
│       └── ralph-phase.md              # Ralph loop for current phase
├── .env.example
└── README.md
```

---

## Polymarket Integration (Three APIs + WebSocket)

The agent uses THREE separate Polymarket services plus a WebSocket:

| Service | URL | Purpose | Auth |
|---------|-----|---------|------|
| **Gamma API** | `https://gamma-api.polymarket.com` | Market discovery, metadata, pagination | None (free reads) |
| **CLOB API** | `https://clob.polymarket.com` | Orderbook, prices, order placement | EIP-712 signed (via sidecar) |
| **Data API** | `https://data-api.polymarket.com` | User positions, trade history, P&L | API key |
| **WebSocket** | `wss://ws-subscriptions-clob.polymarket.com` | Real-time price/order updates | Subscribe to channels |

**Per-cycle workflow:**
1. Gamma API → discover + filter markets (paginated, 50/page)
2. CLOB API → orderbook/prices for candidates
3. Claude analyzes → agent decides to trade
4. CLOB API (via sidecar) → place signed limit order
5. Data API → confirm positions, track P&L, check resolutions

**Auth:** Polygon chain ID 137, USDC collateral, EIP-712 signed orders.
**Funding:** Manual one-time seed — buy USDC on exchange → withdraw to Polygon wallet.
**Orders:** Limit orders with `postOnly` flag. Batch up to 15 orders per call.

See `docs/api-reference.md` for full endpoint documentation and code examples.

---

## Core Loop (every ~10 minutes, adaptive)

1. **Scan Markets** — Gamma API, paginated, filter by liquidity/category/resolution time
2. **Get Prices** — CLOB API orderbook for filtered candidates (or WebSocket subscription)
3. **Enrich Data** — Query weather sidecar for ensemble probabilities; dispatch OpenClaw for sports/crypto/news (see `docs/openclaw-integration.md`)
4. **Analyze with Claude** — Structured prompt → Claude returns `{ probability, confidence, reasoning, data_quality }`
5. **Find Mispricing** — Compare Claude estimate vs market price; flag if edge > 8%
6. **Size Position** — Kelly Criterion, capped at 6% bankroll, half-Kelly for variance reduction
7. **Execute Trade** — HTTP call to Python sidecar → `py-clob-client` signs + places on CLOB API
8. **Manage Positions** — Check stop-loss/take-profit, whale movements, volume spikes on open positions
9. **Self-Fund** — Deduct Claude API costs from bankroll; die if bankroll ≤ $0

---

## Claude API Integration (The Analysis Brain)

Claude receives structured context per market and returns:
```json
{ "probability": 0.72, "confidence": 0.85, "reasoning": "...", "data_quality": "high" }
```

**Model tiering for cost control:**
- `claude-haiku-4-5-20251001` — Initial triage/filtering (quick reject)
- `claude-sonnet-4-5-20250929` — Full fair value analysis on candidates
- `claude-opus-4-6` — Optional, for high-conviction large positions

**Track token usage per call, convert to USD, deduct from bankroll.**

See `prompts/fair_value.md` for the prompt template.

---

## API Cost Budget (CRITICAL — Agent Dies If Math Fails)

Default settings produce ~$109/day in Claude costs. **$50 bankroll dies in 11 hours.**

**Survivable configuration (MUST implement):**
- Programmatic pre-filter to ~50-100 markets before Haiku triage
- Weather-only mode at start (20 Sonnet calls/cycle, no broad triage)
- 30-min cycles when bankroll < $200; 10-min cycles above $200
- Hard cap: MAX_API_COST_PER_CYCLE = $0.50

**Target burn rate:** ~$13/day with weather-only + 30-min cycles.

---

## Weather Trading Strategy (Primary Edge)

**See `docs/weather-research.md` for full feasibility research.**

Core approach: Open-Meteo Ensemble API serves all 31 GEFS members + 51 ECMWF members
as JSON. No GRIB2 parsing needed for MVP. Gaussian KDE over 82 ensemble members →
probability distribution over 2°F temperature buckets → compare vs Polymarket prices.

**Key trade:** When market prices outcome >3% but ensemble model says <1% → buy NO at 98-99¢.

**Resolution source:** Weather Underground data for specific airport METAR stations
(KLGA for NYC, KORD for Chicago, etc.). Model must predict at exact airport coordinates.

**20 US cities in parallel** for natural diversification. Nearby cities (NYC/PHL/BOS)
treated as partially correlated for exposure limits.

---

## Kelly Criterion Position Sizing

```
kelly_fraction = (edge / odds) - ((1 - edge) / (payout - 1))
position_size = min(kelly_fraction * bankroll * KELLY_FRACTION, MAX_POSITION_PCT * bankroll)
```
- Use half-Kelly (KELLY_FRACTION=0.5) to reduce variance
- Hard cap at 6% of bankroll per position
- Never bet if Kelly suggests negative sizing

---

## Risk Management

- Max single position: 6% bankroll
- Max total exposure: 40% bankroll
- Min edge threshold: 8%
- Weather correlation: NYC/PHL/BOS treated as partially correlated
- Drawdown circuit breaker: if bankroll drops >30% from peak, reduce sizing or pause
- Min market liquidity: $500

---

## Active Position Management (every cycle)

- **Stop-loss**: Exit if position down >15% or edge drops below 2%
- **Take-profit**: Sell if captured >90% of expected value
- **Whale monitoring**: Re-evaluate if opposing whale move >$5k
- **Volume spikes**: Re-analyze if volume >3x hourly average
- **News triggers**: OpenClaw breaking news → immediate Claude re-analysis

---

## Survival Mechanic

- Manually-funded USDC wallet on Polygon (seed $50)
- Real-time bankroll ledger: `+winnings +unrealized −fees −claude_api_costs`
- **Death condition:** `available_cash + liquidation_value ≤ 0` → graceful shutdown
- Death report: full trade history, P&L breakdown, cause of death

---

## Web Dashboard (Read-Only, Real-Time)

Single-page HTML + vanilla JS + Chart.js (CDN). Axum/Warp serves static file + WebSocket.
No React, no npm, no Docker. Runs on $4.50 VPS.

**Views:** Live trade feed, portfolio overview, P&L charts, agent status/heartbeat.
**Real-time:** WebSocket push on every trade and every cycle.
**Auth:** HTTP Basic Auth. No control buttons — fully autonomous, observe only.
**Mode indicator:** Prominently shows PAPER or LIVE.

---

## Execution Modes

```env
TRADING_MODE=paper    # "paper" or "live"
```
- **Paper:** Full loop, simulated orders, real Claude API costs deducted from virtual bankroll
- **Live:** Real orders, real money, real death

Paper mode is a 24-48h smoke test, not a backtesting sandbox.

---

## Deployment

- VPS: Hetzner CX22 / Contabo / Oracle Cloud free tier (2GB RAM, 20GB disk minimum)
- Rust binary: single static executable via systemd
- Python sidecar: subprocess or separate systemd unit
- Death exit code: distinct code that systemd does NOT restart

---

## Build Phases

### Phase 1 — Skeleton, Market Scanner & Sidecar
- Cargo.toml with dependencies (reqwest, serde, tokio, axum, rusqlite)
- config.rs loading .env
- market_scanner.rs: Gamma API client, paginate active markets, filter by category/liquidity
- Python sidecar: FastAPI scaffold, health check endpoint, py-clob-client wrapper
- Rust spawns sidecar as subprocess, health check on startup
- SQLite schema (markets, trades, positions, bankroll_log, cycle_log)
- Tests for Gamma API client (mock responses) and sidecar health check

### Phase 2 — Claude Analysis Engine & Edge Detection
- estimator.rs: Claude API client, structured prompt, parse JSON response
- Haiku triage + Sonnet deep analysis pipeline
- edge_detector.rs: compare Claude probability vs market midpoint
- API cost tracking per call (token count → USD)
- prompts/fair_value.md loaded at runtime
- Tests with mock Claude responses

### Phase 3 — Position Sizing & Execution
- position_sizer.rs: Kelly Criterion with half-Kelly and bankroll cap
- executor.rs: HTTP call to sidecar /order endpoint
- Sidecar polymarket_client.py: py-clob-client order signing + placement
- Trade confirmation via Data API
- Paper mode: log simulated trades to SQLite
- Tests for Kelly math, executor ↔ sidecar integration

### Phase 4 — Accounting & Survival
- accounting.rs: bankroll ledger, API cost deduction, survival check
- Death condition and graceful shutdown with death report
- Cost-adaptive cycle frequency (30min when low bankroll, 10min when high)
- Tests for survival math edge cases

### Phase 5 — Weather Pipeline (Highest Edge)
- sidecar/weather/open_meteo.py: Open-Meteo Ensemble API for GEFS (31 members) + ECMWF (51 members)
- sidecar/weather/probability_model.py: scipy KDE → 2°F bucket probabilities
- FastAPI endpoint: GET /weather/probabilities?city=NYC&date=YYYY-MM-DD
- Direct GEFS/ECMWF from AWS S3 as Tier 2 upgrade (Herbie library)
- 20-city parallel fetching, airport coordinate targeting
- Weather-specific NO-side trade logic
- Tests for probability model accuracy

### Phase 6 — Position Management & Risk
- position_manager.rs: stop-loss, take-profit, edge re-evaluation
- Whale wallet monitoring (Polygon RPC)
- Volume spike detection (CLOB API volume data)
- Exposure limits, correlation checks, drawdown circuit breaker
- OpenClaw integration for news-triggered re-evaluation
- Error handling, retry logic, graceful degradation

### Phase 7 — Web Dashboard
- dashboard.rs: Axum HTTP server serving static/dashboard.html
- websocket.rs: WebSocket server for live push updates
- dashboard.html: trade feed, portfolio table, P&L charts, agent status
- HTTP Basic Auth
- PAPER/LIVE mode indicator

---

## Configuration Reference

See `.env.example` for all configuration variables with defaults and documentation.

---

## Key Rules for Claude Code

1. **Rust code:** Use `tokio` for async, `reqwest` for HTTP, `serde` for JSON, `rusqlite` for SQLite, `axum` for web server, `tracing` for logging.
2. **Python sidecar:** FastAPI + uvicorn, `py-clob-client` for Polymarket, `requests` for HTTP, `numpy` + `scipy` for probability model.
3. **Error handling:** Every external call (API, sidecar, database) must have retry logic and graceful fallback. Never panic in production code.
4. **Testing:** Every module gets unit tests. Use mock HTTP responses for external APIs. Integration tests for Rust ↔ sidecar communication.
5. **Logging:** Every decision logged — market scanned, edge found, trade placed, cost incurred. Both file and SQLite.
6. **The agent must not die from a bug.** Death is ONLY from bankroll hitting $0. Crashes restart via systemd. Sidecar failures pause trading, don't kill the agent.
7. **Ship it, then improve it.** Working > elegant. The agent needs to survive, not win a code review. Refactor after profitability is proven.
8. **Fix bugs autonomously.** When a test fails, read the error, find the cause, fix it. Don't ask for hand-holding. Escalate only after 3 failed attempts.
9. **Plan mode at phase boundaries only.** Use plan mode when starting a new phase or making architectural decisions. For implementation work, just execute.
10. **Read `tasks/lessons.md` at session start.** Known pitfalls are documented there. Don't repeat mistakes.
11. **Update `tasks/lessons.md` at phase boundaries.** After corrections, after fixing non-trivial bugs, after discovering gotchas — capture the pattern.
12. **Context window hygiene.** Use `/clear` between phases. Use "Document & Clear" (dump progress to .md, clear, restart) for long sessions. Don't let auto-compaction lose important context.
