import logging
import os
from contextlib import asynccontextmanager
from datetime import datetime

import uvicorn
from fastapi import FastAPI, HTTPException
from pydantic import BaseModel

from polymarket_client import PolymarketClient
from weather.open_meteo import CITY_CONFIGS, fetch_ensemble, fetch_hrrr, fetch_nws_for_city
from weather.probability_model import blend_probabilities, compute_bucket_probabilities

logger = logging.getLogger("sidecar")

SIDECAR_PORT = int(os.getenv("SIDECAR_PORT", "9090"))
TRADING_MODE = os.getenv("TRADING_MODE", "paper")
WEATHER_SPREAD_CORRECTION = float(os.getenv("WEATHER_SPREAD_CORRECTION", "1.3"))
WEATHER_NBM_WEIGHT = float(os.getenv("WEATHER_NBM_WEIGHT", "0.6"))
WEATHER_NWS_WEIGHT = float(os.getenv("WEATHER_NWS_WEIGHT", "0.6"))
WEATHER_HRRR_WEIGHT = float(os.getenv("WEATHER_HRRR_WEIGHT", "0.3"))
WEATHER_DEFAULT_BIAS_OFFSET = float(os.getenv("WEATHER_DEFAULT_BIAS_OFFSET", "0.0"))
DATABASE_PATH = os.getenv("DATABASE_PATH", "data/polymarket-agent.db")

# Global client instance — initialized at startup
polymarket = PolymarketClient()

# Global calibration params — loaded at startup, refreshed on /weather/calibrate
_calibration_params: dict = {}


def _load_calibration_params() -> dict:
    """Load calibration params from file, returning empty dict on failure."""
    try:
        from weather.calibration import load_calibration
        return load_calibration("calibration_params.json")
    except Exception as e:
        logger.debug("No calibration params loaded: %s", e)
        return {}


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
    global _calibration_params
    logger.info("Sidecar starting on port %d in %s mode", SIDECAR_PORT, TRADING_MODE)
    if TRADING_MODE == "live":
        if polymarket.initialize():
            logger.info("Polymarket client ready for live trading")
        else:
            logger.warning("Polymarket client failed to initialize — /order will return 503")
    else:
        logger.info("Paper mode — Polymarket client not initialized")
    _calibration_params = _load_calibration_params()
    if _calibration_params:
        logger.info("Loaded calibration for %d cities", len(_calibration_params))
    if WEATHER_DEFAULT_BIAS_OFFSET != 0.0:
        logger.info("Default weather bias offset: +%.1f°F", WEATHER_DEFAULT_BIAS_OFFSET)
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
    icon_count: int = 0
    gem_count: int = 0
    total_members: int = 0
    nws_forecast_high: float | None = None
    bias_correction: float = 0.0
    raw_ensemble_mean: float = 0.0
    hrrr_max_temp: float | None = None
    hrrr_shift: float = 0.0
    nbm_max_temp: float | None = None
    calibration_bias: float | None = None
    calibration_spread: float | None = None
    wu_high: float | None = None


class WUActualResponse(BaseModel):
    station: str
    date: str
    actual_high: float | None = None


class ValidateResponse(BaseModel):
    city: str
    forecast_date: str
    ensemble_mean: float | None = None
    nws_forecast_high: float | None = None
    wu_actual_high: float | None = None
    prediction_error: float | None = None
    predicted_bucket: str | None = None
    actual_bucket: str | None = None


class HRRRResponse(BaseModel):
    city: str
    station_icao: str
    forecast_date: str
    max_temp_f: float
    hourly_count: int


class CalibrationParamResponse(BaseModel):
    city: str
    bias_offset: float
    spread_factor: float
    sample_size: int


def _validate_city(city: str) -> None:
    if city not in CITY_CONFIGS:
        raise HTTPException(status_code=404, detail=f"Unknown city: {city}")


