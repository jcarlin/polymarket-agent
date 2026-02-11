# Usage Guide

## What This Is

An autonomous trading agent for Polymarket prediction markets. It starts with
$50 USDC on Polygon, pays its own Claude API costs from trading profits, and
shuts down ("dies") when bankroll hits $0. Weather temperature markets are the
primary edge — the agent uses 82 ensemble forecast members to find mispriced
outcomes.

## Prerequisites

- **Rust toolchain** — install via [rustup](https://rustup.rs/) (stable channel)
- **Python 3.11+** with `venv` module
- **Anthropic API key** — sign up at [api.anthropic.com](https://api.anthropic.com)
- **(Live mode only)** A Polygon wallet funded with USDC

## Installation

```bash
git clone <repo-url> && cd polymarket-agent

# Configure
cp .env.example .env
# Edit .env — at minimum set ANTHROPIC_API_KEY

# Install Python sidecar dependencies
cd sidecar
python3 -m venv .venv
source .venv/bin/activate
pip install -r requirements.txt
cd ..

# Build the Rust binary
cargo build --release
```

The compiled binary is at `./target/release/polymarket-agent`.

## Configuration

All configuration lives in `.env`. Copy `.env.example` and edit as needed.

### Critical Variables

| Variable | Default | Description |
|----------|---------|-------------|
| `TRADING_MODE` | `paper` | `paper` for simulated trades, `live` for real money |
| `ANTHROPIC_API_KEY` | *(empty)* | Required. Claude API key for market analysis |
| `POLYMARKET_WALLET_PRIVATE_KEY` | *(empty)* | Polygon EOA private key (`0x...`). Required for live mode |
| `INITIAL_BANKROLL` | `50.0` | Starting bankroll in USD |
| `DATABASE_PATH` | `data/polymarket-agent.db` | SQLite database location |
| `DASHBOARD_PORT` | `8080` | Web dashboard port |
| `DASHBOARD_USER` | `admin` | Dashboard HTTP Basic Auth username |
| `DASHBOARD_PASSWORD` | *(empty)* | Dashboard password (set this before exposing to network) |

### Paper Mode Minimum

Only one variable is strictly required:

```env
ANTHROPIC_API_KEY=sk-ant-...
```

Everything else has sane defaults. The agent will run in paper mode with a
virtual $50 bankroll, simulating trades against real market data.

### Live Mode Minimum

```env
TRADING_MODE=live
ANTHROPIC_API_KEY=sk-ant-...
POLYMARKET_WALLET_PRIVATE_KEY=0x...
```

### Safety & Risk Settings

| Variable | Default | Description |
|----------|---------|-------------|
| `MIN_EDGE_THRESHOLD` | `0.08` | Minimum edge to trade (8%) |
| `KELLY_FRACTION` | `0.5` | Half-Kelly for variance reduction |
| `MAX_POSITION_PCT` | `0.06` | Max 6% of bankroll per position |
| `MAX_TOTAL_EXPOSURE_PCT` | `0.40` | Max 40% bankroll in open positions |
| `STOP_LOSS_PCT` | `0.15` | Exit if position down >15% |
| `TAKE_PROFIT_PCT` | `0.90` | Exit if captured >90% of expected value |
| `MIN_EXIT_EDGE` | `0.02` | Exit if edge drops below 2% |
| `DRAWDOWN_CIRCUIT_BREAKER_PCT` | `0.30` | Halve sizing if bankroll drops >30% from peak |
| `MAX_CORRELATED_EXPOSURE_PCT` | `0.15` | Max 15% bankroll in correlated weather group |
| `MAX_API_COST_PER_CYCLE` | `0.50` | Hard cap on Claude API spend per cycle |

### Cycle Timing

| Variable | Default | Description |
|----------|---------|-------------|
| `CYCLE_FREQUENCY_HIGH_SECS` | `600` | 10 min cycles when bankroll >= threshold |
| `CYCLE_FREQUENCY_LOW_SECS` | `1800` | 30 min cycles when bankroll < threshold |
| `LOW_BANKROLL_THRESHOLD` | `200.0` | Switch to slower cycles below this |

## Running the Agent

```bash
./target/release/polymarket-agent
```

### Startup Sequence

On launch, the agent:

1. Loads `.env` configuration
2. Initializes SQLite database (creates `data/` directory if needed)
3. Seeds bankroll if first run
4. Spawns the Python sidecar on `localhost:9090`
5. Starts the web dashboard on the configured port
6. Begins the recurring trading loop

You'll see output like:

```
Polymarket Agent starting in paper mode
Database initialized at data/polymarket-agent.db
Sidecar spawned and healthy
Dashboard spawned on port 8080
Starting recurring loop at cycle 1 (bankroll: $50.00)
═══ Cycle 1 starting ═══
```

### The Trading Cycle

Each cycle runs these steps:

1. **Scan markets** — Query Gamma API for active markets, filter by liquidity and volume
2. **Get prices** — Fetch CLOB orderbook prices for each candidate
3. **Fetch weather data** — Get ensemble forecast probabilities for weather markets
4. **Analyze with Claude** — Haiku triage (cheap reject) then Sonnet deep analysis
5. **Detect edge** — Compare Claude's probability estimate vs market price
6. **Size positions** — Half-Kelly criterion, capped at 6% bankroll
7. **Execute trades** — Place orders via the Python sidecar (paper or live)
8. **Manage positions** — Check stop-loss, take-profit, edge decay on open positions
9. **Close cycle** — Deduct API costs, check survival, sleep until next cycle

### Stopping the Agent

Press `Ctrl+C` for a graceful shutdown. The sidecar process is terminated
automatically.

## Dashboard

The web dashboard is served at `http://localhost:8080` (or your configured
`DASHBOARD_PORT`).

### Authentication

Set `DASHBOARD_PASSWORD` in `.env` to enable HTTP Basic Auth. The username
defaults to `admin` (configurable via `DASHBOARD_USER`).

### Panels

- **Stats bar** — Bankroll, exposure, total trades, 24h API cost, cycle number, trading mode
- **Bankroll chart** — Historical bankroll over time (Chart.js line chart)
- **Open positions** — Active positions with entry price, current price, unrealized P&L
- **Recent trades** — Trade history with side, price, size, and outcome
- **Alerts** — Stop-loss triggers, correlation warnings, drawdown circuit breaker activations

### Live Updates

The dashboard connects via WebSocket for real-time push updates on every trade
and cycle completion. If the WebSocket disconnects, it falls back to 30-second
polling via REST endpoints.

The trading mode badge (`PAPER` or `LIVE`) is prominently displayed in the header
alongside a heartbeat indicator.

## Paper Mode vs Live Mode

| Aspect | Paper | Live |
|--------|-------|------|
| Market scanning | Real (Gamma API) | Real (Gamma API) |
| Price data | Real (CLOB API) | Real (CLOB API) |
| Claude analysis | Real (costs real money) | Real (costs real money) |
| Weather data | Real (Open-Meteo API) | Real (Open-Meteo API) |
| Order placement | Simulated (logged to SQLite) | Real (signed via py-clob-client) |
| Bankroll | Virtual (starts at $50) | Real USDC on Polygon |
| API costs | Deducted from virtual bankroll | Deducted from real bankroll |
| Death | Virtual (agent stops) | Real (agent stops, money gone) |

**Paper mode still burns real Claude API credits.** Your Anthropic bill is real
even when trades are simulated.

To switch modes, change `TRADING_MODE` in `.env` and restart the agent.

A 48-hour paper smoke test is recommended before going live.

## Going Live Checklist

1. **Run paper mode for 48 hours.** Verify the agent finds opportunities, sizes
   positions correctly, and manages risk. Check the dashboard for anomalies.

2. **Fund your Polygon wallet.** Buy USDC on an exchange, withdraw to your
   Polygon (chain ID 137) wallet address. Start with $50.

3. **Set your private key.**
   ```env
   POLYMARKET_WALLET_PRIVATE_KEY=0x...
   ```

4. **Switch to live mode.**
   ```env
   TRADING_MODE=live
   ```

5. **Restart the agent.** Verify the dashboard shows `LIVE` mode badge.

6. **Monitor for 24 hours.** Watch the dashboard, check trade sizes are
   reasonable, confirm stop-loss triggers work.

## Monitoring & Troubleshooting

### Log Levels

Control verbosity with `RUST_LOG`:

```env
RUST_LOG=polymarket_agent=info      # Normal operation
RUST_LOG=polymarket_agent=debug     # Detailed cycle logging
RUST_LOG=polymarket_agent=trace     # Everything (very verbose)
```

### SQLite Queries

The database at `data/polymarket-agent.db` stores all state. Useful queries:

```sql
-- Current bankroll
SELECT available_cash FROM bankroll ORDER BY updated_at DESC LIMIT 1;

-- Recent trades
SELECT * FROM trades ORDER BY created_at DESC LIMIT 20;

-- Open positions
SELECT * FROM positions WHERE status = 'open';

-- API cost breakdown (last 24h)
SELECT model, COUNT(*) as calls, SUM(cost_usd) as total_cost
FROM api_costs
WHERE created_at > datetime('now', '-1 day')
GROUP BY model;

-- Cycle history
SELECT cycle_number, markets_scanned, trades_placed, api_cost_usd, bankroll_after
FROM cycle_log ORDER BY cycle_number DESC LIMIT 10;
```

### Common Issues

**Sidecar won't start**
- Check Python venv is activated: `source sidecar/.venv/bin/activate`
- Verify dependencies: `pip install -r sidecar/requirements.txt`
- Check port 9090 isn't in use: `lsof -i :9090`
- The agent continues without the sidecar but can't execute trades or fetch weather data

**No trades happening**
- Check `ANTHROPIC_API_KEY` is set — without it, no analysis runs
- Look at cycle logs: the agent may be scanning markets but finding no edge above 8%
- Lower `MIN_EDGE_THRESHOLD` slightly if all markets are being rejected (but be cautious)
- Check that weather markets are active on Polymarket

**High API burn rate**
- Check `MAX_API_COST_PER_CYCLE` is set to `0.50` (hard cap)
- Verify cycle frequency — should be 30min cycles when bankroll < $200
- Review API cost queries above to see per-model spend

**Dashboard not loading**
- Verify `DASHBOARD_PORT` isn't blocked or in use
- Check that `DASHBOARD_PASSWORD` is set if Basic Auth is expected
- Look for "Dashboard server failed" in logs

## Deployment (VPS)

Minimum spec: 2 CPU, 2GB RAM, 20GB disk (Hetzner CX22, Contabo, or Oracle Cloud
free tier).

### Systemd Unit File

```ini
[Unit]
Description=Polymarket Trading Agent
After=network.target

[Service]
Type=simple
User=polymarket
WorkingDirectory=/opt/polymarket-agent
ExecStart=/opt/polymarket-agent/target/release/polymarket-agent
Restart=on-failure
RestartPreventExitStatus=42
Environment=RUST_LOG=polymarket_agent=info
EnvironmentFile=/opt/polymarket-agent/.env

[Install]
WantedBy=multi-user.target
```

Key detail: `RestartPreventExitStatus=42` ensures systemd does **not** restart
the agent after a bankroll death (exit code 42). Crashes from bugs will still
restart automatically.

```bash
sudo cp polymarket-agent.service /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now polymarket-agent

# Check status
sudo systemctl status polymarket-agent
sudo journalctl -u polymarket-agent -f
```

## Risk & Safety

### Risk Parameters Summary

| Parameter | Default | Effect |
|-----------|---------|--------|
| Max position size | 6% bankroll | Limits single-trade loss |
| Max total exposure | 40% bankroll | Prevents over-commitment |
| Min edge threshold | 8% | Only trades with significant mispricing |
| Half-Kelly sizing | 0.5 | Halves theoretical optimal bet for variance reduction |
| Stop-loss | 15% | Exits losing positions early |
| Take-profit | 90% | Locks in gains near expected value |
| Drawdown breaker | 30% from peak | Halves position sizes during drawdowns |
| Correlation cap | 15% per group | Limits exposure to correlated weather markets |
| API cost cap | $0.50/cycle | Prevents runaway Claude API spending |
| Death exit code | 42 | Graceful shutdown, no systemd restart |

### Before Going Live

- The agent trades with real money. Losses are permanent.
- Claude API costs are real in both paper and live modes.
- The $50 seed can be lost entirely — that is the design.
- Weather markets are the primary strategy. Performance depends on forecast
  skill vs market efficiency.
- The death mechanic is intentional: bankroll hits $0, the agent exits with
  code 42, and systemd does not restart it.

### The Death Mechanic

When `available_cash + liquidation_value <= 0`, the agent:

1. Generates a death report (full trade history, P&L breakdown, cause of death)
2. Logs the report
3. Shuts down the Python sidecar
4. Exits with code 42

The death report shows what went wrong — whether it was bad trades, excessive
API costs, or a combination.
