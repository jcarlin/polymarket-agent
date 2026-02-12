"""Tests for calibration module and probability model."""

import os
import sqlite3
import tempfile
from dataclasses import dataclass, field

import numpy as np

from weather.calibration import (
    CalibrationParams,
    compute_calibration,
    load_calibration,
    save_calibration,
)
from weather.probability_model import compute_bucket_probabilities


def _create_test_db(
    rows: list[tuple],
    include_nws: bool = False,
    include_source: bool = False,
) -> str:
    """Create a temp SQLite DB with weather_actuals table and return path.

    If include_source=True, rows should include source as the last element.
    If include_nws=True (and not include_source), rows are
        (city, date, wu_actual_high, ensemble_mean, nws_forecast_high).
    Otherwise rows are (city, date, wu_actual_high, ensemble_mean) with nws_forecast_high=NULL.
    """
    fd, path = tempfile.mkstemp(suffix=".db")
    os.close(fd)
    conn = sqlite3.connect(path)
    if include_source:
        conn.execute(
            "CREATE TABLE weather_actuals "
            "(city TEXT, date TEXT, wu_actual_high REAL, ensemble_mean REAL, "
            "nws_forecast_high REAL, source TEXT DEFAULT 'organic')"
        )
        conn.executemany(
            "INSERT INTO weather_actuals "
            "(city, date, wu_actual_high, ensemble_mean, nws_forecast_high, source) "
            "VALUES (?, ?, ?, ?, ?, ?)",
            rows,
        )
    elif include_nws:
        conn.execute(
            "CREATE TABLE weather_actuals "
            "(city TEXT, date TEXT, wu_actual_high REAL, ensemble_mean REAL, "
            "nws_forecast_high REAL)"
        )
        conn.executemany(
            "INSERT INTO weather_actuals (city, date, wu_actual_high, ensemble_mean, nws_forecast_high) "
            "VALUES (?, ?, ?, ?, ?)",
            rows,
        )
    else:
        conn.execute(
            "CREATE TABLE weather_actuals "
            "(city TEXT, date TEXT, wu_actual_high REAL, ensemble_mean REAL, "
            "nws_forecast_high REAL)"
        )
        conn.executemany(
            "INSERT INTO weather_actuals (city, date, wu_actual_high, ensemble_mean) "
            "VALUES (?, ?, ?, ?)",
            rows,
        )
    conn.commit()
    conn.close()
    return path


def test_compute_calibration_insufficient_data():
    """Cities with fewer than 5 observations should be excluded."""
    rows = [("NYC", f"2026-01-{i:02d}", 35.0 + i, 33.0 + i) for i in range(1, 5)]
    db_path = _create_test_db(rows)
    try:
        result = compute_calibration(db_path)
        assert "NYC" not in result
        assert len(result) == 0
    finally:
        os.unlink(db_path)


def test_compute_calibration_with_data():
    """Should compute correct bias and spread with 7 observations (above MIN_OBSERVATIONS=5)."""
    rng = np.random.default_rng(42)
    rows = []
    for i in range(7):
        actual = 70.0 + rng.normal(0, 3)
        predicted = actual - 2.0 + rng.normal(0, 0.5)  # systematic cold bias of ~2F
        rows.append(("NYC", f"2026-01-{(i % 28) + 1:02d}", actual, predicted))

    db_path = _create_test_db(rows)
    try:
        result = compute_calibration(db_path)
        assert "NYC" in result
        params = result["NYC"]
        assert params.sample_size == 7
        # Bias should be approximately +2.0 (actuals are ~2F warmer than predictions)
        assert 0.5 < params.bias_offset < 4.0, f"Bias {params.bias_offset} should be ~2.0"
        # Spread factor: std(actuals) / std(predictions) -- both have similar variance
        assert 0.8 <= params.spread_factor <= 2.0
    finally:
        os.unlink(db_path)


