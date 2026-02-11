# Phase 5: Weather Pipeline (Highest Edge)

## Status: COMPLETE

### Checklist

- [x] `sidecar/weather/__init__.py` — Empty module init
- [x] `sidecar/weather/open_meteo.py` — Open-Meteo Ensemble API client (20 cities, GEFS+ECMWF, UTC-to-local, retry logic)
- [x] `sidecar/weather/probability_model.py` — Gaussian KDE → 2°F bucket probabilities (scipy, histogram fallback)
- [x] `sidecar/weather/test_open_meteo.py` — 8 tests (city configs, temp conversion, daily max extraction, unknown city)
- [x] `sidecar/weather/test_probability_model.py` — 10 tests (KDE sum≈1, degenerate cases, spread correction, bimodal)
- [x] `sidecar/server.py` — GET /weather/probabilities endpoint (404/400/502 error handling)
- [x] `sidecar/test_server.py` — 4 new tests (unknown city, invalid date, missing params, endpoint exists)
- [x] `src/weather_client.rs` — WeatherClient, parse_weather_market, get_weather_model_probability + 11 tests
- [x] `src/config.rs` — weather_spread_correction field added
- [x] `src/market_scanner.rs` — test_config() updated
- [x] `src/lib.rs` — weather_client module registered
- [x] `Cargo.toml` — regex dependency added
- [x] `src/estimator.rs` — WeatherContext struct, weather param on evaluate/analyze/render_prompt, weather block rendering
- [x] `src/main.rs` — WeatherClient init, weather cache per city/date, weather context construction in analysis loop
- [x] `tests/integration.rs` — 3 new tests (parsing+bucket lookup, client deserialization, non-weather returns none)
- [x] `.env.example` — WEATHER_SPREAD_CORRECTION added
- [x] `cargo build` — passes, zero errors
- [x] `cargo test` — 107 tests pass (96 unit + 11 integration)
- [x] `cargo clippy -- -W clippy::all` — zero warnings
- [x] `cd sidecar && python -m pytest -v` — 29 tests pass
