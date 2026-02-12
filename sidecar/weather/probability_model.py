"""Gaussian KDE probability model for weather ensemble forecasts."""

import logging
from dataclasses import dataclass, field
from typing import Optional

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
    icon_count: int = 0
    gem_count: int = 0
    total_members: int = 0
    spread_correction: float = 1.0
    nws_forecast_high: Optional[float] = None
    bias_correction: float = 0.0
    raw_ensemble_mean: float = 0.0
    hrrr_max_temp: Optional[float] = None
    hrrr_shift: float = 0.0
    nbm_max_temp: Optional[float] = None
    nbm_percentiles: Optional[dict] = None
    calibration_bias: Optional[float] = None
    calibration_spread: Optional[float] = None
    wu_forecast_high: Optional[float] = None
    wu_forecast_shift: float = 0.0


def compute_bucket_probabilities(
    forecast,  # EnsembleForecast
    bucket_range: tuple[float, float] = (0, 130),
    bucket_width: float = 2.0,
    spread_correction: float = 1.0,
    nws_high: Optional[float] = None,
    nws_weight: float = 0.85,
    hrrr_max: Optional[float] = None,
    hrrr_weight: float = 0.3,
    calibration_bias: Optional[float] = None,
    calibration_spread: Optional[float] = None,
    wu_forecast_high: Optional[float] = None,
    wu_forecast_weight: float = 0.5,
) -> WeatherProbabilities:
    """Convert ensemble member temperatures to bucket probabilities using Gaussian KDE."""
    members = np.array(forecast.all_members, dtype=np.float64)

    if len(members) == 0:
        logger.warning("No ensemble members for %s", forecast.city)
        return WeatherProbabilities(
            city=forecast.city,
            station_icao=forecast.station_icao,
            forecast_date=forecast.forecast_date,
        )

    raw_mean = float(np.mean(members))

    # NWS bias correction: weighted blend (not 100% override)
    bias_correction = 0.0
    if nws_high is not None:
        target = nws_weight * nws_high + (1 - nws_weight) * raw_mean
        bias_correction = target - raw_mean
        members = members + bias_correction
        logger.info("NWS bias correction: shift=%.1f°F (raw_mean=%.1f, nws=%.1f, weight=%.1f, target=%.1f)",
                     bias_correction, raw_mean, nws_high, nws_weight, target)

    # Calibration bias: shift toward WU resolution source (applied after NWS, before HRRR)
    if calibration_bias is not None and calibration_bias != 0.0:
        members = members + calibration_bias
        logger.info("Calibration bias correction: +%.1f°F", calibration_bias)

    # HRRR anchoring: nudge distribution toward HRRR deterministic forecast
    hrrr_shift = 0.0
    if hrrr_max is not None and hrrr_weight > 0:
        corrected_mean = float(np.mean(members))
        hrrr_shift = hrrr_weight * (hrrr_max - corrected_mean)
        members = members + hrrr_shift
        logger.info("HRRR anchoring: shift=%.1f°F (weight=%.1f, hrrr=%.1f, mean=%.1f)",
                     hrrr_shift, hrrr_weight, hrrr_max, corrected_mean)

    # WU forecast anchoring: nudge distribution toward WU's own forecast (resolution source)
    wu_fcst_shift = 0.0
    if wu_forecast_high is not None and wu_forecast_weight > 0:
        corrected_mean = float(np.mean(members))
        wu_fcst_shift = wu_forecast_weight * (wu_forecast_high - corrected_mean)
        members = members + wu_fcst_shift
        logger.info("WU forecast anchoring: shift=%.1f°F (weight=%.1f, wu_fcst=%.1f, mean=%.1f)",
                     wu_fcst_shift, wu_forecast_weight, wu_forecast_high, corrected_mean)

    mean = float(np.mean(members))  # corrected mean
    std = float(np.std(members)) if len(members) > 1 else 0.0

    # Apply spread correction: corrected = mean + (val - mean) * factor
    effective_spread = spread_correction
    if calibration_spread is not None:
        effective_spread *= calibration_spread
    if effective_spread != 1.0:
        corrected = mean + (members - mean) * effective_spread
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

    icon_count = len(getattr(forecast, 'icon_daily_max', []))
    gem_count = len(getattr(forecast, 'gem_daily_max', []))

    return WeatherProbabilities(
        city=forecast.city,
        station_icao=forecast.station_icao,
        forecast_date=forecast.forecast_date,
        buckets=significant_buckets,
        ensemble_mean=mean,
        ensemble_std=std,
        gefs_count=len(forecast.gefs_daily_max),
        ecmwf_count=len(forecast.ecmwf_daily_max),
        icon_count=icon_count,
        gem_count=gem_count,
        total_members=len(forecast.all_members),
        spread_correction=spread_correction,
        nws_forecast_high=nws_high,
        bias_correction=bias_correction,
        raw_ensemble_mean=raw_mean,
        hrrr_max_temp=hrrr_max,
        hrrr_shift=hrrr_shift,
        calibration_bias=calibration_bias,
        calibration_spread=calibration_spread,
        wu_forecast_high=wu_forecast_high,
        wu_forecast_shift=wu_fcst_shift,
    )


def _histogram_fallback(values: np.ndarray, buckets: list[BucketProbability]) -> None:
    """Simple histogram fallback when KDE is not feasible."""
    n = len(values)
    if n == 0:
        return
    for bucket in buckets:
        count = np.sum((values >= bucket.lower) & (values < bucket.upper))
        bucket.probability = float(count) / n


def blend_probabilities(
    ensemble_buckets: list[BucketProbability],
    nbm_buckets: list[tuple[float, float, float]],
    nbm_weight: float = 0.6,
) -> list[BucketProbability]:
    """Blend ensemble KDE buckets with NBM-derived bucket probabilities.

    Args:
        ensemble_buckets: Bucket probabilities from ensemble KDE
        nbm_buckets: List of (lower, upper, probability) from NBM model
        nbm_weight: Weight for NBM probabilities (0-1, default 0.6)

    Returns:
        Blended bucket probabilities
    """
    if not nbm_buckets or nbm_weight <= 0:
        return ensemble_buckets

    # Build NBM probability lookup by (lower, upper) range
    nbm_lookup: dict[tuple[float, float], float] = {}
    for lower, upper, prob in nbm_buckets:
        nbm_lookup[(lower, upper)] = prob

    ensemble_weight = 1.0 - nbm_weight
    blended: list[BucketProbability] = []
    total = 0.0

    for bucket in ensemble_buckets:
        key = (bucket.lower, bucket.upper)
        nbm_prob = nbm_lookup.get(key, 0.0)
        blended_prob = ensemble_weight * bucket.probability + nbm_weight * nbm_prob
        blended.append(BucketProbability(
            bucket_label=bucket.bucket_label,
            lower=bucket.lower,
            upper=bucket.upper,
            probability=blended_prob,
        ))
        total += blended_prob

    # Normalize
    if total > 0:
        for b in blended:
            b.probability /= total

    # Filter negligible
    return [b for b in blended if b.probability > 1e-6]
