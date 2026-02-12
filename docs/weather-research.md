# Weather Data Feasibility Research

## Summary

All major ensemble forecast systems are completely free for individual developers.
The fastest path uses **Open-Meteo's Ensemble API** which serves GEFS (31 members) and
ECMWF IFS (51 members) as simple JSON — no GRIB2 parsing required. This gives us 82
ensemble members for probability estimation per city at zero data cost.

---

## Free Ensemble Forecast Sources

### NOAA GEFS (Global Ensemble Forecast System)
- **Members:** 31 (1 control + 30 perturbation)
- **Resolution:** 0.5° (also 0.25° for first 10 days)
- **Update frequency:** Every 6 hours (00Z, 06Z, 12Z, 18Z)
- **Forecast horizon:** 16 days (35 days on 00Z run)
- **Access:** Completely free, no auth, US government public domain
- **Direct access:** NOMADS `https://nomads.ncep.noaa.gov/pub/data/nccf/com/gens/prod/`
- **AWS S3:** `s3://noaa-gefs-pds/` (no rate limits, parallel downloads, SNS notifications)
- **NOMADS rate limit:** 10-second minimum between grib filter requests, aggressive scraping = IP block
- **File pattern:** `gefs.YYYYMMDD/HH/atmos/pgrb2ap5/gepNN.tCCz.pgrb2a.0p50.fXXX`
  - `CC` = cycle hour, `NN` = member 01-30, `XXX` = forecast hour
  - Control member uses `gec00` prefix instead of `gepNN`

### ECMWF IFS Open Data
- **Members:** 51 (50 perturbed + 1 control)
- **Resolution:** 0.25°
- **Forecast horizon:** 15 days
- **License:** CC-BY-4.0 since October 2025 (commercial use explicitly permitted)
- **Access:** No registration required
- **Direct:** `https://data.ecmwf.int/forecasts/`
- **AWS:** `s3://ecmwf-forecasts/`
- **Python client:** `pip install ecmwf-opendata`
- **Variables include:** 2m temperature, min/max temp, precipitation, wind, pressure

### NOAA National Blend of Models (NBM)
- **What:** Fuses GEFS, GFS, ECMWF, Canadian, and other models
- **Resolution:** 2.5 km over CONUS (much better than raw GEFS at 55km)
- **Output:** Pre-calibrated probabilistic forecasts with percentiles and exceedance probabilities
- **Update:** Hourly, forecasts through 264 hours
- **AWS:** `s3://noaa-nbm-grib2-pds/`
- **Why it matters:** Already calibrated against station observations. Best free source for
  point-scale station-like temperature predictions.

### Canadian GEPS
- **Members:** 21
- **Resolution:** 0.5°
- **Horizon:** 16 days
- **Update:** Every 12 hours
- **Access:** MSC Datamart (free)

---

## Open-Meteo Ensemble API (THE KEY SHORTCUT)

Open-Meteo serves ALL ensemble members from multiple models as simple JSON REST API.
This eliminates GRIB2 parsing entirely for the MVP.

### Endpoint
```
GET https://ensemble-api.open-meteo.com/v1/ensemble
  ?latitude=40.71
  &longitude=-74.01
  &hourly=temperature_2m
  &models=gfs025_ens,ecmwf_ifs025_ens
  &temperature_unit=fahrenheit
```

### Response Structure
Returns hourly temperature for every member:
- `temperature_2m_member00` through `temperature_2m_member30` (GEFS, 31 members)
- `temperature_2m_member00` through `temperature_2m_member50` (ECMWF, 51 members)
- Combined: **82 ensemble members** per city

### Rate Limits
- Free tier: **10,000 API calls/day**, 600/minute
- Our usage: 20 cities × 2 models × 4 updates/day = 160 calls = **1.6% of daily quota**

### Available Models via Open-Meteo
- NOAA GEFS (31 members) — `gfs025_ens`
- ECMWF IFS (51 members) — `ecmwf_ifs025_ens`
- DWD ICON-EPS (40 members)
- Canadian GEM (21 members)
- UK Met Office (18 members)
- Australia BOM (18 members)

### Commercial Use Caveat
**Free tier = non-commercial use only.** A trading bot generating revenue = commercial.
Options:
1. **Professional plan: $99/month** — still very cheap for the value
2. **Self-host Open-Meteo via Docker** — open source (AGPLv3), unlimited access
3. **Use AWS S3 direct access** for GEFS/ECMWF (free, no restrictions) with Herbie library

