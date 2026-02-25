import logging
import os
from contextlib import asynccontextmanager
from datetime import datetime

import uvicorn
from fastapi import FastAPI, HTTPException
from pydantic import BaseModel

from polymarket_client import PolymarketClient
from weather.calibration import get_calibration, get_db_connection, log_validation, run_nightly_calibration
from weather.hrrr import fetch_hrrr_temperature, is_available as hrrr_is_available, is_same_day
from weather.nbm import fetch_nbm_percentiles, is_available as nbm_is_available, nbm_anchor_and_spread
from weather.nws import fetch_nws_forecast
from weather.open_meteo import CITY_CONFIGS, fetch_ensemble
from weather.probability_model import compute_bucket_probabilities
from weather.wu_scraper import fetch_wu_daily_high

logger = logging.getLogger("sidecar")

SIDECAR_PORT = int(os.getenv("SIDECAR_PORT", "9090"))
TRADING_MODE = os.getenv("TRADING_MODE", "paper")
WEATHER_SPREAD_CORRECTION = float(os.getenv("WEATHER_SPREAD_CORRECTION", "1.0"))

# Global client instance — initialized at startup
polymarket = PolymarketClient()


class HealthResponse(BaseModel):
    status: str
    version: str
    trading_mode: str


class OrderRequest(BaseModel):
    token_id: str
    price: float
    size: float
    side: str
    order_type: str = "GTC"


class OrderResponse(BaseModel):
    order_id: str
    status: str
    price: float
    size: float


@asynccontextmanager
async def lifespan(app: FastAPI):
    logger.info("Sidecar starting on port %d in %s mode", SIDECAR_PORT, TRADING_MODE)
    if TRADING_MODE == "live":
        if polymarket.initialize():
            logger.info("Polymarket client ready for live trading")
        else:
            logger.warning("Polymarket client failed to initialize — /order will return 503")
    else:
        logger.info("Paper mode — Polymarket client not initialized")
    yield
    logger.info("Sidecar shutting down")


app = FastAPI(title="Polymarket Sidecar", version="0.1.0", lifespan=lifespan)


@app.get("/health", response_model=HealthResponse)
async def health():
    return HealthResponse(
        status="ok",
        version="0.1.0",
        trading_mode=TRADING_MODE,
    )


@app.post("/order", response_model=OrderResponse)
async def place_order(req: OrderRequest):
    if not polymarket.is_initialized:
        raise HTTPException(
            status_code=503,
            detail="Polymarket client not initialized (paper mode or missing private key)",
        )

    try:
        result = polymarket.place_order(
            token_id=req.token_id,
            price=req.price,
            size=req.size,
            side=req.side,
        )
        return OrderResponse(
            order_id=result["order_id"],
            status=result["status"],
            price=req.price,
            size=req.size,
        )
    except Exception as e:
        logger.error("Order placement failed: %s", e)
        raise HTTPException(status_code=500, detail=str(e))


class BucketResponse(BaseModel):
    bucket_label: str
    lower: float
    upper: float
    probability: float


class WeatherResponse(BaseModel):
    city: str
    station_icao: str
    forecast_date: str
    buckets: list[BucketResponse]
    ensemble_mean: float
    ensemble_std: float
    gefs_count: int
    ecmwf_count: int
    gem_count: int = 0
    icon_count: int = 0
    nws_forecast_high: float | None = None
    bias_correction: float | None = None
    nbm_p50: float | None = None
    anchor_source: str = "raw"


