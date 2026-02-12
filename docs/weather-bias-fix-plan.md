# Plan: Fix Weather Model Bias & Maximize Forecast Edge

## Context

**The Problem:** Raw ensemble output from Open-Meteo (GEFS + ECMWF) has a ~5°F systematic cold bias vs. station observations. NWS forecasts 37°F for NYC; our ensemble's warmest member across ALL 82 members tops out at ~34.9°F. This means every bucket probability we compute is centered on the wrong temperature — we systematically overbuy cold buckets and underbuy warm ones. This is fatal for a strategy built on 2°F precision.

**Two Compounding Issues:**
1. **Mean bias (~5°F cold)** — The distribution center is wrong. Raw NWP model output has no MOS/bias correction. NWS applies decades of statistical post-processing to the same underlying models.
2. **Spread bias (underdispersion)** — The distribution is too narrow. GEFS ensembles are systematically overconfident. Our `WEATHER_SPREAD_CORRECTION` is set to 1.0 (no correction).

**Resolution Source Mismatch:** Polymarket resolves via Weather Underground (WU), not NWS. WU and NWS can differ by 1-2°F due to rounding rules and observation windows. Anchoring on NWS is better than raw ensemble but still introduces a subtler systematic error at the bucket-boundary scale.

**Goal:** Fix the bias in priority order — mean first (most impact), spread second, resolution source third — through a phased approach that delivers immediate improvement then compounds over time.

---

## Phase 1: NWS Anchor + Spread Correction (Quick Fix)

**Why first:** Eliminates the ~5°F cold bias immediately. Goes from "every trade is wrong" to "~1-2°F residual error." NWS API is free, no auth, simple JSON — fastest path to a working model.

### Files to Create
- `sidecar/weather/nws.py` — NWS API client

### Files to Modify
- `sidecar/weather/probability_model.py` — Accept NWS anchor, shift ensemble before KDE
- `sidecar/server.py` — Fetch NWS, pass to probability model, add fields to response
- `src/weather_client.rs` — Add Optional fields to WeatherProbabilities struct
- `src/estimator.rs` — Render NWS reference data in Claude prompt
- `.env.example` — Change `WEATHER_SPREAD_CORRECTION` default from 1.0 to 1.3

### Implementation Details

**sidecar/weather/nws.py** (new ~80 lines):
```
async def fetch_nws_forecast(lat, lon, target_date, session) -> Optional[float]:
    # Step 1: GET https://api.weather.gov/points/{lat},{lon}
    #   → Extract properties.forecast URL
    #   → Cache grid mapping per city (doesn't change)
    # Step 2: GET {forecast_url}
    #   → Parse periods[] for target_date daytime period
    #   → Return temperature (high) as float °F
    # Retry: 3 attempts with 2s backoff
    # Rate limit: 1s delay between requests (NWS requirement)
    # Fallback: Return None on failure (don't block ensemble pipeline)
```

**probability_model.py changes:**
- `compute_bucket_probabilities()` gains new parameter: `nws_anchor: Optional[float] = None`
- Before KDE, if nws_anchor is provided:
  ```python
  raw_mean = np.mean(members)
  shift = nws_anchor - raw_mean  # e.g., 37.0 - 32.0 = +5.0
  shifted = members + shift       # shift ALL members by the bias
  # Then apply spread_correction to shifted members
  # Then run KDE on the result
  ```
- This preserves ensemble spread (the uncertainty structure) while centering on NWS's calibrated point forecast
- Both corrections compose: shift fixes mean, spread_correction fixes width

**server.py changes:**
- In `weather_probabilities()` endpoint, after `fetch_ensemble()`:
  ```python
  nws_high = await fetch_nws_forecast(config.lat, config.lon, date, session)
  probs = compute_bucket_probabilities(forecast, spread_correction=..., nws_anchor=nws_high)
  ```
- Add to WeatherResponse: `nws_forecast_high: Optional[float]`, `bias_correction: Optional[float]`

**weather_client.rs changes:**
- Add to `WeatherProbabilities` struct:
  ```rust
  #[serde(default)]
  pub nws_forecast_high: Option<f64>,
  #[serde(default)]
  pub bias_correction: Option<f64>,
  ```
