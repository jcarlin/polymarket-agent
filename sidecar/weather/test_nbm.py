"""Tests for NBM integration module."""

from unittest.mock import patch

import pytest

from weather.nbm import NBMForecast, fetch_nbm_percentiles, nbm_percentiles_to_buckets


def test_nbm_percentiles_to_buckets_sum_to_one():
    """Bucket probabilities from fitted normal should sum to ~1.0."""
    nbm = NBMForecast(
        city="NYC",
        date="2026-02-15",
        p10=60.0,
        p25=65.0,
        p50=70.0,
        p75=75.0,
        p90=80.0,
        max_temp=70.0,
    )
    buckets = nbm_percentiles_to_buckets(nbm)
    total = sum(prob for _, _, prob in buckets)
    assert abs(total - 1.0) < 0.01, f"Total probability {total} should be ~1.0"


def test_nbm_percentiles_to_buckets_centered():
    """Peak probability should be near the median (p50)."""
    nbm = NBMForecast(
        city="CHI",
        date="2026-02-15",
        p10=28.0,
        p25=32.0,
        p50=36.0,
        p75=40.0,
        p90=44.0,
        max_temp=36.0,
    )
    buckets = nbm_percentiles_to_buckets(nbm)

    # Find bucket with highest probability
    max_bucket = max(buckets, key=lambda b: b[2])
    # The peak bucket's lower bound should be within ~4F of the median
    assert abs(max_bucket[0] - 36.0) <= 4.0, (
        f"Peak bucket at {max_bucket[0]}-{max_bucket[1]} "
        f"should be near median 36.0"
    )


def test_nbm_percentiles_to_buckets_bucket_width():
    """Should respect custom bucket width."""
    nbm = NBMForecast(
        city="NYC",
        date="2026-02-15",
        p10=60.0,
        p25=65.0,
        p50=70.0,
        p75=75.0,
        p90=80.0,
        max_temp=70.0,
    )
    buckets = nbm_percentiles_to_buckets(nbm, bucket_width=4.0)
    for lower, upper, _ in buckets:
        assert abs((upper - lower) - 4.0) < 0.01


def test_nbm_percentiles_to_buckets_narrow_distribution():
    """Narrow percentile range should produce concentrated buckets."""
    nbm = NBMForecast(
        city="PHX",
        date="2026-07-15",
        p10=108.0,
        p25=109.0,
        p50=110.0,
        p75=111.0,
        p90=112.0,
        max_temp=110.0,
    )
    buckets = nbm_percentiles_to_buckets(nbm)
    # Most probability should be in the 106-114 range
    mass_near_peak = sum(
        prob for lower, upper, prob in buckets
        if 106 <= lower <= 114
    )
    assert mass_near_peak > 0.8, (
        f"Narrow distribution should have >80% near peak, got {mass_near_peak}"
    )


def test_nbm_percentiles_to_buckets_non_negative():
    """All bucket probabilities must be >= 0."""
    nbm = NBMForecast(
        city="NYC",
        date="2026-02-15",
        p10=30.0,
        p25=35.0,
        p50=40.0,
        p75=45.0,
        p90=50.0,
        max_temp=40.0,
    )
    buckets = nbm_percentiles_to_buckets(nbm)
    for _, _, prob in buckets:
        assert prob >= 0.0


@pytest.mark.asyncio
async def test_fetch_nbm_returns_none_without_herbie():
    """Should return None with a warning when herbie is not installed."""
    import builtins
    real_import = builtins.__import__

    def mock_import(name, *args, **kwargs):
        if name == "herbie":
            raise ImportError("No module named 'herbie'")
        return real_import(name, *args, **kwargs)

    with patch("builtins.__import__", side_effect=mock_import):
        result = await fetch_nbm_percentiles("NYC", "2026-02-15")
        assert result is None