def test_compute_calibration_multiple_cities():
    """Should handle multiple cities independently."""
    rng = np.random.default_rng(42)
    rows = []
    for i in range(7):
        # NYC: actuals ~70, predictions ~62 → warm bias ~+8
        rows.append(("NYC", f"2026-01-{(i % 28) + 1:02d}", 70.0 + rng.normal(0, 1), 62.0 + rng.normal(0, 1)))
        # CHI: actuals ~30, predictions ~38 → cold bias ~-8
        rows.append(("CHI", f"2026-01-{(i % 28) + 1:02d}", 30.0 + rng.normal(0, 1), 38.0 + rng.normal(0, 1)))

    db_path = _create_test_db(rows)
    try:
        result = compute_calibration(db_path)
        assert "NYC" in result
        assert "CHI" in result
        # NYC has warm bias (actuals > predictions)
        assert result["NYC"].bias_offset > 0
        # CHI has cold bias (actuals < predictions)
        assert result["CHI"].bias_offset < 0
    finally:
        os.unlink(db_path)


def test_compute_calibration_uses_blended_prediction():
    """When both NWS and ensemble are available, bias should be computed against the
    pipeline's NWS-weighted blend (0.85*NWS + 0.15*ensemble), not NWS alone."""
    # NWS says 38, ensemble says 32, actual is 42
    # Old formula: bias = actual - NWS = 42 - 38 = +4.0
    # New formula: predicted = 0.85*38 + 0.15*32 = 32.3 + 4.8 = 37.1
    #              bias = 42 - 37.1 = +4.9
    rows = []
    for i in range(6):
        # (city, date, wu_actual_high, ensemble_mean, nws_forecast_high)
        rows.append(("NYC", f"2026-01-{i + 1:02d}", 42.0, 32.0, 38.0))

    db_path = _create_test_db(rows, include_nws=True)
    try:
        result = compute_calibration(db_path)
        assert "NYC" in result
        # predicted = 0.85*38 + 0.15*32 = 37.1, bias = 42 - 37.1 = 4.9
        expected_bias = 42.0 - (0.85 * 38.0 + 0.15 * 32.0)
        assert abs(result["NYC"].bias_offset - expected_bias) < 0.01, \
            f"Bias {result['NYC'].bias_offset} should be ~{expected_bias:.1f} (actual - blended)"
        # Should be larger than old NWS-only bias of 4.0
        assert result["NYC"].bias_offset > 4.0
    finally:
        os.unlink(db_path)


def test_compute_calibration_falls_back_to_ensemble():
    """When nws_forecast_high is NULL, bias should use ensemble_mean."""
    rows = []
    for i in range(6):
        # (city, date, wu_actual_high, ensemble_mean, nws_forecast_high=None)
        rows.append(("NYC", f"2026-01-{i + 1:02d}", 42.0, 38.0, None))

    db_path = _create_test_db(rows, include_nws=True)
    try:
        result = compute_calibration(db_path)
        assert "NYC" in result
        # Bias should be ~4.0 (actual - ensemble)
        assert 3.5 < result["NYC"].bias_offset < 4.5, \
            f"Bias {result['NYC'].bias_offset} should be ~4.0 (actual - ensemble)"
    finally:
        os.unlink(db_path)


def test_compute_calibration_missing_table():
    """Should return empty dict if weather_actuals table doesn't exist."""
    fd, path = tempfile.mkstemp(suffix=".db")
    os.close(fd)
    conn = sqlite3.connect(path)
    conn.close()
    try:
        result = compute_calibration(path)
        assert result == {}
    finally:
        os.unlink(path)


def test_compute_calibration_spread_factor_clamped():
    """Spread factor should be clipped to [0.8, 2.0]."""
    # Create data where actuals have much higher variance than predictions
    rows = []
    for i in range(7):
        actual = 70.0 + (i - 3) * 5  # high variance
        predicted = 70.0 + (i - 3) * 0.5  # very low variance
        rows.append(("NYC", f"2026-01-{(i % 28) + 1:02d}", actual, predicted))

    db_path = _create_test_db(rows)
    try:
        result = compute_calibration(db_path)
        assert result["NYC"].spread_factor == 2.0  # clamped to max
    finally:
        os.unlink(db_path)


