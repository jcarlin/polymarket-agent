"""Tests for weather probability model."""

import numpy as np
from weather.open_meteo import EnsembleForecast
from weather.probability_model import (
    compute_bucket_probabilities,
)


def _make_forecast(members: list[float]) -> EnsembleForecast:
    """Helper to create a forecast with given member temps."""
    n_gefs = min(31, len(members))
    return EnsembleForecast(
        city="NYC",
        station_icao="KLGA",
        forecast_date="2026-01-15",
        gefs_daily_max=members[:n_gefs],
        ecmwf_daily_max=members[n_gefs:],
        all_members=members,
    )


def test_bucket_probabilities_sum_to_one():
    """KDE-derived probabilities should sum to ~1.0."""
    # 82 members clustered around 75F
    rng = np.random.default_rng(42)
    members = list(rng.normal(75, 3, 82))
    forecast = _make_forecast(members)
    result = compute_bucket_probabilities(forecast)
    total = sum(b.probability for b in result.buckets)
    assert abs(total - 1.0) < 0.01, f"Total probability {total} should be ~1.0"


def test_empty_members():
    """Empty ensemble should return empty buckets."""
    forecast = _make_forecast([])
    result = compute_bucket_probabilities(forecast)
    assert result.buckets == []
    assert result.ensemble_mean == 0.0


def test_single_member():
    """Single member should use histogram fallback."""
    forecast = _make_forecast([75.0])
    result = compute_bucket_probabilities(forecast)
    # Should have exactly one bucket with probability 1.0
    nonzero = [b for b in result.buckets if b.probability > 0]
    assert len(nonzero) == 1
    assert abs(nonzero[0].probability - 1.0) < 0.01


def test_spread_correction():
    """Spread correction should widen the distribution."""
    members = [74.0, 75.0, 76.0] * 10  # 30 members, tight cluster
    forecast = _make_forecast(members)

    result_no_correction = compute_bucket_probabilities(forecast, spread_correction=1.0)
    result_with_correction = compute_bucket_probabilities(forecast, spread_correction=1.5)

    # With correction, the std should be wider (more buckets with probability)
    nonzero_no = len([b for b in result_no_correction.buckets if b.probability > 0.01])
    nonzero_with = len([b for b in result_with_correction.buckets if b.probability > 0.01])
    assert nonzero_with >= nonzero_no


def test_bucket_labels():
    """Bucket labels should be formatted correctly."""
    rng = np.random.default_rng(42)
    members = list(rng.normal(75, 2, 50))
    forecast = _make_forecast(members)
    result = compute_bucket_probabilities(forecast, bucket_width=2.0)
    for b in result.buckets:
        assert "-" in b.bucket_label
        parts = b.bucket_label.split("-")
        assert len(parts) == 2
        assert float(parts[0]) == b.lower
        assert float(parts[1]) == b.upper


def test_bucket_width():
    """Custom bucket width should be respected."""
    rng = np.random.default_rng(42)
    members = list(rng.normal(75, 3, 50))
    forecast = _make_forecast(members)
    result = compute_bucket_probabilities(forecast, bucket_width=4.0)
    for b in result.buckets:
        assert abs((b.upper - b.lower) - 4.0) < 0.01


def test_non_negative_probabilities():
    """All probabilities must be >= 0."""
    rng = np.random.default_rng(42)
    members = list(rng.normal(75, 5, 82))
    forecast = _make_forecast(members)
    result = compute_bucket_probabilities(forecast)
    for b in result.buckets:
        assert b.probability >= 0.0


def test_bimodal_distribution():
    """Bimodal distribution should have probabilities in both modes."""
    rng = np.random.default_rng(42)
    members = list(rng.normal(60, 2, 41)) + list(rng.normal(80, 2, 41))
    forecast = _make_forecast(members)
    result = compute_bucket_probabilities(forecast)
    # Find probability mass around 60 and 80
    mass_60 = sum(b.probability for b in result.buckets if 58 <= b.lower <= 64)
    mass_80 = sum(b.probability for b in result.buckets if 78 <= b.lower <= 84)
    assert mass_60 > 0.1, f"Expected mass around 60, got {mass_60}"
    assert mass_80 > 0.1, f"Expected mass around 80, got {mass_80}"