- `#[serde(default)]` ensures backward compatibility — if sidecar doesn't send these fields, they default to None

**estimator.rs changes:**
- In `render_weather_block()`, conditionally add:
  ```
  - **NWS Official Forecast High:** 37.0°F
  - **Bias Correction Applied:** +5.0°F (ensemble shifted to match NWS)
  ```

**Important: Keep hourly extraction, do NOT switch to daily param.**
The current `_extract_daily_max_for_date()` with timezone handling is proven correct. Open-Meteo's `daily: temperature_2m_max` for per-member ensemble data is unverified and may not handle timezones per city correctly. Don't introduce a new bug while fixing an existing one.

### Tests
- `sidecar/weather/test_nws.py`: Mock NWS API responses, test grid caching, test forecast parsing, test fallback on failure
- Update `test_probability_model.py`: Test shift + spread correction together, test None anchor (no shift), test that shift preserves std dev
- Update `test_server.py`: Test new response fields present

### Verification
1. Run sidecar: `GET /weather/probabilities?city=NYC&date=2026-02-12`
2. Confirm `ensemble_mean` is now close to NWS forecast (~37°F for NYC today)
3. Confirm `nws_forecast_high` field is populated in response
4. Confirm `bias_correction` shows the shift applied (e.g., +5.0)
5. Confirm bucket probabilities are centered around 37°F, not 32°F
6. Run all sidecar tests: `cd sidecar && python -m pytest`
7. Run Rust build: `cargo build`
8. Run Rust tests: `cargo test`

---

## Phase 2: Weather Underground Validation Loop

**Why second:** NWS ≠ WU. Polymarket resolves via WU. We need to quantify the NWS↔WU discrepancy per station before we know if Phase 1's anchor is good enough. This phase builds the data pipeline that all future calibration depends on.

### Files to Create
- `sidecar/weather/wu_scraper.py` — WU historical daily high scraper

### Files to Modify
- `sidecar/server.py` — Add `/weather/validation` endpoint
- SQLite schema — Add `weather_validation` table

### Implementation Details

**wu_scraper.py** (~60 lines):
```
async def fetch_wu_daily_high(icao: str, date: str) -> Optional[float]:
    # Scrape: https://www.wunderground.com/history/daily/us/{state}/{city}/{ICAO}/date/{YYYY-M-D}
    # Parse the "Max Temperature" from the daily summary table
    # Return as float °F (integer from WU, but float for consistency)
    # Cache results (WU data doesn't change after the fact)
    # Rate limit: 2s between requests (be polite, avoid blocks)
```

**New endpoint:** `GET /weather/validation?city=NYC&date=YYYY-MM-DD`
- Returns: `{ wu_actual_high, nws_forecast_high, ensemble_raw_mean, ensemble_corrected_mean, error_vs_wu }`
- Purpose: Logging pipeline for daily tracking

**SQLite table:**
```sql
CREATE TABLE weather_validation (
    city TEXT, date TEXT, wu_actual REAL, nws_forecast REAL,
    ensemble_raw_mean REAL, ensemble_corrected_mean REAL,
    error_vs_wu REAL, -- corrected_mean - wu_actual
    PRIMARY KEY (city, date)
);
```

### Verification
- After running for 7+ days, analyze: `SELECT city, AVG(error_vs_wu), STDEV(error_vs_wu) FROM weather_validation GROUP BY city`
- This tells us if NWS anchor produces systematic WU error per city
- If avg error > 1°F for some cities, we need city-specific correction factors

---

## Phase 3: NBM Integration (The Proper Fix)

**Why third:** NBM is the gold standard — NOAA's own bias-corrected, multi-model blend at 2.5km resolution with probabilistic output. It fixes both mean bias AND spread simultaneously. But it requires GRIB2 parsing (Herbie + cfgrib), which is more complex than the NWS JSON API. Phase 1 buys us time to implement this properly.

### Files to Create
- `sidecar/weather/nbm.py` — NBM fetcher via Herbie

### Files to Modify
- `sidecar/requirements.txt` — Add `herbie-data`, `cfgrib`, `xarray`
- `sidecar/weather/probability_model.py` — Blend NBM percentiles with ensemble KDE
- `sidecar/server.py` — Integrate NBM into main weather endpoint

