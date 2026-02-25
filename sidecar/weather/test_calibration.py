"""Tests for weather calibration pipeline."""

import sqlite3

import numpy as np
import pytest

from weather.calibration import (
    CityCalibration,
    compute_calibration,
    get_calibration,
    get_db_connection,
    log_validation,
    run_nightly_calibration,
)


@pytest.fixture
def db():
    """Create an in-memory calibration database."""
    conn = sqlite3.connect(":memory:")
    conn.row_factory = sqlite3.Row
    conn.executescript("""
        CREATE TABLE weather_validation (
            city TEXT NOT NULL,
            date TEXT NOT NULL,
            wu_actual REAL,
            nws_forecast REAL,
            ensemble_raw_mean REAL,
            ensemble_corrected_mean REAL,
            error_vs_wu REAL,
            created_at TEXT DEFAULT (datetime('now')),
            PRIMARY KEY (city, date)
        );
        CREATE TABLE weather_calibration (
            city TEXT NOT NULL,
            lead_days INTEGER NOT NULL,
            mean_bias REAL NOT NULL,
            spread_factor REAL NOT NULL,
            sample_count INTEGER NOT NULL,
            updated_at TEXT DEFAULT (datetime('now')),
            PRIMARY KEY (city, lead_days)
        );
    """)
    conn.commit()
    yield conn
    conn.close()


def _seed_validation(db, city: str, n: int = 10, bias: float = 2.0):
    """Seed validation data with a known bias."""
    rng = np.random.default_rng(42)
    for i in range(n):
        date = f"2026-02-{i + 1:02d}"
        wu_actual = 35.0 + rng.normal(0, 3)
        corrected_mean = wu_actual + bias + rng.normal(0, 1)
        log_validation(
            db, city, date,
            wu_actual=wu_actual,
            nws_forecast=wu_actual + 1.0,
            ensemble_raw_mean=wu_actual - 3.0,
            ensemble_corrected_mean=corrected_mean,
        )


def test_log_validation(db):
    """log_validation should store data correctly."""
    log_validation(db, "NYC", "2026-02-10", 42.0, 40.0, 35.0, 39.0)
    row = db.execute("SELECT * FROM weather_validation WHERE city='NYC'").fetchone()
    assert row["wu_actual"] == 42.0
    assert row["error_vs_wu"] == -3.0  # 39.0 - 42.0


def test_log_validation_null_wu(db):
    """When WU is None, error should be None."""
    log_validation(db, "NYC", "2026-02-10", None, 40.0, 35.0, 39.0)
    row = db.execute("SELECT * FROM weather_validation WHERE city='NYC'").fetchone()
    assert row["wu_actual"] is None
    assert row["error_vs_wu"] is None


def test_log_validation_upsert(db):
    """Logging same city+date should update (REPLACE)."""
    log_validation(db, "NYC", "2026-02-10", 42.0, 40.0, 35.0, 39.0)
    log_validation(db, "NYC", "2026-02-10", 43.0, 41.0, 36.0, 40.0)
    rows = db.execute("SELECT * FROM weather_validation WHERE city='NYC'").fetchall()
    assert len(rows) == 1
    assert rows[0]["wu_actual"] == 43.0


def test_compute_calibration_sufficient_data(db):
    """With enough data, calibration should compute bias and spread."""
    _seed_validation(db, "NYC", n=10, bias=2.0)
    cal = compute_calibration(db, "NYC", min_samples=7)
    assert cal is not None
    assert cal.city == "NYC"
    assert cal.sample_count == 10
    # Bias should be approximately 2.0 (seeded bias)
    assert abs(cal.mean_bias - 2.0) < 1.5
    assert 0.5 <= cal.spread_factor <= 3.0


def test_compute_calibration_insufficient_data(db):
    """With too few samples, should return None."""
    _seed_validation(db, "NYC", n=3, bias=1.0)
    cal = compute_calibration(db, "NYC", min_samples=7)
    assert cal is None


def test_get_calibration_after_compute(db):
    """get_calibration should return stored calibration."""
    _seed_validation(db, "NYC", n=10, bias=1.5)
    compute_calibration(db, "NYC", min_samples=7)

    cal = get_calibration(db, "NYC")
    assert cal is not None
    assert cal.city == "NYC"
    assert cal.sample_count == 10


def test_get_calibration_missing(db):
    """get_calibration should return None for uncalibrated city."""
    cal = get_calibration(db, "NYC")
    assert cal is None


def test_run_nightly_calibration(db):
    """Nightly calibration should process all cities with data."""
    _seed_validation(db, "NYC", n=10, bias=2.0)
    _seed_validation(db, "CHI", n=10, bias=1.0)
    _seed_validation(db, "MIA", n=3, bias=0.5)  # Too few

    results = run_nightly_calibration(db, ["NYC", "CHI", "MIA"], min_samples=7)
    assert "NYC" in results
    assert "CHI" in results
    assert "MIA" not in results  # Insufficient data


def test_calibration_spread_factor_clamp(db):
    """Spread factor should be clamped to [0.5, 3.0]."""
    # Very consistent data (low error std) should produce factor near upper bound
    for i in range(15):
        date = f"2026-02-{i + 1:02d}"
        log_validation(db, "PHX", date, 80.0, 80.0, 75.0, 80.1)  # Tiny error

    cal = compute_calibration(db, "PHX", min_samples=7)
    assert cal is not None
    assert cal.spread_factor <= 3.0
    assert cal.spread_factor >= 0.5


def test_city_calibration_dataclass():
    """CityCalibration should store all fields."""
    cal = CityCalibration(
        city="NYC", lead_days=1, mean_bias=1.5,
        spread_factor=1.2, sample_count=30, updated_at="2026-02-12T00:00:00",
    )
    assert cal.city == "NYC"
    assert cal.mean_bias == 1.5
    assert cal.spread_factor == 1.2
