"""Tests for weather probability model."""

import numpy as np
from weather.open_meteo import EnsembleForecast
from weather.probability_model import (
    blend_probabilities,
    BucketProbability,
    compute_bucket_probabilities,
)


def _make_forecast(
    members: list[float],
    icon: list[float] | None = None,
    gem: list[float] | None = None,
) -> EnsembleForecast:
    """Helper to create a forecast with given member temps."""
    n_gefs = min(31, len(members))
    icon_list = icon or []
    gem_list = gem or []
    all_members = members + icon_list + gem_list
    return EnsembleForecast(
        city="NYC",
        station_icao="KLGA",
        forecast_date="2026-01-15",
        gefs_daily_max=members[:n_gefs],
        ecmwf_daily_max=members[n_gefs:],
        icon_daily_max=icon_list,
        gem_daily_max=gem_list,
        all_members=all_members,
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


def test_nws_bias_correction():
    """NWS bias correction should shift ensemble mean between raw and NWS (weighted blend)."""
    rng = np.random.default_rng(42)
    members = list(rng.normal(30.0, 3.0, 82))
    forecast = _make_forecast(members)
    raw_mean = float(np.mean(members))
    nws = 37.0
    # Default weight=0.85: target = 0.85*37 + 0.15*raw_mean
    result = compute_bucket_probabilities(forecast, nws_high=nws)
    expected_target = 0.85 * nws + 0.15 * raw_mean
    assert abs(result.ensemble_mean - expected_target) < 0.5, (
        f"Corrected ensemble_mean {result.ensemble_mean} should be ~{expected_target:.1f} (weighted blend)"
    )
    # ensemble_mean should be between raw and NWS, not equal to NWS
    assert result.ensemble_mean > raw_mean, "Should shift toward NWS"
    assert result.ensemble_mean < nws, "Should not fully match NWS (weighted blend)"
    assert abs(result.raw_ensemble_mean - 30.0) < 0.5
    assert result.nws_forecast_high == 37.0


def test_nws_none_no_shift():
    """When nws_high is None, no bias correction should be applied."""
    rng = np.random.default_rng(42)
    members = list(rng.normal(30.0, 3.0, 82))
    forecast = _make_forecast(members)
    result = compute_bucket_probabilities(forecast, nws_high=None)
    assert result.bias_correction == 0.0
    assert result.nws_forecast_high is None
    assert abs(result.ensemble_mean - result.raw_ensemble_mean) < 1e-9, (
        "ensemble_mean should equal raw_ensemble_mean when no NWS correction"
    )


def test_hrrr_anchoring_shifts_mean():
    """HRRR anchoring should shift distribution toward HRRR point estimate."""
    rng = np.random.default_rng(42)
    members = list(rng.normal(40.0, 3.0, 82))
    forecast = _make_forecast(members)

    result_no_hrrr = compute_bucket_probabilities(forecast)
    result_with_hrrr = compute_bucket_probabilities(forecast, hrrr_max=50.0, hrrr_weight=0.3)

    # With HRRR=50 and weight=0.3, mean should shift toward 50 by ~30% of the gap
    assert result_with_hrrr.ensemble_mean > result_no_hrrr.ensemble_mean, (
        f"HRRR should shift mean upward: {result_with_hrrr.ensemble_mean} vs {result_no_hrrr.ensemble_mean}"
    )
    assert result_with_hrrr.hrrr_max_temp == 50.0
    assert result_with_hrrr.hrrr_shift > 0


def test_hrrr_weight_zero_no_shift():
    """HRRR with weight=0 should not shift the distribution."""
    rng = np.random.default_rng(42)
    members = list(rng.normal(40.0, 3.0, 82))
    forecast = _make_forecast(members)

    result_no_hrrr = compute_bucket_probabilities(forecast)
    result_zero_weight = compute_bucket_probabilities(forecast, hrrr_max=50.0, hrrr_weight=0.0)

    assert abs(result_zero_weight.ensemble_mean - result_no_hrrr.ensemble_mean) < 1e-9
    assert result_zero_weight.hrrr_shift == 0.0


def test_multi_model_ensemble_sum_to_one():
    """Ensemble with ICON + GEM (>100 members) should still sum to ~1.0."""
    rng = np.random.default_rng(42)
    gefs = list(rng.normal(75, 3, 31))
    ecmwf = list(rng.normal(75, 3, 51))
    icon = list(rng.normal(75, 3, 40))
    gem = list(rng.normal(75, 3, 21))

    forecast = _make_forecast(gefs + ecmwf, icon=icon, gem=gem)
    result = compute_bucket_probabilities(forecast)

    total = sum(b.probability for b in result.buckets)
    assert abs(total - 1.0) < 0.01, f"Total probability {total} should be ~1.0"
    assert result.total_members == 143  # 31+51+40+21
    assert result.icon_count == 40
    assert result.gem_count == 21


def test_multi_model_metadata():
    """Multi-model forecast should report correct member counts."""
    gefs = [70.0 + i * 0.5 for i in range(31)]
    ecmwf = [71.0 + i * 0.3 for i in range(51)]
    icon = [72.0 + i * 0.4 for i in range(40)]
    gem = [73.0 + i * 0.6 for i in range(21)]

    forecast = _make_forecast(gefs + ecmwf, icon=icon, gem=gem)
    result = compute_bucket_probabilities(forecast)

    assert result.gefs_count == 31
    assert result.ecmwf_count == 51
    assert result.icon_count == 40
    assert result.gem_count == 21
    assert result.total_members == 143


def test_blend_probabilities_basic():
    """Blending should produce weighted average of ensemble and NBM buckets."""
    ensemble_buckets = [
        BucketProbability("70-72", 70.0, 72.0, 0.3),
        BucketProbability("72-74", 72.0, 74.0, 0.5),
        BucketProbability("74-76", 74.0, 76.0, 0.2),
    ]
    nbm_buckets = [
        (70.0, 72.0, 0.1),
        (72.0, 74.0, 0.6),
        (74.0, 76.0, 0.3),
    ]

    blended = blend_probabilities(ensemble_buckets, nbm_buckets, nbm_weight=0.6)
    total = sum(b.probability for b in blended)
    assert abs(total - 1.0) < 0.01


def test_nws_weight_one_full_override():
    """With nws_weight=1.0, ensemble mean should equal NWS exactly (old behavior)."""
    rng = np.random.default_rng(42)
    members = list(rng.normal(30.0, 3.0, 82))
    forecast = _make_forecast(members)
    result = compute_bucket_probabilities(forecast, nws_high=37.0, nws_weight=1.0)
    assert abs(result.ensemble_mean - 37.0) < 0.5, (
        f"With weight=1.0, ensemble_mean {result.ensemble_mean} should be ~37.0"
    )


def test_nws_weight_zero_no_nws():
    """With nws_weight=0.0, NWS should have no effect on ensemble mean."""
    rng = np.random.default_rng(42)
    members = list(rng.normal(30.0, 3.0, 82))
    forecast = _make_forecast(members)
    raw_mean = float(np.mean(members))
    result = compute_bucket_probabilities(forecast, nws_high=37.0, nws_weight=0.0)
    assert abs(result.ensemble_mean - raw_mean) < 0.5, (
        f"With weight=0.0, ensemble_mean {result.ensemble_mean} should be ~{raw_mean:.1f} (raw)"
    )
    assert result.bias_correction == 0.0, "bias_correction should be 0 with weight=0"


def test_blend_probabilities_no_nbm():
    """Blending with empty NBM should return ensemble buckets unchanged."""
    ensemble_buckets = [
        BucketProbability("70-72", 70.0, 72.0, 0.5),
        BucketProbability("72-74", 72.0, 74.0, 0.5),
    ]
    blended = blend_probabilities(ensemble_buckets, [], nbm_weight=0.6)
    assert len(blended) == 2
    assert abs(blended[0].probability - 0.5) < 0.01
