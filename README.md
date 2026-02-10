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

## Project Structure

```
├── CLAUDE.md                 # Full spec (Claude Code reads this automatically)
├── docs/
│   ├── weather-research.md   # Weather data feasibility research
│   └── api-reference.md      # Polymarket API endpoints + examples
├── src/                      # Rust core
├── sidecar/                  # Python FastAPI sidecar
├── prompts/                  # Claude prompt templates
├── static/                   # Dashboard HTML
└── .claude/                  # Claude Code agents + commands
```

## License

Private. Not for redistribution.
