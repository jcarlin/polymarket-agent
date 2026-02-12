"""Tests for NBM integration module."""

import numpy as np
import pytest

from weather.nbm import (
    NBMPercentiles,
    is_available,
    nbm_anchor_and_spread,
)


def test_is_available_returns_bool():
    """is_available should return a boolean."""
    result = is_available()
    assert isinstance(result, bool)


def test_nbm_percentiles_dataclass():
    """NBMPercentiles should store all percentile values."""
    p = NBMPercentiles(
        p10=30.0, p25=33.0, p50=36.0, p75=39.0, p90=42.0,
        lat=40.77, lon=-73.87, init_time="2026-02-12T00:00:00",
    )
    assert p.p50 == 36.0
    assert p.p10 < p.p25 < p.p50 < p.p75 < p.p90


def test_nbm_anchor_and_spread_normal():
    """nbm_anchor_and_spread should return anchor near p50 and reasonable spread."""
    nbm = NBMPercentiles(
        p10=30.0, p25=33.0, p50=36.0, p75=39.0, p90=42.0,
        lat=40.77, lon=-73.87, init_time="2026-02-12T00:00:00",
    )
    raw_std = 3.0
    anchor, spread = nbm_anchor_and_spread(nbm, raw_std)

    assert anchor == 36.0  # p50
    assert 0.5 <= spread <= 3.0  # clamped range
    # NBM range is 12°F (42-30), sigma ~4.69 => spread ~1.56
    assert spread > 1.0  # NBM wider than raw ensemble


def test_nbm_anchor_and_spread_tight_ensemble():
    """When raw ensemble is very tight, spread correction should be larger."""
    nbm = NBMPercentiles(
        p10=30.0, p25=33.0, p50=36.0, p75=39.0, p90=42.0,
        lat=40.77, lon=-73.87, init_time="2026-02-12T00:00:00",
    )
    anchor, spread = nbm_anchor_and_spread(nbm, raw_ensemble_std=1.0)
    assert spread > 2.0  # Should widen a lot


def test_nbm_anchor_and_spread_wide_ensemble():
    """When raw ensemble is already wide, spread correction should be smaller."""
    nbm = NBMPercentiles(
        p10=30.0, p25=33.0, p50=36.0, p75=39.0, p90=42.0,
        lat=40.77, lon=-73.87, init_time="2026-02-12T00:00:00",
    )
    anchor, spread = nbm_anchor_and_spread(nbm, raw_ensemble_std=6.0)
    assert spread < 1.0  # Should narrow


def test_nbm_anchor_and_spread_zero_std_fallback():
    """Zero raw std should use default spread correction."""
    nbm = NBMPercentiles(
        p10=30.0, p25=33.0, p50=36.0, p75=39.0, p90=42.0,
        lat=40.77, lon=-73.87, init_time="2026-02-12T00:00:00",
    )
    anchor, spread = nbm_anchor_and_spread(nbm, raw_ensemble_std=0.0)
    assert spread == 1.3  # Default fallback


def test_nbm_anchor_spread_clamped():
    """Spread correction should be clamped to [0.5, 3.0]."""
    # Very tiny raw std → would produce huge correction, should clamp to 3.0
    nbm = NBMPercentiles(
        p10=20.0, p25=30.0, p50=40.0, p75=50.0, p90=60.0,
        lat=40.77, lon=-73.87, init_time="2026-02-12T00:00:00",
    )
    _, spread = nbm_anchor_and_spread(nbm, raw_ensemble_std=0.5)
    assert spread == 3.0  # Clamped

    # Very wide raw std → would produce tiny correction, should clamp to 0.5
    _, spread = nbm_anchor_and_spread(nbm, raw_ensemble_std=100.0)
    assert spread == 0.5  # Clamped