def test_save_and_load_calibration():
    """Round-trip save and load should preserve all values."""
    params = {
        "NYC": CalibrationParams(city="NYC", bias_offset=1.5, spread_factor=1.2, sample_size=50),
        "CHI": CalibrationParams(city="CHI", bias_offset=-0.8, spread_factor=0.9, sample_size=30),
    }

    fd, path = tempfile.mkstemp(suffix=".json")
    os.close(fd)
    try:
        save_calibration(params, path)
        loaded = load_calibration(path)

        assert len(loaded) == 2
        assert loaded["NYC"].bias_offset == 1.5
        assert loaded["NYC"].spread_factor == 1.2
        assert loaded["NYC"].sample_size == 50
        assert loaded["CHI"].bias_offset == -0.8
        assert loaded["CHI"].spread_factor == 0.9
        assert loaded["CHI"].sample_size == 30
    finally:
        os.unlink(path)


def test_load_calibration_missing_file():
    """Should return empty dict for missing file."""
    result = load_calibration("/nonexistent/calibration.json")
    assert result == {}


def test_load_calibration_invalid_json():
    """Should return empty dict for invalid JSON."""
    fd, path = tempfile.mkstemp(suffix=".json")
    os.close(fd)
    try:
        with open(path, "w") as f:
            f.write("not valid json {{{")
        result = load_calibration(path)
        assert result == {}
    finally:
        os.unlink(path)


def test_compute_calibration_weighted_backfill():
    """Backfilled rows should contribute less weight than organic rows."""
    # 3 organic rows: actual=50, predicted=45 -> bias=+5
    # 4 backfill rows: actual=50, predicted=40 -> bias=+10
    # Effective n = 3*1.0 + 4*0.6 = 5.4 >= 5 (passes MIN_OBSERVATIONS)
    # Weighted bias should be closer to +5 (organic) than +10 (backfill)
    rows = []
    for i in range(3):
        rows.append(("NYC", f"2026-01-{i + 1:02d}", 50.0, 45.0, None, "organic"))
    for i in range(4):
        rows.append(("NYC", f"2026-01-{i + 10:02d}", 50.0, 40.0, None, "backfill_gfs"))

    db_path = _create_test_db(rows, include_source=True)
    try:
        result = compute_calibration(db_path)
        assert "NYC" in result
        params = result["NYC"]
        assert params.sample_size == 7
        # Organic-only bias = 5.0, backfill-only bias = 10.0
        # Weighted: (3*5.0 + 4*0.6*10.0) / (3 + 4*0.6) = (15 + 24) / 5.4 = 7.22...
        expected_bias = (3 * 5.0 + 4 * 0.6 * 10.0) / (3 + 4 * 0.6)
        assert abs(params.bias_offset - expected_bias) < 0.01, \
            f"Bias {params.bias_offset} should be ~{expected_bias:.2f}"
        # Should be closer to 5 (organic) than 10 (backfill)
        assert params.bias_offset < 10.0
        assert params.bias_offset > 5.0
    finally:
        os.unlink(db_path)


def test_compute_calibration_backfill_only_insufficient():
    """8 backfill rows = 8*0.6 = 4.8 effective, should be insufficient."""
    rows = []
    for i in range(8):
        rows.append(("NYC", f"2026-01-{i + 1:02d}", 50.0, 45.0, None, "backfill_gfs"))

    db_path = _create_test_db(rows, include_source=True)
    try:
        result = compute_calibration(db_path)
        assert "NYC" not in result, (
            "8 backfill rows (effective=4.8) should be insufficient"
        )
    finally:
        os.unlink(db_path)


def test_compute_calibration_backfill_sufficient_at_nine():
    """9 backfill rows = 9*0.6 = 5.4 effective, should be sufficient."""
    rows = []
    for i in range(9):
        rows.append(("NYC", f"2026-01-{i + 1:02d}", 50.0, 45.0, None, "backfill_gfs"))

    db_path = _create_test_db(rows, include_source=True)
    try:
        result = compute_calibration(db_path)
        assert "NYC" in result, (
            "9 backfill rows (effective=5.4) should be sufficient"
        )
        assert abs(result["NYC"].bias_offset - 5.0) < 0.01
    finally:
        os.unlink(db_path)


