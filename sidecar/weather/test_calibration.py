"""Tests for calibration module."""

import os
import sqlite3
import tempfile

import numpy as np

from weather.calibration import (
    CalibrationParams,
    compute_calibration,
    load_calibration,
    save_calibration,
)


def _create_test_db(rows: list[tuple], include_nws: bool = False) -> str:
    """Create a temp SQLite DB with weather_actuals table and return path.

    If include_nws=True, rows should be (city, date, wu_actual_high, ensemble_mean, nws_forecast_high).
    Otherwise rows are (city, date, wu_actual_high, ensemble_mean) with nws_forecast_high=NULL.
    """
    fd, path = tempfile.mkstemp(suffix=".db")
    os.close(fd)
    conn = sqlite3.connect(path)
    conn.execute(
        "CREATE TABLE weather_actuals "
        "(city TEXT, date TEXT, wu_actual_high REAL, ensemble_mean REAL, "
        "nws_forecast_high REAL)"
    )
    if include_nws:
        conn.executemany(
            "INSERT INTO weather_actuals (city, date, wu_actual_high, ensemble_mean, nws_forecast_high) "
            "VALUES (?, ?, ?, ?, ?)",
            rows,
        )
    else:
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


def test_compute_calibration_prefers_nws_over_ensemble():
    """When nws_forecast_high is available, bias should be computed against NWS, not ensemble."""
    # NWS says 38, ensemble says 32, actual is 42 → bias vs NWS = +4, bias vs ensemble = +10
    rows = []
    for i in range(6):
        # (city, date, wu_actual_high, ensemble_mean, nws_forecast_high)
        rows.append(("NYC", f"2026-01-{i + 1:02d}", 42.0, 32.0, 38.0))

    db_path = _create_test_db(rows, include_nws=True)
    try:
        result = compute_calibration(db_path)
        assert "NYC" in result
        # Bias should be ~4.0 (actual - nws), NOT ~10.0 (actual - ensemble)
        assert 3.5 < result["NYC"].bias_offset < 4.5, \
            f"Bias {result['NYC'].bias_offset} should be ~4.0 (actual - NWS)"
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