### Implementation Details

**nbm.py** (~120 lines):
```
def fetch_nbm_percentiles(lat, lon, target_date) -> Optional[dict]:
    # Use Herbie to download latest NBM GRIB2 from AWS S3
    # Product: "blend" model, MaxT variable
    # Extract percentiles (10th, 25th, 50th, 75th, 90th) at nearest grid point
    # Return: { "p10": 33.0, "p25": 35.0, "p50": 37.0, "p75": 39.0, "p90": 41.0 }
    #
    # Herbie usage:
    #   from herbie import Herbie
    #   H = Herbie(date, model="nbm", product="co", fxx=24)
    #   ds = H.xarray(":TMAX:").sel(latitude=lat, longitude=lon, method='nearest')
```

**probability_model.py changes — NBM blending:**
When NBM percentiles are available, replace the simple NWS mean-shift with a quantile-matched approach:
1. Fit a normal distribution to NBM percentiles (mu, sigma from p10/p25/p50/p75/p90)
2. Use NBM's mu as the anchor (replaces NWS point forecast)
3. Use NBM's sigma to set the spread correction factor dynamically:
   `spread_correction = nbm_sigma / raw_ensemble_sigma`
4. Apply both corrections, then run KDE

This is strictly better than Phase 1's NWS anchor because:
- NBM is calibrated against actual station observations (closer to WU than NWS forecasts)
- NBM provides spread information (fixes underdispersion automatically)
- NBM updates hourly (vs NWS forecast 2x/day)

**Graceful degradation:** If NBM fetch fails, fall back to NWS anchor (Phase 1). If NWS also fails, use raw ensemble with spread_correction=1.3.

### Verification
- Compare NBM-anchored vs NWS-anchored predictions against WU actuals from Phase 2 validation table
- NBM should have lower avg error and better-calibrated spread
- Run for 7 days, then compare RMSE

---

## Phase 4: HRRR for Same-Day Markets

**Why fourth:** For markets resolving TODAY, HRRR gives hourly 3km updates while GEFS only updates 4x/day. On market day, the most recent data wins.

### Files to Create
- `sidecar/weather/hrrr.py` — HRRR point forecast fetcher via Herbie

### Files to Modify
- `sidecar/weather/probability_model.py` — Accept HRRR anchor for same-day overrides
- `sidecar/server.py` — Fetch HRRR when target_date is today

### Implementation Details

**hrrr.py** (~80 lines):
```
def fetch_hrrr_temperature(lat, lon) -> Optional[float]:
    # Fetch latest HRRR run from AWS S3 via Herbie
    # Extract 2m temperature at airport coordinates
    # Return forecast high for remaining day hours
    # Herbie: H = Herbie(datetime.utcnow(), model="hrrr", fxx=1-18)
```

**Same-day logic in server.py:**
- If `target_date == today` AND HRRR available:
  - Use HRRR as primary anchor (freshest data)
  - Use NBM spread (calibrated uncertainty)
  - Fallback chain: HRRR → NBM → NWS → raw ensemble
- If `target_date > today`:
  - Use NBM anchor + ensemble spread (Phase 3 approach)

### Verification
- On a market day, compare HRRR-anchored prediction vs. NBM-anchored vs. WU actual
- HRRR should be closest for same-day, NBM better for multi-day

---

## Phase 5: Multi-Model Expansion (82 → 143+ members)

**Why fifth:** Quick win that improves tail probability estimates. More members = smoother KDE = better edge detection on extreme buckets (where the NO-side profit lives).

### Files to Modify
- `sidecar/weather/open_meteo.py` — Add Canadian GEM + DWD ICON models

### Implementation Details
- Add two more model fetches in `fetch_ensemble()`:
  - `"gem_global"` → Canadian GEM, 21 members
  - `"icon_seamless"` → DWD ICON-EPS, 40 members
- Update `EnsembleForecast` to track per-model counts
- Combined: 31 + 51 + 21 + 40 = **143 members**
- Update `WeatherProbabilities` to report all model counts

**Rust side:** Add `#[serde(default)]` fields for `gem_count: Option<u32>`, `icon_count: Option<u32>`

### Verification
- Confirm 143 members in response
- Compare KDE smoothness: 82 vs 143 members (tail probabilities should be more stable)