@app.get("/weather/probabilities", response_model=WeatherResponse)
async def weather_probabilities(city: str, date: str) -> WeatherResponse:
    if city not in CITY_CONFIGS:
        raise HTTPException(status_code=404, detail=f"Unknown city: {city}")

    # Validate date format
    try:
        datetime.strptime(date, "%Y-%m-%d")
    except ValueError:
        raise HTTPException(
            status_code=400,
            detail=f"Invalid date format: {date}. Expected YYYY-MM-DD",
        )

    try:
        forecast = await fetch_ensemble(city, date)
    except Exception as e:
        logger.error("Weather fetch failed for %s/%s: %s", city, date, e)
        raise HTTPException(
            status_code=502, detail=f"Upstream weather API failed: {e}"
        )

    if forecast is None:
        raise HTTPException(status_code=502, detail="Weather API returned no data")

    config = CITY_CONFIGS[city]

    # Fallback chain: HRRR (same-day) → NBM → NWS → raw ensemble
    nws_high: float | None = None
    nbm_anchor_val: float | None = None
    nbm_spread_val: float | None = None
    hrrr_anchor_val: float | None = None

    # Try HRRR first for same-day markets (freshest hourly data)
    if is_same_day(date) and hrrr_is_available():
        try:
            hrrr_data = fetch_hrrr_temperature(config.lat, config.lon)
            if hrrr_data is not None:
                hrrr_anchor_val = hrrr_data.max_temp_f
                logger.info("HRRR anchor for %s: %.1f°F (init=%s, %dh)",
                            city, hrrr_anchor_val, hrrr_data.init_time, hrrr_data.valid_hours)
        except Exception as e:
            logger.warning("HRRR fetch failed for %s (falling back): %s", city, e)

    # Try NBM (gold standard, calibrated percentiles)
    if nbm_is_available():
        try:
            import numpy as np

            raw_std = float(np.std(forecast.all_members)) if len(forecast.all_members) > 1 else 0.0
            nbm_data = fetch_nbm_percentiles(config.lat, config.lon, date)
            if nbm_data is not None:
                nbm_anchor_val, nbm_spread_val = nbm_anchor_and_spread(nbm_data, raw_std)
                logger.info("NBM anchor for %s: p50=%.1f, spread=%.2f", city, nbm_anchor_val, nbm_spread_val)
        except Exception as e:
            logger.warning("NBM fetch failed for %s/%s (falling back to NWS): %s", city, date, e)

    # Fetch NWS as fallback anchor (best-effort, non-blocking)
    try:
        nws_high = await fetch_nws_forecast(config.lat, config.lon, date)
    except Exception as e:
        logger.warning("NWS fetch failed for %s/%s (continuing without): %s", city, date, e)

    # HRRR overrides NBM anchor for same-day (freshest data wins)
    # but use NBM spread (better calibrated uncertainty)
    effective_nbm_anchor = nbm_anchor_val
    if hrrr_anchor_val is not None:
        effective_nbm_anchor = hrrr_anchor_val

    # Check for per-city calibration (overrides global spread correction)
    effective_spread = WEATHER_SPREAD_CORRECTION
    try:
        cal_db = get_db_connection()
        cal = get_calibration(cal_db, city)
        if cal is not None:
            effective_spread = cal.spread_factor
            logger.debug("Using calibrated spread=%.2f for %s", effective_spread, city)
        cal_db.close()
    except Exception as e:
        logger.debug("Calibration lookup failed for %s: %s", city, e)

    probs = compute_bucket_probabilities(
        forecast,
        spread_correction=effective_spread,
        nws_anchor=nws_high,
        nbm_anchor=effective_nbm_anchor,
        nbm_spread=nbm_spread_val,
    )

    return WeatherResponse(
        city=probs.city,
        station_icao=probs.station_icao,
        forecast_date=probs.forecast_date,
        buckets=[
            BucketResponse(
                bucket_label=b.bucket_label,
                lower=b.lower,
                upper=b.upper,
                probability=b.probability,
            )
            for b in probs.buckets
        ],
        ensemble_mean=probs.ensemble_mean,
        ensemble_std=probs.ensemble_std,
        gefs_count=probs.gefs_count,
        ecmwf_count=probs.ecmwf_count,
        gem_count=probs.gem_count,
        icon_count=probs.icon_count,
        nws_forecast_high=probs.nws_forecast_high,
        bias_correction=probs.bias_correction,
        nbm_p50=probs.nbm_p50,
        anchor_source=probs.anchor_source,
    )