### Recommendation for This Project
- **MVP (Tier 1):** Open-Meteo API. Fast, simple, good enough to validate the strategy.
  Either pay $99/month or self-host.
- **Production (Tier 2):** Direct GEFS + ECMWF download from AWS S3 via Herbie library.
  Zero cost, no rate limits, minimum latency. Requires GRIB2 parsing (cfgrib + xarray).
- **Both tiers** should also check NBM for calibrated baseline.

---

## Probability Model

### Converting Ensemble Members to Bucket Probabilities

For each city and forecast date, we have 82 temperature values (31 GEFS + 51 ECMWF).

**Method: Gaussian Kernel Density Estimation (KDE)**
```python
from scipy import stats
import numpy as np

# member_temps = array of 82 temperature values in °F
kde = stats.gaussian_kde(member_temps, bw_method='silverman')

# Integrate over each 2°F bucket
buckets = range(20, 110, 2)  # 20°F to 110°F in 2°F steps
bucket_probs = {}
for b in buckets:
    prob = kde.integrate_box_1d(b, b + 2)
    bucket_probs[f"{b}-{b+2}"] = float(prob)
```

### Critical Calibration Notes

1. **Raw GEFS ensembles are systematically underdispersive** — spread is too narrow.
   Apply spread correction factor of 1.2-1.5× based on historical verification.
   NBM can serve as ground truth for calibration.

2. **Daily max temperature** on Polymarket = highest recorded at any point during the
   calendar day in local time. Must extract multiple forecast hours spanning the full
   local day (typically 12Z to 12Z UTC for eastern cities) and take the maximum.

3. **UTC-to-local conversion errors** are the #1 source of systematic bias.
   Off-by-one errors shift the distribution by several degrees.

---

## Polymarket Weather Market Details

### Resolution Source
Weather markets resolve using **Weather Underground historical data** for specific
airport METAR stations. The station determines the "ground truth."

### Key Airport Stations (Must Use Exact Coordinates)
| City | Station | ICAO | Latitude | Longitude |
|------|---------|------|----------|-----------|
| NYC | LaGuardia | KLGA | 40.7772 | -73.8726 |
| Chicago | O'Hare | KORD | 41.9742 | -87.9073 |
| Miami | Miami Intl | KMIA | 25.7959 | -80.2870 |
| Dallas | DFW | KDFW | 32.8998 | -97.0403 |
| Atlanta | Hartsfield | KATL | 33.6407 | -84.4277 |
| Houston | IAH | KIAH | 29.9844 | -95.3414 |
| Phoenix | Sky Harbor | KPHX | 33.4373 | -112.0078 |
| Philadelphia | PHL | KPHL | 39.8721 | -75.2411 |
| San Diego | SAN | KSAN | 32.7336 | -117.1897 |
| Austin | Bergstrom | KAUS | 30.1945 | -97.6699 |
| Jacksonville | JAX | KJAX | 30.4941 | -81.6879 |
| San Francisco | SFO | KSFO | 37.6213 | -122.3790 |
| Columbus | CMH | KCMH | 39.9980 | -82.8919 |
| Indianapolis | IND | KIND | 39.7173 | -86.2944 |
| Charlotte | CLT | KCLT | 35.2140 | -80.9431 |
| Seattle | SeaTac | KSEA | 47.4502 | -122.3088 |
| Denver | DIA | KDEN | 39.8561 | -104.6737 |
| Boston | Logan | KBOS | 42.3656 | -71.0096 |
| Nashville | BNA | KBNA | 36.1245 | -86.6782 |
| OKC | Will Rogers | KOKC | 35.3931 | -97.6007 |
| Portland | PDX | KPDX | 45.5898 | -122.5951 |
| LA | LAX | KLAX | 33.9425 | -118.4081 |

### Market Format
- Question: "Highest temperature in [City] on [Date]?"
- Outcomes: 2°F buckets (e.g., "40-42°F", "42-44°F", "44-46°F", etc.)
- Volume: $28K–$236K per market daily
- ~61 active weather markets at any time
- Total weather vertical: $3.6M+ volume

### Successful Weather Traders
- "Hans323": $1.11M profit on a single weather bet, 2,373 total predictions
- "neobrother": $20K+ using "temperature laddering" — buying YES across adjacent buckets
- These traders likely use ensemble model data. The tooling is now mature enough for
  individual developers to compete.

---

## Implementation Tiers