def test_compute_calibration_no_source_column():
    """Backward compat: DB without source column treats all rows as organic (weight 1.0)."""
    rows = []
    for i in range(6):
        rows.append(("NYC", f"2026-01-{i + 1:02d}", 42.0, 38.0))

    # No source column in this DB
    db_path = _create_test_db(rows, include_nws=False, include_source=False)
    try:
        result = compute_calibration(db_path)
        assert "NYC" in result
        # All treated as organic weight 1.0, so 6 observations >= 5
        assert result["NYC"].sample_size == 6
        assert abs(result["NYC"].bias_offset - 4.0) < 0.01
    finally:
        os.unlink(db_path)


def test_backfill_idempotency():
    """Inserting rows with upsert should not create duplicates."""
    fd, path = tempfile.mkstemp(suffix=".db")
    os.close(fd)
    conn = sqlite3.connect(path)
    conn.execute(
        "CREATE TABLE weather_actuals "
        "(city TEXT, forecast_date TEXT, wu_actual_high REAL, ensemble_mean REAL, "
        "nws_forecast_high REAL, predicted_bucket TEXT, actual_bucket TEXT, "
        "prediction_error REAL, source TEXT DEFAULT 'organic', "
        "UNIQUE(city, forecast_date))"
    )

    # Insert a backfill row
    conn.execute(
        """INSERT INTO weather_actuals
           (city, forecast_date, wu_actual_high, ensemble_mean, source)
           VALUES (?, ?, ?, ?, ?)""",
        ("NYC", "2026-01-15", 42.0, 38.0, "backfill_gfs"),
    )
    conn.commit()

    # Insert again (upsert) -- should not create a duplicate
    conn.execute(
        """INSERT INTO weather_actuals
           (city, forecast_date, wu_actual_high, ensemble_mean, source)
           VALUES (?, ?, ?, ?, ?)
           ON CONFLICT(city, forecast_date) DO UPDATE SET
             wu_actual_high = COALESCE(excluded.wu_actual_high, wu_actual_high),
             ensemble_mean = COALESCE(excluded.ensemble_mean, ensemble_mean),
             source = CASE WHEN weather_actuals.source = 'organic'
                           THEN weather_actuals.source
                           ELSE excluded.source END""",
        ("NYC", "2026-01-15", 43.0, 39.0, "backfill_gfs"),
    )
    conn.commit()

    cursor = conn.execute(
        "SELECT COUNT(*) FROM weather_actuals WHERE city='NYC' AND forecast_date='2026-01-15'"
    )
    count = cursor.fetchone()[0]
    assert count == 1, f"Expected 1 row, got {count} (upsert should prevent duplicates)"

    # Verify the values were updated (COALESCE with non-null values updates)
    cursor = conn.execute(
        "SELECT wu_actual_high, ensemble_mean, source FROM weather_actuals "
        "WHERE city='NYC' AND forecast_date='2026-01-15'"
    )
    row = cursor.fetchone()
    assert row[0] == 43.0, "wu_actual_high should be updated to 43.0"
    assert row[1] == 39.0, "ensemble_mean should be updated to 39.0"
    assert row[2] == "backfill_gfs", "source should remain backfill_gfs"

    conn.close()
    os.unlink(path)


def test_backfill_organic_source_preserved():
    """Organic source should not be overwritten by backfill upsert."""
    fd, path = tempfile.mkstemp(suffix=".db")
    os.close(fd)
    conn = sqlite3.connect(path)
    conn.execute(
        "CREATE TABLE weather_actuals "
        "(city TEXT, forecast_date TEXT, wu_actual_high REAL, ensemble_mean REAL, "
        "nws_forecast_high REAL, predicted_bucket TEXT, actual_bucket TEXT, "
        "prediction_error REAL, source TEXT DEFAULT 'organic', "
        "UNIQUE(city, forecast_date))"
    )

    # Insert an organic row first
    conn.execute(
        """INSERT INTO weather_actuals
           (city, forecast_date, wu_actual_high, ensemble_mean, source)
           VALUES (?, ?, ?, ?, ?)""",
        ("NYC", "2026-01-15", 42.0, 38.0, "organic"),
    )
    conn.commit()

    # Backfill upsert should NOT overwrite the organic source
    conn.execute(
        """INSERT INTO weather_actuals
           (city, forecast_date, wu_actual_high, ensemble_mean, source)
           VALUES (?, ?, ?, ?, ?)
           ON CONFLICT(city, forecast_date) DO UPDATE SET
             wu_actual_high = COALESCE(excluded.wu_actual_high, wu_actual_high),
             ensemble_mean = COALESCE(excluded.ensemble_mean, ensemble_mean),
             source = CASE WHEN weather_actuals.source = 'organic'
                           THEN weather_actuals.source
                           ELSE excluded.source END""",
        ("NYC", "2026-01-15", 43.0, 39.0, "backfill_gfs"),
    )
    conn.commit()

    cursor = conn.execute(
        "SELECT source FROM weather_actuals WHERE city='NYC' AND forecast_date='2026-01-15'"
    )
    row = cursor.fetchone()
    assert row[0] == "organic", "Organic source should be preserved on upsert"

    conn.close()
    os.unlink(path)


