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