### Tier 1 — Weekend Prototype (for this project's Phase 5)
- Open-Meteo Ensemble API for GEFS + ECMWF
- Gaussian KDE over 82 combined ensemble members
- Python sidecar endpoint: `GET /weather/probabilities?city=NYC&date=2026-02-11`
- Dependencies: `requests`, `numpy`, `scipy`
- Cost: $0 (or $99/month for commercial Open-Meteo license)

### Tier 2 — Production Upgrade (after proving profitability)
- Direct GEFS/ECMWF download from AWS S3 via Herbie library
- Add NBM data for calibrated baseline probabilities
- Historical bias correction using ERA5 reanalysis
- Self-hosted Open-Meteo via Docker for unlimited access
- Dependencies add: `herbie-data`, `cfgrib`, `xarray`, `ecmwf-opendata`

### Tier 3 — Maximum Edge
- Multi-model Bayesian ensemble weighting (all 6+ models)
- GEFS underdispersion correction via historical verification
- Automated calibration pipeline comparing predictions to actual resolutions
- Station-specific microclimate adjustments

---

## Python Libraries for Weather Stack

```
# Core (Tier 1 - MVP)
requests          # Open-Meteo API calls
numpy             # Array operations
scipy             # KDE for probability distributions

# Production (Tier 2)
herbie-data       # GEFS/ECMWF download from AWS/NOMADS
cfgrib            # GRIB2 parsing backend
xarray            # Multi-dimensional data arrays
ecmwf-opendata    # ECMWF open data client

# Optional
pandas            # Time series manipulation
matplotlib        # Visualization for debugging/calibration
```

---

## Scheduling Weather Data Updates

GEFS data appears on AWS ~5-6 hours after initialization:
- 00Z data available ~05:30-06:00 UTC
- 06Z data available ~11:30-12:00 UTC
- 12Z data available ~17:30-18:00 UTC
- 18Z data available ~23:30-00:00 UTC

Schedule sidecar weather fetches at: 06:00, 12:00, 18:00, 00:00 UTC

Open-Meteo updates slightly after NOAA publishes, typically within minutes.
Cache latest data — never block the trading loop waiting for fresh weather data.

---

## Viral Post Claim Evaluation

*"My friend made $27K betting on weather on Polymarket using ensemble forecasts."*

**Verdict: Plausible, with minor embellishments.**

| Claim | Reality | Assessment |
|-------|---------|------------|
| "$27K profit" | Documented traders: "neobrother" made $20K+, one address scaled $1K→$24K on London weather, another bot printed $65K across NYC/London/Seoul. "Hans323" made $1.11M on a single bet. | **Plausible** — $27K is modest vs documented profits |
| "NOAA GFS ensemble (21 forecast runs)" | GFS ensemble = GEFS. Currently 31 members (upgraded from 21 in Sept 2020 with GEFSv12). Post uses outdated member count. | **Real source**, member count is 31 not 21 |
| "ECMWF satellite data" | ECMWF IFS provides 51 ensemble members, freely available since Oct 2025 under CC-BY-4.0. Not "satellite data" — it's NWP model output initialized from satellite + radiosonde + buoy obs. | **Real, but mislabeled** — model forecasts, not raw satellite |
| "Pulls every hour" | GEFS updates 4x/day (00Z, 06Z, 12Z, 18Z). ECMWF also 4x/day. HRRR updates hourly but is deterministic, not ensemble. Open-Meteo updates within minutes of source publication. | **Exaggerated** — 4x/day for ensembles, hourly only for HRRR |
| "Probability per 2°F bucket" | Standard approach: Gaussian KDE over ensemble members → integrate over bucket boundaries. Exactly what our `probability_model.py` does. | **Exactly correct** |
| "Market prices >3%, model says <1% → buy NO at 98-99¢" | The NO-side strategy is the primary documented edge. When ensemble says probability ≈0% but market prices at 3-5%, buying NO at 95-97¢ captures the spread on resolution. | **Core strategy, well-documented** |
| "20 cities in parallel" | Polymarket has ~22 US city temperature markets active at any time. Running all in parallel is standard. | **Correct** |
| "800 lines of Python, $5/month server" | Open-source `polymarket-kalshi-weather-bot` on GitHub is comparable. Our sidecar weather code is ~520 lines. 800 lines for a complete bot is realistic. | **Realistic** |
| "$5/month server" | Hetzner CX22 or Oracle Cloud free tier easily handles this. Computation is trivial — JSON fetch + scipy KDE. | **Correct** |
| "Government satellites" | NOAA operates GOES-16/17/18 and JPSS polar orbiters. This data feeds GFS/GEFS initialization. The bot uses model output derived from satellite obs, not raw imagery. | **Technically true but oversold** |