def _validate_date(date: str) -> None:
    try:
        datetime.strptime(date, "%Y-%m-%d")
    except ValueError:
        raise HTTPException(
            status_code=400,
            detail=f"Invalid date format: {date}. Expected YYYY-MM-DD",
        )


def _temp_to_bucket_label(temp: float, bucket_width: float = 2.0) -> str:
    """Convert a temperature to the bucket label it falls into."""
    lower = int(temp // bucket_width) * int(bucket_width)
    upper = lower + int(bucket_width)
    return f"{lower}-{upper}"


@app.get("/weather/probabilities", response_model=WeatherResponse)
async def weather_probabilities(city: str, date: str, same_day: bool = False) -> WeatherResponse:
    _validate_city(city)
    _validate_date(date)

    try:
        forecast = await fetch_ensemble(city, date)
    except Exception as e:
        logger.error("Weather fetch failed for %s/%s: %s", city, date, e)
        raise HTTPException(
            status_code=502, detail=f"Upstream weather API failed: {e}"
        )

    if forecast is None:
        raise HTTPException(status_code=502, detail="Weather API returned no data")

    # Fetch NWS bias-corrected forecast (optional — graceful None on failure)
    nws_high = None
    try:
        nws_high = await fetch_nws_for_city(city, date)
    except Exception as e:
        logger.warning("NWS fetch failed for %s/%s: %s", city, date, e)

    # Fetch HRRR for same-day markets (optional)
    hrrr_max = None
    if same_day:
        try:
            hrrr_result = await fetch_hrrr(city, date)
            if hrrr_result is not None:
                hrrr_max = hrrr_result.max_temp_f
        except Exception as e:
            logger.warning("HRRR fetch failed for %s/%s: %s", city, date, e)

    # Fetch WU observations (same-day: partial, past: full, future: None)
    wu_high = None
    try:
        from weather.wu_scraper import fetch_wu_actual
        config = CITY_CONFIGS[city]
        wu_high = await fetch_wu_actual(config.icao, date)
    except ImportError:
        logger.debug("WU scraper not available")
    except Exception as e:
        logger.debug("WU fetch for %s/%s: %s", city, date, e)

    # Determine calibration bias: per-city from calibration data, else default
    cal_bias = None
    cal_spread = None
    if city in _calibration_params:
        cal = _calibration_params[city]
        cal_bias = cal.bias_offset
        cal_spread = cal.spread_factor
        logger.info("Using calibration for %s: bias=%.2f, spread=%.2f (n=%d)",
                     city, cal_bias, cal_spread, cal.sample_size)
    elif WEATHER_DEFAULT_BIAS_OFFSET != 0.0:
        cal_bias = WEATHER_DEFAULT_BIAS_OFFSET
        logger.info("Using default bias offset for %s: +%.1f°F", city, cal_bias)

    probs = compute_bucket_probabilities(
        forecast,
        spread_correction=WEATHER_SPREAD_CORRECTION,
        nws_high=nws_high,
        nws_weight=WEATHER_NWS_WEIGHT,
        hrrr_max=hrrr_max,
        hrrr_weight=WEATHER_HRRR_WEIGHT,
        calibration_bias=cal_bias,
        calibration_spread=cal_spread,
    )

    # Try NBM blending (optional — graceful degradation if herbie not installed)
    nbm_max_temp = None
    try:
        from weather.nbm import fetch_nbm_percentiles, nbm_percentiles_to_buckets

        nbm = await fetch_nbm_percentiles(city, date)
        if nbm is not None:
            nbm_max_temp = nbm.max_temp
            nbm_buckets = nbm_percentiles_to_buckets(nbm)
            if nbm_buckets and WEATHER_NBM_WEIGHT > 0:
                probs_buckets = blend_probabilities(
                    probs.buckets, nbm_buckets, nbm_weight=WEATHER_NBM_WEIGHT
                )
                probs.buckets = probs_buckets
                probs.nbm_max_temp = nbm_max_temp
                probs.nbm_percentiles = {
                    "p10": nbm.p10, "p25": nbm.p25, "p50": nbm.p50,
                    "p75": nbm.p75, "p90": nbm.p90,
                }
    except ImportError:
        logger.debug("NBM module not available (herbie not installed), skipping")
    except Exception as e:
        logger.warning("NBM blending failed for %s/%s: %s", city, date, e)

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
        icon_count=probs.icon_count,
        gem_count=probs.gem_count,
        total_members=probs.total_members,
        nws_forecast_high=probs.nws_forecast_high,
        bias_correction=probs.bias_correction,
        raw_ensemble_mean=probs.raw_ensemble_mean,
        hrrr_max_temp=probs.hrrr_max_temp,
        hrrr_shift=probs.hrrr_shift,
        nbm_max_temp=nbm_max_temp,
        calibration_bias=probs.calibration_bias,
        calibration_spread=probs.calibration_spread,
        wu_high=wu_high,
    )


