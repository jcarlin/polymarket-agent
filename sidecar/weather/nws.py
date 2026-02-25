"""NWS API client for bias-corrected temperature forecasts.

Uses api.weather.gov (free, no auth) to get official NWS point forecasts.
These are MOS-corrected and serve as an anchor to shift raw ensemble
distributions, fixing the ~5°F cold bias in raw GEFS/ECMWF output.
"""

import asyncio
import logging
from typing import Optional

import httpx

logger = logging.getLogger("weather.nws")

NWS_API_BASE = "https://api.weather.gov"
NWS_USER_AGENT = "polymarket-weather-agent/1.0 (weather@example.com)"

# Cache grid point URLs per (lat, lon) — these don't change
_grid_cache: dict[tuple[float, float], str] = {}


async def _get_forecast_url(
    lat: float, lon: float, session: httpx.AsyncClient
) -> Optional[str]:
    """Get the forecast URL for a lat/lon from NWS /points endpoint.

    Caches results since grid mappings are stable.
    """
    key = (round(lat, 4), round(lon, 4))
    if key in _grid_cache:
        return _grid_cache[key]

    url = f"{NWS_API_BASE}/points/{lat:.4f},{lon:.4f}"
    try:
        resp = await session.get(url)
        resp.raise_for_status()
        data = resp.json()
        forecast_url = data.get("properties", {}).get("forecast")
        if forecast_url:
            _grid_cache[key] = forecast_url
            return forecast_url
        logger.warning("No forecast URL in NWS /points response for %s,%s", lat, lon)
        return None
    except Exception as e:
        logger.warning("NWS /points request failed for %s,%s: %s", lat, lon, e)
        return None


async def fetch_nws_forecast(
    lat: float,
    lon: float,
    target_date: str,
    session: Optional[httpx.AsyncClient] = None,
    max_retries: int = 3,
) -> Optional[float]:
    """Fetch NWS official forecast high temperature for a location and date.

    Args:
        lat: Latitude
        lon: Longitude
        target_date: YYYY-MM-DD format
        session: Optional async HTTP client (reused to avoid overhead)
        max_retries: Number of retry attempts

    Returns:
        Forecast high temperature in °F, or None on failure.
    """
    close_session = False
    if session is None:
        session = httpx.AsyncClient(
            timeout=15.0,
            headers={"User-Agent": NWS_USER_AGENT, "Accept": "application/geo+json"},
        )
        close_session = True

    try:
        # Step 1: Get forecast URL from /points endpoint
        forecast_url = await _get_forecast_url(lat, lon, session)
        if forecast_url is None:
            return None

        # Step 2: Fetch the 7-day forecast
        last_err: Optional[Exception] = None
        data = None
        for attempt in range(max_retries):
            try:
                # NWS recommends ~1s between requests
                if attempt > 0:
                    await asyncio.sleep(2.0 * attempt)
                resp = await session.get(forecast_url)
                resp.raise_for_status()
                data = resp.json()
                break
            except Exception as e:
                last_err = e
                logger.debug(
                    "NWS forecast attempt %d failed: %s", attempt + 1, e
                )

        if data is None:
            logger.warning(
                "NWS forecast failed after %d retries: %s", max_retries, last_err
            )
            return None

        # Step 3: Find the daytime period matching target_date
        periods = data.get("properties", {}).get("periods", [])
        for period in periods:
            if not period.get("isDaytime", False):
                continue
            # Period startTime is like "2026-02-12T06:00:00-05:00"
            start_time = period.get("startTime", "")
            if start_time.startswith(target_date):
                temp = period.get("temperature")
                unit = period.get("temperatureUnit", "F")
                if temp is not None:
                    temp_f = float(temp)
                    if unit == "C":
                        temp_f = temp_f * 9.0 / 5.0 + 32.0
                    logger.info(
                        "NWS forecast for %s,%s on %s: %.1f°F",
                        lat, lon, target_date, temp_f,
                    )
                    return temp_f

        logger.warning(
            "No daytime period found for %s in NWS forecast (got %d periods)",
            target_date, len(periods),
        )
        return None

    finally:
        if close_session:
            await session.aclose()


def clear_grid_cache() -> None:
    """Clear the grid point URL cache (useful for testing)."""
    _grid_cache.clear()