**Bottom line:** The strategy is real, the data sources are real, and $27K is within documented
profit ranges. The post embellishes details (hourly updates, "satellite data") but the core
edge — ensemble forecasts vs. market mispricing on NO-side bets — is legitimate.

---

## Competitive Landscape (as of Feb 2026)

### Degen Doppler (degendoppler.com)
- 14-model weighted ensemble for daily high temperature predictions
- Compares forecast vs. market odds to identify edge
- Shows "Observed High" from Weather Underground (confirms resolution source)
- Free tool — likely drives some market efficiency

### Wethr.net
- Real-time weather analytics for Kalshi + Polymarket traders
- 33 markets across US and international cities
- Key detail: **matches rounding rules per platform** (different resolution sources)
- Toggle between NWS (Kalshi) and Weather Underground (Polymarket) resolution
- Faster updates than traditional weather sites — speed as a feature

### Open-Source Bots
- `suislanchez/polymarket-kalshi-weather-bot` — FastAPI + Open-Meteo GFS (31 members) + KDE + Kelly sizing
- Uses quarter-Kelly with 5% bankroll cap, 8% edge threshold
- 100% free to run, no paid APIs

### Implication for Our Edge
The market is becoming more efficient as tools proliferate. Edge is shrinking but still
exists, especially through:
1. **Better calibration** (NBM vs. raw GEFS — most competitors skip this)
2. **Faster data access** (direct S3 vs. API wrappers)
3. **More ensemble members** (82+ vs. competitors using only 31)
4. **Station-specific microclimate corrections**
5. **Multi-model weighting** (not just equal-weight averaging)

---

## Advanced Data Sources (Beyond Current Implementation)

### NOAA HGEFS — Hybrid AI+Physics Ensemble (NEW, Dec 2025)
- **What:** 31 physics-based GEFS members + 31 AI-based AIGEFS members = **62 total**
- **How:** NOAA combines traditional FV3 dynamical core ensemble with a GraphCast/FourCastNet
  -style AI ensemble, creating a "grand super ensemble"
- **Performance:** HGEFS "consistently outperforms both GEFS and AIGEFS across most major
  verification metrics" per NOAA's announcement
- **Status:** Experimental/operational since late 2025. Availability on AWS/NOMADS TBD.
- **Edge:** If accessible, this is the single best ensemble source — more members AND
  better calibrated than either physics-only or AI-only systems
- **Action:** Monitor `https://www.emc.ncep.noaa.gov/emc/pages/numerical_forecast_systems/gefs.php`
  for HGEFS data availability announcements

### NOAA NBM Deep Dive — The Underrated Edge
- **What:** National Blend of Models — fuses GEFS, GFS, ECMWF, HRRR, RAP, Canadian, and
  others into a single calibrated product
- **Resolution:** 2.5 km CONUS (vs. 25 km for raw GEFS — **10x finer**)
- **Calibration:** NBM is bias-corrected against actual station observations using
  historical verification. It accounts for:
  - GEFS underdispersion (raw ensembles are systematically overconfident)
  - Terrain effects (coastal, valley, urban heat island)
  - Model-specific systematic errors
  - Diurnal cycle corrections
- **Probabilistic output:** Percentiles (10th, 25th, 50th, 75th, 90th) and exceedance
  probabilities for MaxT/MinT — directly usable for bucket probability estimation
- **Update frequency:** Hourly, forecasts through 264 hours (11 days)
- **Access:** `s3://noaa-nbm-grib2-pds/` — free, GRIB2 format
- **NBM Text Bulletins:** For specific stations, can get CSV-like text output:
  `https://nomads.ncep.noaa.gov/txt_descriptions/BLEND_txt.html`
  - Hourly: `blend_nbhtx.tCCz`
  - Probabilistic: `blend_nbptx.tCCz`
- **Why most competitors skip it:** Requires GRIB2 parsing (cfgrib) and station extraction.
  More complex than Open-Meteo JSON API. But the calibration quality is worth it.
- **Implementation path:** Use `herbie-data` to download NBM GRIB2, extract MaxT percentiles
  at exact airport coordinates, convert percentile distribution to bucket probabilities.