class WeatherValidationResponse(BaseModel):
    city: str
    date: str
    wu_actual_high: float | None = None
    nws_forecast_high: float | None = None
    ensemble_raw_mean: float | None = None
    ensemble_corrected_mean: float | None = None
    error_vs_wu: float | None = None


@app.get("/weather/validation", response_model=WeatherValidationResponse)
async def weather_validation(city: str, date: str) -> WeatherValidationResponse:
    """Compare WU actual vs NWS forecast vs ensemble for a past date."""
    if city not in CITY_CONFIGS:
        raise HTTPException(status_code=404, detail=f"Unknown city: {city}")

    try:
        datetime.strptime(date, "%Y-%m-%d")
    except ValueError:
        raise HTTPException(
            status_code=400,
            detail=f"Invalid date format: {date}. Expected YYYY-MM-DD",
        )

    config = CITY_CONFIGS[city]

    # Fetch all three data sources in parallel (best-effort each)
    wu_high: float | None = None
    nws_high: float | None = None
    ensemble_raw_mean: float | None = None
    ensemble_corrected_mean: float | None = None

    try:
        wu_high = await fetch_wu_daily_high(config.icao, date)
    except Exception as e:
        logger.warning("WU fetch failed for %s/%s: %s", city, date, e)

    try:
        nws_high = await fetch_nws_forecast(config.lat, config.lon, date)
    except Exception as e:
        logger.warning("NWS fetch failed for %s/%s: %s", city, date, e)

    try:
        forecast = await fetch_ensemble(city, date)
        if forecast and forecast.all_members:
            import numpy as np

            ensemble_raw_mean = float(np.mean(forecast.all_members))
            probs = compute_bucket_probabilities(
                forecast, spread_correction=WEATHER_SPREAD_CORRECTION, nws_anchor=nws_high,
            )
            ensemble_corrected_mean = probs.ensemble_mean
    except Exception as e:
        logger.warning("Ensemble fetch failed for %s/%s: %s", city, date, e)

    error_vs_wu: float | None = None
    if wu_high is not None and ensemble_corrected_mean is not None:
        error_vs_wu = ensemble_corrected_mean - wu_high

    # Log validation data for calibration pipeline
    try:
        cal_db = get_db_connection()
        log_validation(cal_db, city, date, wu_high, nws_high, ensemble_raw_mean, ensemble_corrected_mean)
        cal_db.close()
    except Exception as e:
        logger.debug("Validation logging failed: %s", e)

    return WeatherValidationResponse(
        city=city,
        date=date,
        wu_actual_high=wu_high,
        nws_forecast_high=nws_high,
        ensemble_raw_mean=ensemble_raw_mean,
        ensemble_corrected_mean=ensemble_corrected_mean,
        error_vs_wu=error_vs_wu,
    )


class CalibrationResponse(BaseModel):
    cities_calibrated: int
    cities_total: int
    results: dict[str, dict]


@app.post("/weather/calibrate", response_model=CalibrationResponse)
async def trigger_calibration() -> CalibrationResponse:
    """Trigger nightly calibration for all cities."""
    try:
        cal_db = get_db_connection()
        cities = list(CITY_CONFIGS.keys())
        results = run_nightly_calibration(cal_db, cities)
        cal_db.close()

        return CalibrationResponse(
            cities_calibrated=len(results),
            cities_total=len(cities),
            results={
                city: {
                    "mean_bias": cal.mean_bias,
                    "spread_factor": cal.spread_factor,
                    "sample_count": cal.sample_count,
                }
                for city, cal in results.items()
            },
        )
    except Exception as e:
        logger.error("Calibration failed: %s", e)
        raise HTTPException(status_code=500, detail=str(e))


if __name__ == "__main__":
    logging.basicConfig(
        level=logging.INFO,
        format="%(asctime)s [%(name)s] %(levelname)s: %(message)s",
    )
    uvicorn.run(app, host="0.0.0.0", port=SIDECAR_PORT, log_level="info")