@app.get("/weather/wu_actual", response_model=WUActualResponse)
async def wu_actual(station: str, date: str) -> WUActualResponse:
    _validate_date(date)
    try:
        from weather.wu_scraper import fetch_wu_actual

        actual = await fetch_wu_actual(station, date)
        return WUActualResponse(station=station, date=date, actual_high=actual)
    except ImportError:
        raise HTTPException(status_code=501, detail="WU scraper not available")
    except Exception as e:
        logger.error("WU actual fetch failed for %s/%s: %s", station, date, e)
        raise HTTPException(status_code=502, detail=str(e))


@app.get("/weather/validate", response_model=ValidateResponse)
async def weather_validate(city: str, date: str) -> ValidateResponse:
    _validate_city(city)
    _validate_date(date)

    config = CITY_CONFIGS[city]
    ensemble_mean = None
    nws_high = None
    wu_high = None

    # Get ensemble prediction
    try:
        forecast = await fetch_ensemble(city, date)
        if forecast and forecast.all_members:
            import numpy as np
            ensemble_mean = float(np.mean(forecast.all_members))
    except Exception as e:
        logger.warning("Validation: ensemble fetch failed for %s/%s: %s", city, date, e)

    # Get NWS forecast
    try:
        nws_high = await fetch_nws_for_city(city, date)
    except Exception as e:
        logger.warning("Validation: NWS fetch failed for %s/%s: %s", city, date, e)

    # Get WU actual
    try:
        from weather.wu_scraper import fetch_wu_actual
        wu_high = await fetch_wu_actual(config.icao, date)
    except ImportError:
        logger.debug("WU scraper not available")
    except Exception as e:
        logger.warning("Validation: WU fetch failed for %s/%s: %s", city, date, e)

    # Compute error and buckets
    prediction_error = None
    predicted_bucket = None
    actual_bucket = None

    if ensemble_mean is not None:
        predicted_bucket = _temp_to_bucket_label(ensemble_mean)
    if wu_high is not None:
        actual_bucket = _temp_to_bucket_label(wu_high)
    if ensemble_mean is not None and wu_high is not None:
        prediction_error = ensemble_mean - wu_high

    return ValidateResponse(
        city=city,
        forecast_date=date,
        ensemble_mean=ensemble_mean,
        nws_forecast_high=nws_high,
        wu_actual_high=wu_high,
        prediction_error=prediction_error,
        predicted_bucket=predicted_bucket,
        actual_bucket=actual_bucket,
    )


@app.get("/weather/hrrr", response_model=HRRRResponse)
async def weather_hrrr(city: str, date: str) -> HRRRResponse:
    _validate_city(city)
    _validate_date(date)

    try:
        result = await fetch_hrrr(city, date)
    except Exception as e:
        logger.error("HRRR fetch failed for %s/%s: %s", city, date, e)
        raise HTTPException(status_code=502, detail=f"HRRR fetch failed: {e}")

    if result is None:
        raise HTTPException(status_code=502, detail="HRRR returned no data")

    return HRRRResponse(
        city=result.city,
        station_icao=result.station_icao,
        forecast_date=result.forecast_date,
        max_temp_f=result.max_temp_f,
        hourly_count=len(result.hourly_temps_f),
    )