def test_metadata_populated():
    """Result should have correct metadata."""
    gefs = [70.0 + i * 0.5 for i in range(31)]
    ecmwf = [71.0 + i * 0.3 for i in range(51)]
    forecast = EnsembleForecast(
        city="CHI",
        station_icao="KORD",
        forecast_date="2026-02-20",
        gefs_daily_max=gefs,
        ecmwf_daily_max=ecmwf,
        all_members=gefs + ecmwf,
    )
    result = compute_bucket_probabilities(forecast)
    assert result.city == "CHI"
    assert result.station_icao == "KORD"
    assert result.forecast_date == "2026-02-20"
    assert result.gefs_count == 31
    assert result.ecmwf_count == 51
    assert result.ensemble_std > 0


def test_histogram_fallback_with_few_members():
    """Less than 5 members should use histogram fallback."""
    members = [72.0, 74.0, 76.0, 78.0]  # 4 members
    forecast = _make_forecast(members)
    result = compute_bucket_probabilities(forecast)
    nonzero = [b for b in result.buckets if b.probability > 0]
    assert len(nonzero) > 0
    total = sum(b.probability for b in result.buckets)
    assert abs(total - 1.0) < 0.01


def test_nws_anchor_shifts_mean():
    """NWS anchor should shift ensemble mean to match the anchor value."""
    # Members centered around 32°F (raw cold bias)
    members = [30.0 + i * 0.2 for i in range(82)]  # ~32°F mean
    forecast = _make_forecast(members)
    raw_mean = np.mean(members)

    # Anchor to NWS forecast of 37°F
    result = compute_bucket_probabilities(forecast, nws_anchor=37.0)

    # Mean should now be close to 37, not 32
    assert abs(result.ensemble_mean - 37.0) < 0.5, (
        f"Expected mean ~37.0 after NWS anchor, got {result.ensemble_mean}"
    )
    assert result.nws_forecast_high == 37.0
    assert result.bias_correction is not None
    assert abs(result.bias_correction - (37.0 - raw_mean)) < 0.1


def test_nws_anchor_preserves_spread():
    """NWS anchor should preserve ensemble spread (only shift, not scale)."""
    rng = np.random.default_rng(42)
    members = list(rng.normal(32, 3, 82))
    forecast = _make_forecast(members)

    result_raw = compute_bucket_probabilities(forecast)
    result_anchored = compute_bucket_probabilities(forecast, nws_anchor=37.0)

    # Std dev should be identical (shift doesn't change spread)
    assert abs(result_raw.ensemble_std - result_anchored.ensemble_std) < 0.01


def test_nws_anchor_none_no_shift():
    """When nws_anchor is None, no shift should be applied."""
    rng = np.random.default_rng(42)
    members = list(rng.normal(75, 3, 82))
    forecast = _make_forecast(members)

    result = compute_bucket_probabilities(forecast, nws_anchor=None)
    assert result.nws_forecast_high is None
    assert result.bias_correction is None
    assert abs(result.ensemble_mean - np.mean(members)) < 0.1


def test_nws_anchor_with_spread_correction():
    """NWS anchor and spread correction should compose correctly."""
    members = [30.0 + i * 0.2 for i in range(82)]  # ~32°F mean, tight
    forecast = _make_forecast(members)

    # Apply both: shift to 37°F AND widen spread by 1.3x
    result = compute_bucket_probabilities(
        forecast, spread_correction=1.3, nws_anchor=37.0
    )

    # Mean should be near 37
    assert abs(result.ensemble_mean - 37.0) < 0.5
    # Probabilities should still sum to ~1
    total = sum(b.probability for b in result.buckets)
    assert abs(total - 1.0) < 0.01
    # NWS fields should be populated
    assert result.nws_forecast_high == 37.0
    assert result.bias_correction is not None


def test_nws_anchor_probabilities_sum_to_one():
    """After NWS anchor shift, probabilities should still sum to ~1.0."""
    rng = np.random.default_rng(42)
    members = list(rng.normal(30, 4, 82))
    forecast = _make_forecast(members)

    result = compute_bucket_probabilities(forecast, nws_anchor=45.0)
    total = sum(b.probability for b in result.buckets)
    assert abs(total - 1.0) < 0.01, f"Total probability {total} should be ~1.0"
