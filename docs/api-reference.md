# Polymarket API Reference

Quick reference for all Polymarket APIs, authentication, and `py-clob-client` code
examples. This is the reference material Claude Code should consult when building
the market scanner (Rust/Gamma API) and the Python sidecar (py-clob-client/CLOB API).

---

## API Architecture Overview

Polymarket is NOT a single API. It is **three services + a WebSocket**:

| Service | Base URL | Auth | Purpose |
|---------|----------|------|---------|
| **Gamma API** | `https://gamma-api.polymarket.com` | None | Market discovery, metadata, tags, events |
| **CLOB API** | `https://clob.polymarket.com` | EIP-712 + L2 HMAC | Orderbook, prices, order placement |
| **Data API** | `https://data-api.polymarket.com` | API key | User positions, trade history, P&L |
| **WebSocket** | `wss://ws-subscriptions-clob.polymarket.com` | Subscribe by channel | Real-time price/order updates |

---

## 1. Gamma API (Market Discovery)

No authentication required. Used by the Rust `market_scanner.rs` module.

### GET /events — List Events (Recommended for Discovery)
Events contain their associated markets. Paginate through events to get all markets.

```bash
# Page 1: newest 50 active events
curl "https://gamma-api.polymarket.com/events?order=id&ascending=false&closed=false&limit=50&offset=0"

# Page 2
curl "https://gamma-api.polymarket.com/events?order=id&ascending=false&closed=false&limit=50&offset=50"
```

**Query Parameters:**
- `limit` — Results per page (max 50)
- `offset` — Pagination offset
- `order` — Sort field (e.g., `id`, `volume`)
- `ascending` — `true` or `false`
- `closed` — `false` for active only
- `tag_id` — Filter by category tag

### GET /markets — List Markets
Direct market listing with filtering.

```bash
# Active markets with tag filtering
curl "https://gamma-api.polymarket.com/markets?closed=false&limit=50&offset=0"

# Filter by tag (e.g., weather)
curl "https://gamma-api.polymarket.com/markets?tag_id=84&closed=false&limit=25&offset=0"
```

**Key response fields per market:**
```json
{
  "id": 12345,
  "question": "Highest temperature in NYC on Feb 10?",
  "slug": "highest-temp-nyc-feb-10",
  "conditionId": "0x...",
  "tokens": [
    { "token_id": "abc123...", "outcome": "Yes", "price": 0.65 },
    { "token_id": "def456...", "outcome": "No", "price": 0.35 }
  ],
  "volume": 150000.0,
  "liquidity": 25000.0,
  "endDate": "2026-02-11T00:00:00Z",
  "closed": false,
  "active": true,
  "tags": [{ "id": 84, "label": "Weather" }]
}
```

**IMPORTANT:** The `token_id` from Gamma is what you pass to CLOB API for prices and orders.

### GET /events/{slug} — Single Event by Slug
```bash
curl "https://gamma-api.polymarket.com/events/highest-temp-nyc-feb-10"
```

### GET /tags — List Available Tags
```bash
curl "https://gamma-api.polymarket.com/tags"
```

### Best Practices
- Always include `closed=false` unless you need historical data
- Use `/events` endpoint and extract markets — more efficient than `/markets` for bulk discovery
- Paginate with `limit=50&offset=N` (max 50 per page)
- Cache tag IDs for weather, sports, crypto, politics categories

---

## 2. CLOB API (Trading)

### Authentication

**Two layers:**
1. **L1 Auth (EIP-712):** Signs a challenge with your Polygon private key to prove wallet ownership.
   Used to create/derive API credentials.
2. **L2 Auth (HMAC-SHA256):** Uses derived API key/secret/passphrase for request signing.
   Requests expire after 30 seconds.

**py-clob-client handles both automatically.** You just provide the private key.

### Read Endpoints (No Auth Required)

```bash
# Health check
curl "https://clob.polymarket.com/"

# Server time
curl "https://clob.polymarket.com/time"

# Get midpoint price
curl "https://clob.polymarket.com/midpoint?token_id=<TOKEN_ID>"

# Get best bid/ask price
curl "https://clob.polymarket.com/price?token_id=<TOKEN_ID>&side=BUY"
curl "https://clob.polymarket.com/price?token_id=<TOKEN_ID>&side=SELL"

# Get orderbook
curl "https://clob.polymarket.com/book?token_id=<TOKEN_ID>"

# Get spread
curl "https://clob.polymarket.com/spread?token_id=<TOKEN_ID>"
```