@app.get("/weather/nbm")
async def weather_nbm(city: str, date: str):
    _validate_city(city)
    _validate_date(date)

    try:
        from weather.nbm import fetch_nbm_percentiles
    except ImportError:
        raise HTTPException(status_code=501, detail="NBM module not available (herbie not installed)")

    try:
        nbm = await fetch_nbm_percentiles(city, date)
    except Exception as e:
        logger.error("NBM fetch failed for %s/%s: %s", city, date, e)
        raise HTTPException(status_code=502, detail=f"NBM fetch failed: {e}")

    if nbm is None:
        raise HTTPException(status_code=502, detail="NBM returned no data")

    return {
        "city": nbm.city,
        "date": nbm.date,
        "max_temp": nbm.max_temp,
        "percentiles": {
            "p10": nbm.p10, "p25": nbm.p25, "p50": nbm.p50,
            "p75": nbm.p75, "p90": nbm.p90,
        },
    }


@app.get("/weather/calibration")
async def weather_calibration():
    try:
        from weather.calibration import load_calibration

        params = load_calibration("calibration_params.json")
        return {
            "params": {
                city: {
                    "bias_offset": p.bias_offset,
                    "spread_factor": p.spread_factor,
                    "sample_size": p.sample_size,
                }
                for city, p in params.items()
            },
            "count": len(params),
        }
    except ImportError:
        raise HTTPException(status_code=501, detail="Calibration module not available")
    except FileNotFoundError:
        return {"params": {}, "count": 0, "note": "No calibration data yet"}
    except Exception as e:
        logger.error("Calibration load failed: %s", e)
        raise HTTPException(status_code=500, detail=str(e))


@app.post("/weather/calibrate")
async def weather_calibrate(db_path: str = DATABASE_PATH):
    global _calibration_params
    try:
        from weather.calibration import compute_calibration, save_calibration

        params = compute_calibration(db_path)
        save_calibration(params, "calibration_params.json")
        _calibration_params = params  # Refresh in-memory params
        return {
            "status": "ok",
            "cities_calibrated": len(params),
            "params": {
                city: {
                    "bias_offset": p.bias_offset,
                    "spread_factor": p.spread_factor,
                    "sample_size": p.sample_size,
                }
                for city, p in params.items()
            },
        }
    except ImportError:
        raise HTTPException(status_code=501, detail="Calibration module not available")
    except Exception as e:
        logger.error("Calibration failed: %s", e)
        raise HTTPException(status_code=500, detail=str(e))


class CollectActualRequest(BaseModel):
    city: str
    date: str
    ensemble_mean: float | None = None
    nws_forecast_high: float | None = None


class CollectActualResponse(BaseModel):
    city: str
    date: str
    station_icao: str
    wu_actual_high: float | None = None
    stored: bool = False