### HRRR (High-Resolution Rapid Refresh) — Same-Day Edge
- **What:** 3km resolution, convection-allowing model with radar assimilation
- **Update:** Every hour (24 cycles/day)
- **Forecast horizon:** 18 hours (48 hours on 00Z/06Z/12Z/18Z cycles)
- **Key advantage:** For markets resolving TODAY, HRRR provides the freshest forecast.
  While competitors wait 6 hours between GEFS updates, HRRR gives hourly updates.
- **Limitation:** Deterministic (single run), not ensemble. Use as a point estimate to
  anchor the KDE distribution, not as a replacement for ensemble probabilities.
- **Access:** `s3://noaa-hrrr-pds/` — free on AWS
- **Strategy:** On market day, weight HRRR heavily for the "most likely" temperature,
  then use ensemble spread from GEFS/ECMWF for the uncertainty bands.

### Multi-Model Super-Ensemble (133+ members)
Open-Meteo provides additional ensemble models beyond our current GEFS + ECMWF:
- GEFS: 31 members
- ECMWF IFS: 51 members
- DWD ICON-EPS: 40 members
- Canadian GEM: 21 members
- UK Met Office: 18 members
- **Total available: 161 members**

Not all members are equal — weight by recent verification skill using:
- **Bayesian Model Averaging (BMA):** Weighted mixture of model distributions
- **Simple skill-weighting:** Track RMSE per model over rolling 30-day window, weight inversely

---

## Resolution Source Matching (CRITICAL for Accuracy)

### Polymarket: Weather Underground
- Resolves via Weather Underground historical data for airport METAR stations
- WU may round differently than NWS — temperature rounding rules matter for edge cases
- WU URL pattern: `https://www.wunderground.com/history/daily/us/ny/new-york-city/KLGA/date/YYYY-MM-DD`
- **Must verify:** Does WU report the highest METAR obs, or a daily summary? Rounding to
  nearest integer or nearest even? These details can shift a bet by one bucket.

### Kalshi: NWS Climatological Report (CLI)
- Resolves via the final NWS CLI report for the station
- CLI reports are issued ~2-6 hours after midnight local time
- CLI and WU can disagree by 1-2°F due to:
  - Different observation windows (midnight-to-midnight vs. calendar day)
  - Rounding methodology
  - Station selection differences

### Action Item
Scrape Weather Underground for historical daily highs at all 22 ICAO stations. Compare
against our ensemble predictions over 30+ days to calibrate the spread correction factor.
This builds the feedback loop that separates profitable bots from demo projects.

---

## Update Timing & Latency (Speed = Edge)

| Source | Init Time | Available On AWS | Latency |
|--------|-----------|-----------------|---------|
| GEFS 00Z | 00:00 UTC | ~05:30-06:00 UTC | ~5.5 hrs |
| GEFS 06Z | 06:00 UTC | ~11:30-12:00 UTC | ~5.5 hrs |
| GEFS 12Z | 12:00 UTC | ~17:30-18:00 UTC | ~5.5 hrs |
| GEFS 18Z | 18:00 UTC | ~23:30-00:00 UTC | ~5.5 hrs |
| ECMWF 00Z | 00:00 UTC | ~06:00-07:00 UTC | ~6-7 hrs |
| ECMWF 12Z | 12:00 UTC | ~18:00-19:00 UTC | ~6-7 hrs |
| HRRR (hourly) | Every hour | ~45 min after init | ~45 min |
| NBM (hourly) | Every hour | ~1 hr after init | ~1 hr |
| Open-Meteo | N/A | Minutes after AWS | +5-15 min vs direct |

**For same-day markets:**
- HRRR hourly updates are the freshest signal (45 min latency)
- NBM hourly updates are the best calibrated (1 hr latency)
- Use both: HRRR for point estimate, NBM for calibrated uncertainty

**For 2-7 day markets:**
- GEFS/ECMWF ensembles are primary (4x daily)
- NBM provides calibrated baseline (hourly, blending all models)
- HRRR adds value only for the nearest 18-48 hour window

---

## Maximum Edge Strategies (Ranked by Impact)

### 1. NBM Integration (Highest Value, Moderate Effort)
Most competitors use raw GEFS which is underdispersive. NBM is already calibrated by NOAA
against actual station observations. Integrating NBM percentiles as a secondary probability
distribution — or as a calibration anchor for our KDE — would make our estimates more
accurate than any raw-ensemble approach.