### Write Endpoints (Auth Required — Use py-clob-client)

```
POST /order          — Place single order (requires L2 header)
POST /orders         — Place batch orders (up to 15 per call)
DELETE /order/{id}   — Cancel order
DELETE /orders       — Cancel all orders
```

### Order Types
- **GTC (Good Til Cancelled):** Default limit order, sits on book
- **FOK (Fill Or Kill):** Market order, fill entirely or cancel
- **FAK (Fill And Kill):** Market order, fill what's available, cancel rest
- **postOnly:** Limit order that rejects if it would cross the spread (prevents unintended market orders)

### Order Status Values
After placement, orders return a status:
- `matched` — Fully filled immediately
- `delayed` — In queue, likely to match soon
- `live` — Resting on the book
- `unmatched` — No match found (for market orders = rejected)

---

## 3. Data API (Portfolio)

Auth required for user-specific data.

```bash
# User positions (requires auth)
GET https://data-api.polymarket.com/positions?user=<ADDRESS>

# Trade history
GET https://data-api.polymarket.com/trades?user=<ADDRESS>

# Resolved markets
GET https://data-api.polymarket.com/resolved?user=<ADDRESS>
```

---

## 4. WebSocket (Real-Time Updates)

```
wss://ws-subscriptions-clob.polymarket.com/ws/market
wss://ws-subscriptions-clob.polymarket.com/ws/user
```

**Subscribe to market price updates:**
```json
{
  "type": "subscribe",
  "channel": "market",
  "assets_id": "<TOKEN_ID>"
}
```

**Subscribe to user order updates (requires auth):**
```json
{
  "type": "subscribe",
  "channel": "user",
  "auth": { ... }
}
```

---

## 5. py-clob-client Reference (Python Sidecar)

Install: `pip install py-clob-client`
Latest version: v0.29.0 (Dec 2025) with HTTP2 and Keep-Alive.
Requires Python 3.9+.

### Client Initialization

```python
import os
from py_clob_client.client import ClobClient
from dotenv import load_dotenv

load_dotenv()

HOST = "https://clob.polymarket.com"
CHAIN_ID = 137  # Polygon mainnet

# For EOA wallet (simplest — recommended for bot)
client = ClobClient(
    HOST,
    key=os.getenv("POLYMARKET_WALLET_PRIVATE_KEY"),
    chain_id=CHAIN_ID,
    signature_type=0,  # 0 = standard EOA
)

# For email/Magic proxy wallet
# client = ClobClient(
#     HOST,
#     key=os.getenv("POLYMARKET_WALLET_PRIVATE_KEY"),
#     chain_id=CHAIN_ID,
#     signature_type=1,  # 1 = POLY_PROXY (email/Magic)
#     funder=os.getenv("POLYMARKET_FUNDER_ADDRESS"),
# )

# Derive API credentials (do this once, they're deterministic)
client.set_api_creds(client.create_or_derive_api_creds())
```

### Read Operations (No Auth)

```python
from py_clob_client.clob_types import BookParams

# Health check
ok = client.get_ok()
server_time = client.get_server_time()

# Get prices for a token
token_id = "<token-id>"  # from Gamma API
midpoint = client.get_midpoint(token_id)
buy_price = client.get_price(token_id, side="BUY")
sell_price = client.get_price(token_id, side="SELL")

# Get full orderbook
order_book = client.get_order_book(token_id)

# Batch orderbooks
books = client.get_order_books([
    BookParams(token_id="token1"),
    BookParams(token_id="token2"),
])
```

### Place Limit Order (postOnly)

```python
from py_clob_client.clob_types import OrderArgs, OrderType
from py_clob_client.order_builder.constants import BUY, SELL

# Buy 100 YES tokens at $0.50 each
order_args = OrderArgs(
    price=0.50,
    size=100.0,
    side=BUY,
    token_id="<token-id>",
)
signed_order = client.create_order(order_args)
resp = client.post_order(signed_order, orderType=OrderType.GTC)
print(resp)
# Response includes: { "orderID": "0x...", "status": "live" }

# Buy NO tokens (sell YES equivalent) — for weather NO-side trades
# To buy NO, you BUY the NO token_id (each market has YES and NO token IDs)
no_order_args = OrderArgs(
    price=0.98,       # Buy NO at 98¢
    size=50.0,        # 50 NO shares
    side=BUY,
    token_id="<NO-token-id>",  # The NO token from Gamma API
)
signed_no = client.create_order(no_order_args)
resp = client.post_order(signed_no, orderType=OrderType.GTC)
```

