import logging
import os
from contextlib import asynccontextmanager
from datetime import datetime

import uvicorn
from fastapi import FastAPI, HTTPException
from pydantic import BaseModel

from polymarket_client import PolymarketClient
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
    nws_forecast_high: float | None = None
    bias_correction: float | None = None


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

    # Fetch NWS bias-corrected forecast as anchor (best-effort, non-blocking)
    config = CITY_CONFIGS[city]
    nws_high: float | None = None
    try:
        nws_high = await fetch_nws_forecast(config.lat, config.lon, date)
    except Exception as e:
        logger.warning("NWS fetch failed for %s/%s (continuing without): %s", city, date, e)

    probs = compute_bucket_probabilities(
        forecast,
        spread_correction=WEATHER_SPREAD_CORRECTION,
        nws_anchor=nws_high,
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
        nws_forecast_high=probs.nws_forecast_high,
        bias_correction=probs.bias_correction,
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

    return WeatherValidationResponse(
        city=city,
        date=date,
        wu_actual_high=wu_high,
        nws_forecast_high=nws_high,
        ensemble_raw_mean=ensemble_raw_mean,
        ensemble_corrected_mean=ensemble_corrected_mean,
        error_vs_wu=error_vs_wu,
    )


if __name__ == "__main__":
    logging.basicConfig(
        level=logging.INFO,
        format="%(asctime)s [%(name)s] %(levelname)s: %(message)s",
    )
    uvicorn.run(app, host="0.0.0.0", port=SIDECAR_PORT, log_level="info")