**Implementation:** Add `GET /weather/nbm?city=NYC&date=YYYY-MM-DD` endpoint to sidecar.
Use Herbie to download NBM GRIB2, extract MaxT percentiles at airport coordinates, return
as bucket probabilities. Blend with ensemble KDE using a weighted average.

### 2. HRRR for Same-Day Markets (High Value, Low Effort)
For markets resolving within 24 hours, HRRR's hourly 3km forecasts give a significant
information advantage. Add an HRRR point-estimate fetch that anchors the ensemble
distribution mean on market day.

**Implementation:** Add `GET /weather/hrrr?city=NYC` endpoint. Fetch latest HRRR via
Herbie, extract 2m temperature at airport coordinates, return as JSON. Use to shift
ensemble mean toward HRRR on same-day markets.

### 3. Multi-Model Ensemble (Medium Value, Low Effort)
Expand from 82 members (GEFS+ECMWF) to 133+ by adding Canadian GEM (21) and DWD ICON-EPS
(40) via Open-Meteo. More members = smoother KDE = better tail probability estimates.

**Implementation:** Add `icon_seamless` and `gem_global` to Open-Meteo model list.
Adjust `open_meteo.py` to parse additional model members.

### 4. Automated Calibration Pipeline (High Value, High Effort)
Track prediction vs. actual resolution over time. Automatically adjust:
- Spread correction factor per city (some stations are more predictable than others)
- Model weights (some models perform better in certain weather regimes)
- Bucket probability biases (systematic over/under-estimation)

**Implementation:** Nightly job that scrapes Weather Underground actuals, compares to
stored predictions, updates calibration parameters in SQLite.

### 5. Direct S3 Access via Herbie (Medium Value, Medium Effort)
Eliminates Open-Meteo as intermediary. Benefits:
- No commercial license concern (US government data = public domain)
- No rate limits
- Slightly faster access (no intermediary processing delay)
- Required anyway for NBM and HRRR integration

**Implementation:** Replace Open-Meteo calls in `open_meteo.py` with Herbie-based
GRIB2 downloads. Or keep Open-Meteo for GEFS/ECMWF and add Herbie only for NBM/HRRR.

### 6. Weather Underground Monitoring (Medium Value, Low Effort)
Since Polymarket resolves via WU, monitoring actual WU observations on market day
provides last-minute edge. If WU shows the temperature has already hit 72°F by 2pm,
the "70-72°F or higher" bucket is locked in — sell any YES on lower buckets.

**Implementation:** Periodic scrape of WU current conditions for all 22 stations.
Use as a real-time constraint on same-day market positions.

---

## GEFS Underdispersion: Detailed Analysis

Raw GEFS ensembles are systematically overconfident. The ensemble spread is narrower
than the actual forecast error distribution. This means:

- Probability of extreme outcomes is underestimated
- Probability of the most likely outcome is overestimated
- NO-side trades on extreme buckets have less edge than the raw model suggests

### Quantifying the Problem
Research shows GEFS 2m temperature spread needs ~1.2-1.5x correction to match observed
forecast error variance. The exact factor varies by:
- Lead time (worse at longer ranges)
- Season (worse in winter with temperature inversions)
- Location (worse in complex terrain)
- Weather regime (worse during pattern transitions)

### Correction Methods (From Simplest to Best)

1. **Linear spread correction** (current implementation):
   `corrected = mean + (value - mean) * correction_factor`
   Our `WEATHER_SPREAD_CORRECTION` env var controls this. Default 1.0 (no correction).
   Recommended starting point: 1.3

2. **NBM as calibrated baseline:**
   Compare raw GEFS KDE to NBM percentile distribution. NBM is already calibrated.
   Use NBM percentiles to set the spread, GEFS members for the shape.

3. **Nonhomogeneous Gaussian Regression (NGR):**
   Fit a regression model: `obs ~ N(a + b*ens_mean, c + d*ens_spread)`
   where a, b, c, d are trained on historical forecast-observation pairs.
   This corrects both the mean bias and the spread simultaneously.

4. **NOAA's decaying-average method:**
   `bias_today = 0.02 * error_today + 0.98 * bias_yesterday`
   Simple, operational, effective. Applied per station per lead time.

### Recommendation
Start with `WEATHER_SPREAD_CORRECTION=1.3`, then implement the automated calibration
pipeline (Strategy #4 above) to learn the optimal per-city, per-lead-time correction
factors from actual Weather Underground resolution data.