### Place Market Order (FOK)

```python
from py_clob_client.clob_types import MarketOrderArgs, OrderType
from py_clob_client.order_builder.constants import BUY

mo = MarketOrderArgs(
    token_id="<token-id>",
    amount=25.0,       # $25 spend
    side=BUY,
    order_type=OrderType.FOK,
)
signed = client.create_market_order(mo)
resp = client.post_order(signed, OrderType.FOK)
```

### Cancel Orders

```python
# Cancel single order
client.cancel(order_id="0x...")

# Cancel all orders
client.cancel_all()

# Cancel all orders for a specific market
client.cancel_market_orders(market="<condition-id>")
```

### Get Open Orders

```python
from py_clob_client.clob_types import OpenOrderParams

orders = client.get_orders(
    OpenOrderParams(market="<condition-id>")
)
```

### Token Allowances (Required Before First Trade)

EOA wallets must set token allowances before trading. Do this once:

```python
# Set USDC allowance (for buying)
client.set_allowances()

# Or explicitly:
# 1. Collateral (USDC) allowance for the Exchange contract
# 2. Conditional token allowance for selling
```

### Batch Orders (Up to 15)

```python
# Place multiple orders in one call
orders = []
for city_market in weather_markets:
    order_args = OrderArgs(
        price=0.98,
        size=10.0,
        side=BUY,
        token_id=city_market["no_token_id"],
    )
    signed = client.create_order(order_args)
    orders.append(signed)

# Post batch (max 15 per call)
resp = client.post_orders(orders, orderType=OrderType.GTC)
```

---

## 6. Open-Meteo Ensemble API (Weather Sidecar)

No auth required for free tier. Used by `sidecar/weather/open_meteo.py`.

### Fetch GEFS + ECMWF Ensemble Members

```python
import requests

def fetch_ensemble(lat: float, lon: float, models: str = "gfs025_ens,ecmwf_ifs025_ens"):
    """Fetch all ensemble members from Open-Meteo."""
    resp = requests.get(
        "https://ensemble-api.open-meteo.com/v1/ensemble",
        params={
            "latitude": lat,
            "longitude": lon,
            "hourly": "temperature_2m",
            "models": models,
            "temperature_unit": "fahrenheit",
            "timezone": "America/New_York",  # Adjust per city
            "forecast_days": 7,
        },
        timeout=30,
    )
    resp.raise_for_status()
    return resp.json()
```

### Response Structure

```json
{
  "hourly": {
    "time": ["2026-02-10T00:00", "2026-02-10T01:00", ...],
    "temperature_2m_member00": [32.1, 31.8, ...],
    "temperature_2m_member01": [33.0, 32.5, ...],
    ...
    "temperature_2m_member30": [31.5, 30.9, ...]
  }
}
```

Each model returns members numbered `member00` through `memberNN`.
GEFS: 31 members (00-30). ECMWF: 51 members (00-50).

### Extract Daily Max Temperatures per Member

```python
import numpy as np
from datetime import date

def extract_daily_max(data: dict, target_date: date) -> np.ndarray:
    """Extract max temperature for target_date from each ensemble member."""
    times = data["hourly"]["time"]
    target_str = target_date.isoformat()

    # Find indices for target date
    indices = [i for i, t in enumerate(times) if t.startswith(target_str)]

    member_maxes = []
    for key, values in data["hourly"].items():
        if key.startswith("temperature_2m_member"):
            day_temps = [values[i] for i in indices if values[i] is not None]
            if day_temps:
                member_maxes.append(max(day_temps))

    return np.array(member_maxes)
```

### Rate Limits
- Free: 10,000 calls/day, 600/minute
- Our usage: ~160 calls/day (1.6% of quota)
- Commercial use requires $99/month plan or self-hosting

---

## 7. Key Identifiers Glossary

| Identifier | Source | Example | Used For |
|-----------|--------|---------|----------|
| `event_id` | Gamma | `16085` | Group related markets |
| `slug` | Gamma | `highest-temp-nyc-feb-10` | Human-readable market key |
| `condition_id` | Gamma + CLOB | `0x9915bea2...` | Unique market identifier |
| `token_id` | Gamma | `abc123...` (long hex) | Specific outcome (YES or NO) — used for CLOB prices/orders |
| `order_id` | CLOB | `0xb816482a...` | Track individual orders |

