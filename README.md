# Polymarket Autonomous Trading Agent

An autonomous trading agent for [Polymarket](https://polymarket.com) prediction markets.
Starts with $50 USDC, pays its own Claude API costs from profits, and shuts down if it
runs out of money.

## Architecture

**Rust core** (scheduler, analysis, risk management, dashboard) + **Python sidecar**
(FastAPI on localhost:9090 for Polymarket order signing via `py-clob-client` and weather
data processing).

## Primary Strategy

Weather temperature markets using NOAA GEFS (31 members) and ECMWF (51 members) ensemble
forecasts to identify mispriced outcomes. The agent buys NO shares on overpriced tail
outcomes where 82 ensemble members agree the probability is near zero.

## Quick Start

```bash
# 1. Clone and configure
cp .env.example .env
# Edit .env with your private key and Anthropic API key

# 2. Install Python dependencies
cd sidecar && pip install -r requirements.txt && cd ..

# 3. Build and run
cargo build --release
./target/release/polymarket-agent
```

## Development with Claude Code

This project is designed to be built phase-by-phase with [Claude Code](https://code.claude.com).

```bash
# Install Claude Code
curl -fsSL https://code.claude.com/install.sh | sh

# Start development
cd polymarket-agent
claude

# First session:
> Read CLAUDE.md and docs/. Understand the full project architecture.
  Then execute Phase 1.
```

See `CLAUDE.md` for the full specification and build phases.

## Build Progress

| Phase | Description | Status |
|-------|-------------|--------|
| 1 | Skeleton, Market Scanner & Sidecar | Done |
| 2 | Claude Analysis Engine & Edge Detection | Done |
| 3 | Position Sizing & Execution | Done |
| 4 | Accounting & Survival | Done |
| 5 | Weather Pipeline (Highest Edge) | Done |
| 6 | Position Management & Risk | Done |
| 7 | Web Dashboard | Next |

## What Works Today

- **Market scanning**: Paginated Gamma API discovery with liquidity/category filters
- **Claude analysis**: Haiku triage → Sonnet deep analysis pipeline with structured JSON output
- **Edge detection**: Fair value vs market price comparison, configurable thresholds
- **Position sizing**: Half-Kelly criterion with 6% bankroll cap and exposure limits
- **Trade execution**: Paper mode (simulated) and live mode (via sidecar → py-clob-client)
- **Weather pipeline**: 82 ensemble members (GEFS+ECMWF) → KDE probability buckets for 20 US cities
- **Position management**: Stop-loss (15%), take-profit (90%), edge decay, drawdown circuit breaker
- **Risk controls**: Correlation groups for weather markets, max exposure limits, adaptive cycle timing
- **Survival mechanic**: Real-time bankroll ledger, API cost tracking, graceful death on $0

## Project Structure

```
├── CLAUDE.md                 # Full spec (Claude Code reads this automatically)
├── docs/
│   ├── weather-research.md   # Weather data feasibility research
│   ├── api-reference.md      # Polymarket API endpoints + examples
│   └── claude-code-workflow.md
├── src/                      # Rust core
│   ├── main.rs               # Entry point, scheduler, lifecycle
│   ├── config.rs             # All configurable params from .env
│   ├── market_scanner.rs     # Gamma API client (market discovery)
│   ├── estimator.rs          # Claude API integration (analysis brain)
│   ├── edge_detector.rs      # Fair value vs market price comparison
│   ├── position_sizer.rs     # Kelly Criterion with bankroll cap
│   ├── executor.rs           # Order execution + position exits
│   ├── position_manager.rs   # Stop-loss, take-profit, drawdown
│   ├── accounting.rs         # Bankroll, API costs, survival check
│   ├── weather_client.rs     # Sidecar weather endpoint client
│   ├── clob_client.rs        # CLOB API orderbook/prices
│   └── data_sources/         # OpenClaw stub + future integrations
├── sidecar/                  # Python FastAPI sidecar
│   ├── server.py             # FastAPI app on localhost:9090
│   ├── polymarket_client.py  # py-clob-client wrapper
│   └── weather/              # Open-Meteo ensemble → probabilities
├── prompts/                  # Claude prompt templates
├── tasks/                    # Lessons learned + phase tracking
└── .claude/                  # Claude Code agents + commands
```

## License

Private. Not for redistribution.
