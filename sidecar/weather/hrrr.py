"""HRRR (High-Resolution Rapid Refresh) fetcher for same-day temperature forecasts.

HRRR provides hourly 3km resolution forecasts, updated every hour. For markets
resolving TODAY, HRRR gives the freshest and most accurate data available.

Uses Herbie library for AWS S3 access. Falls back gracefully if unavailable.
"""

import logging
from dataclasses import dataclass
from datetime import datetime, timedelta, timezone
from typing import Optional

logger = logging.getLogger("weather.hrrr")

_HERBIE_AVAILABLE = False
try:
    from herbie import Herbie  # noqa: F401

    _HERBIE_AVAILABLE = True
    logger.info("Herbie library available — HRRR fetching enabled")
except ImportError:
    logger.info("Herbie library not available — HRRR fetching disabled")


@dataclass
class HRRRForecast:
    """HRRR temperature forecast at a point."""

    max_temp_f: float  # Forecast daily max temperature in °F
    init_time: str  # ISO format of HRRR initialization time
    valid_hours: int  # Number of forecast hours used
    lat: float
    lon: float


def is_available() -> bool:
    """Check if HRRR fetching is available (Herbie installed)."""
    return _HERBIE_AVAILABLE


def fetch_hrrr_temperature(
    lat: float,
    lon: float,
    max_retries: int = 2,
) -> Optional[HRRRForecast]:
    """Fetch HRRR forecast high temperature for remaining hours today.

    Downloads the latest HRRR run from AWS S3 via Herbie, extracts 2m
    temperature at airport coordinates for remaining daylight hours,
    and returns the forecast daily max.

    Args:
        lat: Latitude
        lon: Longitude
        max_retries: Number of retry attempts

    Returns:
        HRRRForecast with max_temp_f, or None on failure.
    """
    if not _HERBIE_AVAILABLE:
        return None

    try:
        return _fetch_hrrr_herbie(lat, lon, max_retries)
    except Exception as e:
        logger.warning("HRRR fetch failed for %.4f,%.4f: %s", lat, lon, e)
        return None


def _fetch_hrrr_herbie(
    lat: float,
    lon: float,
    max_retries: int,
) -> Optional[HRRRForecast]:
    """Internal: fetch HRRR via Herbie."""
    from herbie import Herbie
    import numpy as np

    now_utc = datetime.now(timezone.utc)

    # Try the last few HRRR runs (they're available ~1h after init)
    # HRRR runs every hour; try current hour -2, -3, -4
    init_times = [
        now_utc.replace(minute=0, second=0, microsecond=0) - timedelta(hours=h)
        for h in range(2, 5)
    ]

    for init_time in init_times:
        try:
            # Forecast hours remaining today (up to 18h, HRRR goes to 48h)
            hours_left = max(1, min(18, 24 - init_time.hour))

            max_temps = []
            # Sample a few forecast hours to find daily max
            for fxx in range(1, hours_left + 1, 1):
                try:
                    H = Herbie(
                        init_time.strftime("%Y-%m-%d %H:%M"),
                        model="hrrr",
                        product="sfc",
                        fxx=fxx,
                        verbose=False,
                    )

                    ds = H.xarray(":TMP:2 m above ground:")
                    point = ds.sel(
                        latitude=lat,
                        longitude=lon % 360,
                        method="nearest",
                    )

                    val = float(point.values.flatten()[0])
                    # Convert K to F
                    if val > 200:
                        val = (val - 273.15) * 9 / 5 + 32
                    elif -50 < val < 60:  # Celsius
                        val = val * 9 / 5 + 32

                    max_temps.append(val)
                except Exception:
                    continue

            if max_temps:
                return HRRRForecast(
                    max_temp_f=float(np.max(max_temps)),
                    init_time=init_time.isoformat(),
                    valid_hours=len(max_temps),
                    lat=lat,
                    lon=lon,
                )

        except Exception as e:
            logger.debug("HRRR init=%s failed: %s", init_time.isoformat(), e)
            continue

    return None


def is_same_day(target_date: str) -> bool:
    """Check if target_date is today (UTC)."""
    today = datetime.now(timezone.utc).strftime("%Y-%m-%d")
    return target_date == today