**Critical flow:** Gamma API gives you `token_id` → you use `token_id` with CLOB API for prices and orders.

---

## 8. Common Gotchas

1. **token_id vs condition_id:** Orders use `token_id` (specific YES/NO outcome), not `condition_id` (the market). Each market has TWO token_ids.

2. **Price semantics:** Price 0.65 on a YES token means the market thinks 65% probability. Buying NO at 0.35 is equivalent. To buy NO, use the NO token_id with side=BUY.

3. **USDC decimals:** Polymarket uses 6 decimal USDC (not 18). Amounts are in human-readable dollars (the client handles conversion).

4. **Signature types:**
   - `0` = Standard EOA (Metamask, raw private key)
   - `1` = POLY_PROXY (email/Magic wallet)
   - `2` = POLY_GNOSIS_SAFE (browser wallet proxy)
   For a trading bot, use type `0` with a dedicated EOA wallet.

5. **postOnly orders:** Always use `postOnly=True` for limit orders to avoid accidentally crossing the spread and getting worse execution.

6. **Batch limit:** Max 15 orders per batch call. For 20 cities × 2 directions, you need 2-3 batch calls.

7. **Balance tracking:** Your balance on CLOB is per-market. If you have $500 and place a $500 BUY on Market A, you cannot place any more BUY orders on Market A (but can on Market B). Track available balance per market.

8. **web3 version pinning:** `pip install web3==6.14.0` to avoid dependency conflicts with `eth-typing` in `py-clob-client`.

---

## 9. Rust HTTP Client Notes (for market_scanner.rs)

The Gamma API is called from Rust. Use `reqwest` with these patterns:

```rust
use reqwest::Client;
use serde::Deserialize;

#[derive(Deserialize)]
struct GammaMarket {
    id: u64,
    question: String,
    slug: String,
    condition_id: String,
    tokens: Vec<Token>,
    volume: f64,
    liquidity: f64,
    end_date: Option<String>,
    closed: bool,
    active: bool,
}

#[derive(Deserialize)]
struct Token {
    token_id: String,
    outcome: String,
    price: f64,
}

async fn fetch_markets(client: &Client, offset: u32) -> Result<Vec<GammaMarket>, reqwest::Error> {
    let resp = client.get("https://gamma-api.polymarket.com/markets")
        .query(&[
            ("closed", "false"),
            ("limit", "50"),
            ("offset", &offset.to_string()),
        ])
        .send()
        .await?
        .json::<Vec<GammaMarket>>()
        .await?;
    Ok(resp)
}

// Paginate all active markets
async fn scan_all_markets(client: &Client) -> Vec<GammaMarket> {
    let mut all = Vec::new();
    let mut offset = 0;
    loop {
        let page = fetch_markets(client, offset).await.unwrap_or_default();
        if page.is_empty() { break; }
        offset += page.len() as u32;
        all.extend(page);
        if page.len() < 50 { break; }
    }
    all
}
```

For CLOB read endpoints (prices, orderbook), also call from Rust:

```rust
#[derive(Deserialize)]
struct OrderBook {
    bids: Vec<OrderLevel>,
    asks: Vec<OrderLevel>,
}

#[derive(Deserialize)]
struct OrderLevel {
    price: String,
    size: String,
}

async fn get_midpoint(client: &Client, token_id: &str) -> Result<f64, reqwest::Error> {
    let resp: serde_json::Value = client
        .get(format!("https://clob.polymarket.com/midpoint?token_id={}", token_id))
        .send()
        .await?
        .json()
        .await?;
    Ok(resp["mid"].as_str().unwrap_or("0").parse().unwrap_or(0.0))
}
```

---

## 10. Sidecar API Endpoints (Our Python FastAPI)

These are the endpoints the Rust core calls on `localhost:9090`:

```
GET  /health                          → { "status": "ok" }
POST /order                           → Place single signed order
POST /orders                          → Place batch orders (max 15)
GET  /positions                       → Get all open positions
GET  /balance                         → Get USDC balance
POST /cancel                          → Cancel order by ID
POST /cancel-all                      → Cancel all open orders
GET  /weather/probabilities           → Get temperature bucket probabilities
     ?city=NYC&date=2026-02-11
     → { "buckets": { "32-34": 0.02, "34-36": 0.15, ... }, "members": 82 }
```
