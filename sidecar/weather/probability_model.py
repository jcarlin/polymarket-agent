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
    gem_count: int = 0
    icon_count: int = 0
    spread_correction: float = 1.0
    nws_forecast_high: float | None = None
    bias_correction: float | None = None
    nbm_p50: float | None = None
    anchor_source: str = "raw"  # "hrrr", "nbm", "nws", or "raw"


def compute_bucket_probabilities(
    forecast,  # EnsembleForecast
    bucket_range: tuple[float, float] = (0, 130),
    bucket_width: float = 2.0,
    spread_correction: float = 1.0,
    nws_anchor: float | None = None,
    nbm_anchor: float | None = None,
    nbm_spread: float | None = None,
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
        nbm_anchor: NBM p50 temperature in °F. If provided, takes priority over
            nws_anchor for mean-shift correction (NBM is better calibrated).
        nbm_spread: NBM-derived spread correction factor. If provided, overrides
            the static spread_correction parameter.
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

    # Step 1: Apply anchor bias correction (NBM > NWS > none)
    # Fallback chain: NBM anchor (best) → NWS anchor → raw (no correction)
    bias_correction: float | None = None
    anchor_source = "raw"
    effective_anchor: float | None = None
    nbm_p50: float | None = nbm_anchor

    if nbm_anchor is not None:
        effective_anchor = nbm_anchor
        anchor_source = "nbm"
    elif nws_anchor is not None:
        effective_anchor = nws_anchor
        anchor_source = "nws"

    if effective_anchor is not None:
        bias_correction = effective_anchor - raw_mean
        members = members + bias_correction
        logger.info(
            "Applied %s anchor for %s: raw_mean=%.1f, anchor=%.1f, shift=%+.1f",
            anchor_source, forecast.city, raw_mean, effective_anchor, bias_correction,
        )

    corrected_mean = float(np.mean(members))

    # Step 2: Apply spread correction
    # NBM-derived spread takes priority over static config value
    effective_spread = nbm_spread if nbm_spread is not None else spread_correction
    if effective_spread != 1.0:
        corrected = corrected_mean + (members - corrected_mean) * effective_spread
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

    gem_count = len(forecast.gem_daily_max) if hasattr(forecast, "gem_daily_max") else 0
    icon_count = len(forecast.icon_daily_max) if hasattr(forecast, "icon_daily_max") else 0

    return WeatherProbabilities(
        city=forecast.city,
        station_icao=forecast.station_icao,
        forecast_date=forecast.forecast_date,
        buckets=significant_buckets,
        ensemble_mean=corrected_mean,
        ensemble_std=std,
        gefs_count=len(forecast.gefs_daily_max),
        ecmwf_count=len(forecast.ecmwf_daily_max),
        gem_count=gem_count,
        icon_count=icon_count,
        spread_correction=effective_spread,
        nws_forecast_high=nws_anchor,
        bias_correction=bias_correction,
        nbm_p50=nbm_p50,
        anchor_source=anchor_source,
    )


def _histogram_fallback(values: np.ndarray, buckets: list[BucketProbability]) -> None:
    """Simple histogram fallback when KDE is not feasible."""
    n = len(values)
    if n == 0:
        return
    for bucket in buckets:
        count = np.sum((values >= bucket.lower) & (values < bucket.upper))
        bucket.probability = float(count) / n