# --- Probability model tests: wu_actual floor clamping ---


@dataclass
class _FakeEnsembleForecast:
    """Minimal mock for EnsembleForecast used by compute_bucket_probabilities."""
    city: str = "ATL"
    station_icao: str = "KATL"
    forecast_date: str = "2026-02-12"
    gefs_daily_max: list = field(default_factory=list)
    ecmwf_daily_max: list = field(default_factory=list)
    icon_daily_max: list = field(default_factory=list)
    gem_daily_max: list = field(default_factory=list)
    all_members: list = field(default_factory=list)


def test_wu_actual_floor_zeros_impossible_buckets():
    """When wu_actual=64, all buckets below 64°F should have ~0% probability."""
    # Simulate ensemble members centered around 60°F (some below 64)
    rng = np.random.default_rng(42)
    members = list(rng.normal(60.0, 4.0, size=100))

    forecast = _FakeEnsembleForecast(
        gefs_daily_max=members[:31],
        ecmwf_daily_max=members[31:82],
        all_members=members,
    )

    # Without floor: should have probability mass below 64
    probs_no_floor = compute_bucket_probabilities(forecast, bucket_range=(50, 80))
    below_64_no_floor = sum(
        b.probability for b in probs_no_floor.buckets if b.upper <= 64
    )
    assert below_64_no_floor > 0.1, (
        f"Without floor, expected >10% below 64°F, got {below_64_no_floor:.1%}"
    )

    # With floor at 64: nothing below 64 should survive
    probs_with_floor = compute_bucket_probabilities(
        forecast, bucket_range=(50, 80), wu_actual=64.0
    )
    below_64_with_floor = sum(
        b.probability for b in probs_with_floor.buckets if b.upper <= 64
    )
    assert below_64_with_floor < 0.01, (
        f"With wu_actual=64, expected <1% below 64°F, got {below_64_with_floor:.1%}"
    )
    assert probs_with_floor.wu_actual_floor == 64.0


def test_wu_actual_floor_preserves_above_buckets():
    """Floor should not eliminate probability mass above the actual."""
    rng = np.random.default_rng(42)
    members = list(rng.normal(70.0, 3.0, size=100))

    forecast = _FakeEnsembleForecast(
        gefs_daily_max=members[:31],
        ecmwf_daily_max=members[31:82],
        all_members=members,
    )

    probs = compute_bucket_probabilities(
        forecast, bucket_range=(50, 90), wu_actual=65.0
    )
    above_65 = sum(b.probability for b in probs.buckets if b.lower >= 66)
    # Most mass should still be above the floor (mean is 70)
    assert above_65 > 0.5, (
        f"Expected >50% above 66°F with mean=70, got {above_65:.1%}"
    )
    total = sum(b.probability for b in probs.buckets)
    assert abs(total - 1.0) < 0.01, f"Total probability should be ~1.0, got {total}"


def test_wu_actual_floor_none_is_noop():
    """When wu_actual is None (not same-day), behavior should be unchanged."""
    rng = np.random.default_rng(42)
    members = list(rng.normal(60.0, 4.0, size=100))

    forecast = _FakeEnsembleForecast(
        gefs_daily_max=members[:31],
        ecmwf_daily_max=members[31:82],
        all_members=members,
    )

    probs = compute_bucket_probabilities(forecast, bucket_range=(50, 80), wu_actual=None)
    assert probs.wu_actual_floor is None
    below_60 = sum(b.probability for b in probs.buckets if b.upper <= 60)
    assert below_60 > 0.05, "Without floor, should have mass below mean"
