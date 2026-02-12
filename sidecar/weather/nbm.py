"""NBM (National Blend of Models) integration for weather percentile forecasts."""

import logging
from dataclasses import dataclass
from typing import Optional

import numpy as np
from scipy.optimize import minimize
from scipy.stats import norm

logger = logging.getLogger("weather.nbm")


@dataclass
class NBMForecast:
    city: str
    date: str
    p10: float
    p25: float
    p50: float
    p75: float
    p90: float
    max_temp: float  # point forecast (median or mean)


async def fetch_nbm_percentiles(
    city: str,
    date: str,
) -> Optional[NBMForecast]:
    """Fetch NBM MaxT percentiles for a city and date using herbie-data.

    Returns None if herbie is not installed or if the fetch fails.
    The city parameter should be a key from CITY_CONFIGS (e.g. "NYC").
    """
    try:
        from herbie import Herbie  # type: ignore[import-untyped]
    except ImportError:
        logger.warning(
            "herbie-data not installed; NBM percentiles unavailable. "
            "Install with: pip install herbie-data"
        )
        return None

    try:
        from weather.open_meteo import CITY_CONFIGS

        config = CITY_CONFIGS.get(city)
        if config is None:
            logger.warning("Unknown city for NBM: %s", city)
            return None

        lat, lon = config.lat, config.lon

        # Fetch NBM GRIB2 for MaxT
        h = Herbie(date, model="nbm", product="co", fxx=24)
        ds = h.xarray("TMAX:surface")

        # Find nearest grid point
        temps = ds.sel(latitude=lat, longitude=lon + 360, method="nearest")
        max_temp = float(temps["tmax"].values)

        # NBM provides percentile fields -- try to extract them
        percentile_values = {}
        for pct, search_str in [
            (10, "TMAX:surface:prob <"),
            (25, "TMAX:surface:prob <"),
            (50, "TMAX:surface:prob <"),
            (75, "TMAX:surface:prob <"),
            (90, "TMAX:surface:prob <"),
        ]:
            try:
                ds_pct = h.xarray(search_str)
                val = float(ds_pct.sel(
                    latitude=lat, longitude=lon + 360, method="nearest"
                ).values)
                percentile_values[pct] = val
            except Exception:
                percentile_values[pct] = max_temp  # fallback to point forecast

        return NBMForecast(
            city=city,
            date=date,
            p10=percentile_values.get(10, max_temp - 5),
            p25=percentile_values.get(25, max_temp - 2),
            p50=percentile_values.get(50, max_temp),
            p75=percentile_values.get(75, max_temp + 2),
            p90=percentile_values.get(90, max_temp + 5),
            max_temp=max_temp,
        )
    except Exception as e:
        logger.warning("NBM fetch failed for %s on %s: %s", city, date, e)
        return None


def nbm_percentiles_to_buckets(
    nbm: NBMForecast,
    bucket_range: tuple[float, float] = (0, 130),
    bucket_width: float = 2.0,
) -> list[tuple[float, float, float]]:
    """Convert NBM percentile forecast to 2-degree-F bucket probabilities.

    Fits a normal distribution to the 5 percentile points (p10, p25, p50, p75, p90)
    using least-squares optimization, then integrates over each bucket.

    Returns:
        List of (lower_bound, upper_bound, probability) tuples.
    """
    # Known percentile points
    percentiles = np.array([0.10, 0.25, 0.50, 0.75, 0.90])
    observed = np.array([nbm.p10, nbm.p25, nbm.p50, nbm.p75, nbm.p90])

    # Fit normal distribution: find mu and sigma that minimize squared error
    # between observed percentile values and theoretical quantiles
    def objective(params: np.ndarray) -> float:
        mu, log_sigma = params
        sigma = np.exp(log_sigma)  # ensure sigma > 0
        theoretical = norm.ppf(percentiles, loc=mu, scale=sigma)
        return float(np.sum((theoretical - observed) ** 2))

    # Initial guess: median for mu, IQR-based estimate for sigma
    mu0 = nbm.p50
    iqr = nbm.p75 - nbm.p25
    sigma0 = max(iqr / 1.349, 0.5)  # 1.349 = IQR of standard normal

    result = minimize(
        objective,
        x0=np.array([mu0, np.log(sigma0)]),
        method="Nelder-Mead",
    )
    mu_fit = result.x[0]
    sigma_fit = np.exp(result.x[1])

    # Generate bucket probabilities
    buckets: list[tuple[float, float, float]] = []
    lower = bucket_range[0]
    while lower < bucket_range[1]:
        upper = lower + bucket_width
        prob = float(norm.cdf(upper, loc=mu_fit, scale=sigma_fit)
                     - norm.cdf(lower, loc=mu_fit, scale=sigma_fit))
        buckets.append((lower, upper, max(0.0, prob)))
        lower = upper

    return buckets