---

## Phase 6: Automated Calibration Pipeline

**Why last:** Requires data from Phases 1-5 running in production for 30+ days. This is the compound-interest phase — small improvements per city per season that accumulate into a durable edge.

### Files to Create
- `sidecar/weather/calibration.py` — Nightly calibration job

### Implementation Details
- Nightly cron job that:
  1. Fetches WU actuals for all cities for yesterday
  2. Compares against stored predictions (from `weather_validation` table)
  3. Computes per-city, per-lead-time bias and spread error
  4. Updates calibration parameters in SQLite:
     ```sql
     CREATE TABLE weather_calibration (
         city TEXT, lead_days INT,
         mean_bias REAL,      -- avg(predicted - actual)
         spread_factor REAL,  -- optimal spread correction for this city
         updated_at TEXT,
         PRIMARY KEY (city, lead_days)
     );
     ```
  5. `compute_bucket_probabilities()` reads per-city calibration from DB instead of using global `WEATHER_SPREAD_CORRECTION`

### Verification
- After 30 days of data: per-city RMSE should decrease as calibration parameters converge
- Compare calibrated vs. uncalibrated predictions on held-out days

---

## Summary: Error Reduction Path

| Phase | Anchor | Est. Error vs WU | Effort |
|-------|--------|-------------------|--------|
| **Current** | None (raw ensemble) | ~5°F bias | — |
| **Phase 1** | NWS forecast | ~1-2°F | 1-2 days |
| **Phase 2** | NWS + WU validation | ~1-2°F (now measured) | 0.5-1 day |
| **Phase 3** | NBM percentiles | ~0.5-1°F | 2-3 days |
| **Phase 4** | HRRR (same-day) | ~0.5°F same-day | 1-2 days |
| **Phase 5** | 143 members | Better tails | 0.5 day |
| **Phase 6** | Per-city calibration | <0.5°F | 2-3 days |

---

## Critical Design Decisions

1. **Keep hourly extraction, don't switch to daily param.** The current `_extract_daily_max_for_date()` with timezone handling is proven. Open-Meteo's `daily: temperature_2m_max` for per-member ensemble data is unverified.

2. **NWS anchor is a stepping stone, not the destination.** NWS ≠ WU. Phase 3 (NBM) replaces it with something better. Phase 1 just stops the bleeding.

3. **All new sidecar response fields use `#[serde(default)]` in Rust.** This makes the Rust side forward-compatible — old sidecar responses still parse fine.

4. **Fallback chain: HRRR → NBM → NWS → raw ensemble.** Each source degrades gracefully. The agent never fails to produce probabilities — just less accurate ones.

5. **Spread correction AND mean correction are independent.** They compose: shift fixes center, spread_correction fixes width. Both are needed.

6. **Don't touch the Rust weather_client HTTP/caching logic.** It works. Only add Optional fields to the deserialization struct. The sidecar does all the heavy lifting.

---

## Execution Order

Start with Phase 1 (immediate bias fix), then Phase 2 (validation data), then proceed through 3-6 as each phase stabilizes. Phases 1-2 should be committed together. Phase 3 can follow once GRIB dependencies are resolved. Phases 4-5 are independent and can be parallelized. Phase 6 requires 30+ days of production data.

---

## Current Status

### Phase 1: COMPLETE (code done, tests passing)
- `sidecar/weather/nws.py` — Created (NWS API client with grid caching, retry, fallback)
- `sidecar/weather/probability_model.py` — Modified (nws_anchor param, mean-shift before KDE, new fields)
- `sidecar/server.py` — Modified (fetches NWS, passes anchor to probability model, new response fields)
- `src/weather_client.rs` — Modified (Optional nws_forecast_high + bias_correction with #[serde(default)])
- `src/estimator.rs` — Modified (renders NWS data in Claude prompt)
- `.env.example` — Modified (WEATHER_SPREAD_CORRECTION default 1.0 → 1.3)
- `sidecar/weather/test_nws.py` — Created (7 tests, all passing)
- `sidecar/weather/test_probability_model.py` — Modified (6 new NWS anchor tests, all passing)
- Test results: 22/22 Python tests pass, 177/177 Rust tests pass, clippy clean, fmt clean
