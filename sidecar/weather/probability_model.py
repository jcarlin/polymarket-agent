"""Gaussian KDE probability model for weather ensemble forecasts."""

import logging
from dataclasses import dataclass, field

import numpy as np

logger = logging.getLogger("weather.probability_model")


@dataclass
class BucketProbability:
    bucket_label: str  # e.g., "72-74"
    lower: float
    upper: float
    probability: float


@dataclass
class WeatherProbabilities:
    city: str
    station_icao: str
    forecast_date: str
    buckets: list[BucketProbability] = field(default_factory=list)
    ensemble_mean: float = 0.0
    ensemble_std: float = 0.0
    gefs_count: int = 0
    ecmwf_count: int = 0
    spread_correction: float = 1.0
    nws_forecast_high: float | None = None
    bias_correction: float | None = None


def compute_bucket_probabilities(
    forecast,  # EnsembleForecast
    bucket_range: tuple[float, float] = (0, 130),
    bucket_width: float = 2.0,
    spread_correction: float = 1.0,
    nws_anchor: float | None = None,
) -> WeatherProbabilities:
    """Convert ensemble member temperatures to bucket probabilities using Gaussian KDE.

    Args:
        forecast: EnsembleForecast with all_members temperatures
        bucket_range: Min/max temperature range for buckets
        bucket_width: Width of each bucket in °F
        spread_correction: Multiplicative spread adjustment (>1 widens, <1 narrows)
        nws_anchor: NWS official forecast high in °F. If provided, shifts the entire
            ensemble distribution to center on this value before applying spread
            correction and KDE. Fixes the ~5°F cold bias in raw ensemble output.
    """
    members = np.array(forecast.all_members, dtype=np.float64)

    if len(members) == 0:
        logger.warning("No ensemble members for %s", forecast.city)
        return WeatherProbabilities(
            city=forecast.city,
            station_icao=forecast.station_icao,
            forecast_date=forecast.forecast_date,
        )

    raw_mean = float(np.mean(members))
    std = float(np.std(members)) if len(members) > 1 else 0.0

    # Step 1: Apply NWS anchor bias correction (shift mean to match NWS forecast)
    bias_correction: float | None = None
    if nws_anchor is not None:
        bias_correction = nws_anchor - raw_mean
        members = members + bias_correction
        logger.info(
            "Applied NWS anchor for %s: raw_mean=%.1f, nws=%.1f, shift=%+.1f",
            forecast.city, raw_mean, nws_anchor, bias_correction,
        )

    corrected_mean = float(np.mean(members))

    # Step 2: Apply spread correction: corrected = mean + (val - mean) * factor
    if spread_correction != 1.0:
        corrected = corrected_mean + (members - corrected_mean) * spread_correction
    else:
        corrected = members

    # Build buckets
    buckets: list[BucketProbability] = []
    lower = bucket_range[0]
    while lower < bucket_range[1]:
        upper = lower + bucket_width
        label = f"{int(lower)}-{int(upper)}"
        buckets.append(BucketProbability(
            bucket_label=label,
            lower=lower,
            upper=upper,
            probability=0.0,
        ))
        lower = upper

    # Use KDE if enough members, else histogram fallback
    if len(corrected) >= 5:
        try:
            from scipy.stats import gaussian_kde
            from scipy.integrate import quad

            kde = gaussian_kde(corrected, bw_method="silverman")
            # Integrate KDE over each bucket using quadrature
            total = 0.0
            for bucket in buckets:
                prob, _ = quad(kde, bucket.lower, bucket.upper)
                bucket.probability = max(0.0, prob)
                total += bucket.probability
            # Normalize to sum to 1.0
            if total > 0:
                for bucket in buckets:
                    bucket.probability /= total
        except Exception as e:
            logger.warning("KDE failed, falling back to histogram: %s", e)
            _histogram_fallback(corrected, buckets)
    else:
        _histogram_fallback(corrected, buckets)

    # Filter to only buckets with non-negligible probability
    significant_buckets = [b for b in buckets if b.probability > 1e-6]

    return WeatherProbabilities(
        city=forecast.city,
        station_icao=forecast.station_icao,
        forecast_date=forecast.forecast_date,
        buckets=significant_buckets,
        ensemble_mean=corrected_mean,
        ensemble_std=std,
        gefs_count=len(forecast.gefs_daily_max),
        ecmwf_count=len(forecast.ecmwf_daily_max),
        spread_correction=spread_correction,
        nws_forecast_high=nws_anchor,
        bias_correction=bias_correction,
    )


def _histogram_fallback(values: np.ndarray, buckets: list[BucketProbability]) -> None:
    """Simple histogram fallback when KDE is not feasible."""
    n = len(values)
    if n == 0:
        return
    for bucket in buckets:
        count = np.sum((values >= bucket.lower) & (values < bucket.upper))
        bucket.probability = float(count) / n
