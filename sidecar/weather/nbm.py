"""NBM (National Blend of Models) fetcher for bias-corrected temperature forecasts.

NBM is NOAA's gold-standard post-processed forecast: a multi-model blend at 2.5km
resolution with probabilistic output. It fixes both mean bias AND spread simultaneously.

Uses the Herbie library to download GRIB2 from AWS S3. Falls back gracefully if
Herbie/cfgrib are not installed (exotic system dependencies).
"""

import logging
from dataclasses import dataclass
from datetime import datetime, timedelta
from typing import Optional

logger = logging.getLogger("weather.nbm")

# Track whether Herbie is available
_HERBIE_AVAILABLE = False
try:
    from herbie import Herbie  # noqa: F401

    _HERBIE_AVAILABLE = True
    logger.info("Herbie library available — NBM fetching enabled")
except ImportError:
    logger.info("Herbie library not available — NBM fetching disabled, will fall back to NWS")


@dataclass
class NBMPercentiles:
    """NBM temperature percentiles at a grid point."""

    p10: float
    p25: float
    p50: float  # median — best single-point estimate
    p75: float
    p90: float
    lat: float
    lon: float
    init_time: str  # ISO format of NBM initialization time


def is_available() -> bool:
    """Check if NBM fetching is available (Herbie installed)."""
    return _HERBIE_AVAILABLE


def fetch_nbm_percentiles(
    lat: float,
    lon: float,
    target_date: str,
    max_retries: int = 2,
) -> Optional[NBMPercentiles]:
    """Fetch NBM temperature max percentiles for a location and date.

    Downloads the latest NBM run from AWS S3 via Herbie, extracts MaxT
    percentiles at the nearest grid point.

    Args:
        lat: Latitude
        lon: Longitude
        target_date: YYYY-MM-DD format
        max_retries: Number of retry attempts

    Returns:
        NBMPercentiles with p10/p25/p50/p75/p90, or None on failure.
    """
    if not _HERBIE_AVAILABLE:
        return None

    try:
        return _fetch_nbm_herbie(lat, lon, target_date, max_retries)
    except Exception as e:
        logger.warning("NBM fetch failed for %.4f,%.4f on %s: %s", lat, lon, target_date, e)
        return None


def _fetch_nbm_herbie(
    lat: float,
    lon: float,
    target_date: str,
    max_retries: int,
) -> Optional[NBMPercentiles]:
    """Internal: fetch NBM via Herbie with retries."""
    from herbie import Herbie
    import numpy as np

    target = datetime.strptime(target_date, "%Y-%m-%d")
    # Use the most recent NBM run (usually ~6h behind real-time)
    # Try the latest 00Z run for the target date, fall back to prior day 12Z
    init_times = [
        target.replace(hour=0),
        (target - timedelta(days=1)).replace(hour=12),
        (target - timedelta(days=1)).replace(hour=0),
    ]

    for attempt_idx, init_time in enumerate(init_times):
        try:
            # Forecast hour: hours from init to end of target day
            fxx = int((target.replace(hour=23) - init_time).total_seconds() / 3600)
            fxx = max(1, min(fxx, 264))  # NBM goes out to 264h

            H = Herbie(
                init_time.strftime("%Y-%m-%d %H:%M"),
                model="nbm",
                product="co",
                fxx=fxx,
                verbose=False,
            )

            # Search for MaxT (maximum temperature) variable
            ds = H.xarray(":TMAX:")

            # Find nearest grid point
            if hasattr(ds, "latitude") and hasattr(ds, "longitude"):
                point = ds.sel(
                    latitude=lat,
                    longitude=lon % 360,  # NBM may use 0-360 lon
                    method="nearest",
                )
            else:
                # Try alternative coordinate names
                point = ds.sel(y=lat, x=lon, method="nearest")

            # Extract percentiles from ensemble or probability fields
            # NBM provides percentile fields directly
            vals = point.values if hasattr(point, "values") else np.array([point])
            vals_flat = vals.flatten()

            if len(vals_flat) >= 5:
                # If we have multiple percentile values
                percentiles = np.percentile(vals_flat, [10, 25, 50, 75, 90])
            elif len(vals_flat) >= 1:
                # Single deterministic value — synthesize spread
                median = float(vals_flat[0])
                # Convert K to F if needed (NBM can be in Kelvin)
                if median > 200:  # Kelvin
                    median = (median - 273.15) * 9 / 5 + 32
                # Assume ~3°F spread for synthetic percentiles
                spread = 3.0
                percentiles = [
                    median - 1.5 * spread,
                    median - 0.5 * spread,
                    median,
                    median + 0.5 * spread,
                    median + 1.5 * spread,
                ]
            else:
                continue

            # Convert to °F if in Celsius (values below 60 likely Celsius)
            p50 = float(percentiles[2])
            if -50 < p50 < 60:
                # Likely Celsius
                percentiles = [(v * 9 / 5 + 32) for v in percentiles]

            return NBMPercentiles(
                p10=float(percentiles[0]),
                p25=float(percentiles[1]),
                p50=float(percentiles[2]),
                p75=float(percentiles[3]),
                p90=float(percentiles[4]),
                lat=lat,
                lon=lon,
                init_time=init_time.isoformat(),
            )

        except Exception as e:
            logger.debug(
                "NBM attempt %d (init=%s) failed: %s",
                attempt_idx + 1, init_time.isoformat(), e,
            )
            continue

    return None


def nbm_anchor_and_spread(
    nbm: NBMPercentiles,
    raw_ensemble_std: float,
) -> tuple[float, float]:
    """Compute anchor temperature and spread correction from NBM percentiles.

    Returns:
        (anchor_temp_f, spread_correction_factor)
        - anchor_temp_f: Use as mean-shift anchor (replaces NWS point forecast)
        - spread_correction_factor: Multiply raw ensemble spread by this
    """
    import numpy as np
    from scipy.stats import norm

    # Use p50 (median) as the anchor — most robust central estimate
    anchor = nbm.p50

    # Estimate NBM sigma from percentile spread
    # p90 - p10 spans ~2.56 sigma in a normal distribution
    nbm_range = nbm.p90 - nbm.p10
    nbm_sigma = nbm_range / (2 * norm.ppf(0.9))  # ~2.56

    # Spread correction = NBM sigma / raw ensemble sigma
    if raw_ensemble_std > 0.1:
        spread_correction = nbm_sigma / raw_ensemble_std
        # Clamp to reasonable range
        spread_correction = max(0.5, min(spread_correction, 3.0))
    else:
        spread_correction = 1.3  # Default fallback

    return anchor, spread_correction