@app.post("/weather/collect_actual", response_model=CollectActualResponse)
async def collect_actual(req: CollectActualRequest) -> CollectActualResponse:
    """Fetch WU actual for a city/date and store in weather_actuals table."""
    _validate_city(req.city)
    _validate_date(req.date)

    config = CITY_CONFIGS[req.city]
    wu_high = None
    try:
        from weather.wu_scraper import fetch_wu_actual
        wu_high = await fetch_wu_actual(config.icao, req.date)
    except ImportError:
        raise HTTPException(status_code=501, detail="WU scraper not available")
    except Exception as e:
        logger.warning("WU fetch failed for %s/%s: %s", req.city, req.date, e)

    stored = False
    if wu_high is not None:
        try:
            import sqlite3
            conn = sqlite3.connect(DATABASE_PATH)
            actual_bucket = _temp_to_bucket_label(wu_high)
            predicted_bucket = _temp_to_bucket_label(req.ensemble_mean) if req.ensemble_mean else None
            prediction_error = (req.ensemble_mean - wu_high) if req.ensemble_mean else None
            conn.execute(
                """INSERT INTO weather_actuals (city, forecast_date, wu_actual_high, nws_forecast_high,
                   ensemble_mean, predicted_bucket, actual_bucket, prediction_error)
                   VALUES (?, ?, ?, ?, ?, ?, ?, ?)
                   ON CONFLICT(city, forecast_date) DO UPDATE SET
                     wu_actual_high = COALESCE(excluded.wu_actual_high, wu_actual_high),
                     nws_forecast_high = COALESCE(excluded.nws_forecast_high, nws_forecast_high),
                     ensemble_mean = COALESCE(excluded.ensemble_mean, ensemble_mean),
                     predicted_bucket = COALESCE(excluded.predicted_bucket, predicted_bucket),
                     actual_bucket = COALESCE(excluded.actual_bucket, actual_bucket),
                     prediction_error = COALESCE(excluded.prediction_error, prediction_error)""",
                (req.city, req.date, wu_high, req.nws_forecast_high,
                 req.ensemble_mean, predicted_bucket, actual_bucket, prediction_error),
            )
            conn.commit()
            conn.close()
            stored = True
            logger.info("Stored WU actual for %s/%s: %.0f°F", req.city, req.date, wu_high)
        except Exception as e:
            logger.error("Failed to store WU actual: %s", e)

    return CollectActualResponse(
        city=req.city,
        date=req.date,
        station_icao=config.icao,
        wu_actual_high=wu_high,
        stored=stored,
    )


class CollectBatchResponse(BaseModel):
    date: str
    collected: int
    failed: int
    results: list[CollectActualResponse]


@app.post("/weather/collect_actuals_batch", response_model=CollectBatchResponse)
async def collect_actuals_batch(date: str | None = None) -> CollectBatchResponse:
    """Collect WU actuals for all 20 cities for a given date (default: yesterday)."""
    if date is None:
        from datetime import timedelta
        yesterday = datetime.utcnow() - timedelta(days=1)
        date = yesterday.strftime("%Y-%m-%d")
    _validate_date(date)

    results = []
    collected = 0
    failed = 0

    for city_code in CITY_CONFIGS:
        config = CITY_CONFIGS[city_code]
        wu_high = None
        try:
            from weather.wu_scraper import fetch_wu_actual
            wu_high = await fetch_wu_actual(config.icao, date)
        except Exception as e:
            logger.warning("Batch WU fetch failed for %s/%s: %s", city_code, date, e)

        stored = False
        if wu_high is not None:
            try:
                import sqlite3
                conn = sqlite3.connect(DATABASE_PATH)
                actual_bucket = _temp_to_bucket_label(wu_high)
                conn.execute(
                    """INSERT INTO weather_actuals (city, forecast_date, wu_actual_high, actual_bucket)
                       VALUES (?, ?, ?, ?)
                       ON CONFLICT(city, forecast_date) DO UPDATE SET
                         wu_actual_high = COALESCE(excluded.wu_actual_high, wu_actual_high),
                         actual_bucket = COALESCE(excluded.actual_bucket, actual_bucket)""",
                    (city_code, date, wu_high, actual_bucket),
                )
                conn.commit()
                conn.close()
                stored = True
                collected += 1
            except Exception as e:
                logger.error("Failed to store batch WU actual for %s: %s", city_code, e)
                failed += 1
        else:
            failed += 1

        results.append(CollectActualResponse(
            city=city_code,
            date=date,
            station_icao=config.icao,
            wu_actual_high=wu_high,
            stored=stored,
        ))

    logger.info("Batch WU collection for %s: %d collected, %d failed", date, collected, failed)
    return CollectBatchResponse(
        date=date,
        collected=collected,
        failed=failed,
        results=results,
    )


if __name__ == "__main__":
    logging.basicConfig(
        level=logging.INFO,
        format="%(asctime)s [%(name)s] %(levelname)s: %(message)s",
    )
    uvicorn.run(app, host="0.0.0.0", port=SIDECAR_PORT, log_level="info")
