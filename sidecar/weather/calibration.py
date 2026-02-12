"""Automated weather forecast calibration pipeline.

Computes per-city, per-lead-time bias and spread correction factors from
historical WU validation data. Replaces the static global WEATHER_SPREAD_CORRECTION
with data-driven parameters that improve over time.

Designed to run as a nightly job (or on-demand via API endpoint).
"""

import logging
import os
import sqlite3
from dataclasses import dataclass
from datetime import datetime, timedelta
from typing import Optional

import numpy as np

logger = logging.getLogger("weather.calibration")

# Default SQLite path for calibration data
CALIBRATION_DB_PATH = os.getenv(
    "WEATHER_CALIBRATION_DB",
    os.path.join(os.path.dirname(__file__), "..", "..", "data", "weather_calibration.db"),
)


@dataclass
class CityCalibration:
    """Per-city calibration parameters."""

    city: str
    lead_days: int
    mean_bias: float  # avg(predicted - actual) — subtract from predictions
    spread_factor: float  # optimal spread correction for this city
    sample_count: int  # number of data points used
    updated_at: str  # ISO timestamp


def get_db_connection(db_path: Optional[str] = None) -> sqlite3.Connection:
    """Get a connection to the calibration database."""
    path = db_path or CALIBRATION_DB_PATH
    os.makedirs(os.path.dirname(path), exist_ok=True)
    conn = sqlite3.connect(path)
    conn.row_factory = sqlite3.Row
    _ensure_tables(conn)
    return conn


def _ensure_tables(conn: sqlite3.Connection) -> None:
    """Create calibration tables if they don't exist."""
    conn.executescript("""
        CREATE TABLE IF NOT EXISTS weather_validation (
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

        CREATE TABLE IF NOT EXISTS weather_calibration (
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


def log_validation(
    conn: sqlite3.Connection,
    city: str,
    date: str,
    wu_actual: Optional[float],
    nws_forecast: Optional[float],
    ensemble_raw_mean: Optional[float],
    ensemble_corrected_mean: Optional[float],
) -> None:
    """Log a validation data point to the database."""
    error_vs_wu = None
    if wu_actual is not None and ensemble_corrected_mean is not None:
        error_vs_wu = ensemble_corrected_mean - wu_actual

    conn.execute(
        """INSERT OR REPLACE INTO weather_validation
           (city, date, wu_actual, nws_forecast, ensemble_raw_mean,
            ensemble_corrected_mean, error_vs_wu)
           VALUES (?, ?, ?, ?, ?, ?, ?)""",
        (city, date, wu_actual, nws_forecast, ensemble_raw_mean,
         ensemble_corrected_mean, error_vs_wu),
    )
    conn.commit()


def compute_calibration(
    conn: sqlite3.Connection,
    city: str,
    lead_days: int = 1,
    min_samples: int = 7,
) -> Optional[CityCalibration]:
    """Compute calibration parameters for a city from validation data.

    Args:
        conn: Database connection
        city: City code
        lead_days: Forecast lead time in days (1 = next day)
        min_samples: Minimum data points required for calibration

    Returns:
        CityCalibration with computed parameters, or None if insufficient data.
    """
    rows = conn.execute(
        """SELECT error_vs_wu FROM weather_validation
           WHERE city = ? AND error_vs_wu IS NOT NULL
           ORDER BY date DESC LIMIT 90""",
        (city,),
    ).fetchall()

    if len(rows) < min_samples:
        logger.info(
            "Insufficient data for %s calibration: %d/%d samples",
            city, len(rows), min_samples,
        )
        return None

    errors = np.array([row["error_vs_wu"] for row in rows])
    mean_bias = float(np.mean(errors))
    error_std = float(np.std(errors))

    # Compute optimal spread factor
    # If our error std is high, we need wider spread (bigger factor)
    # Target: error_std should be ~2°F for well-calibrated forecasts
    target_std = 2.0
    if error_std > 0.1:
        spread_factor = max(0.5, min(3.0, target_std / error_std * 1.3))
    else:
        spread_factor = 1.3  # Default

    now = datetime.utcnow().isoformat()
    cal = CityCalibration(
        city=city,
        lead_days=lead_days,
        mean_bias=mean_bias,
        spread_factor=spread_factor,
        sample_count=len(rows),
        updated_at=now,
    )

    # Store in database
    conn.execute(
        """INSERT OR REPLACE INTO weather_calibration
           (city, lead_days, mean_bias, spread_factor, sample_count, updated_at)
           VALUES (?, ?, ?, ?, ?, ?)""",
        (cal.city, cal.lead_days, cal.mean_bias, cal.spread_factor,
         cal.sample_count, cal.updated_at),
    )
    conn.commit()

    logger.info(
        "Calibration for %s (lead=%dd): bias=%.2f°F, spread=%.2f, n=%d",
        city, lead_days, mean_bias, spread_factor, len(rows),
    )

    return cal


def get_calibration(
    conn: sqlite3.Connection,
    city: str,
    lead_days: int = 1,
) -> Optional[CityCalibration]:
    """Get stored calibration parameters for a city."""
    row = conn.execute(
        """SELECT * FROM weather_calibration
           WHERE city = ? AND lead_days = ?""",
        (city, lead_days),
    ).fetchone()

    if row is None:
        return None

    return CityCalibration(
        city=row["city"],
        lead_days=row["lead_days"],
        mean_bias=row["mean_bias"],
        spread_factor=row["spread_factor"],
        sample_count=row["sample_count"],
        updated_at=row["updated_at"],
    )


def run_nightly_calibration(
    conn: sqlite3.Connection,
    cities: list[str],
    lead_days: int = 1,
    min_samples: int = 7,
) -> dict[str, CityCalibration]:
    """Run calibration for all cities. Returns dict of city → calibration."""
    results: dict[str, CityCalibration] = {}
    for city in cities:
        cal = compute_calibration(conn, city, lead_days, min_samples)
        if cal is not None:
            results[city] = cal
    logger.info("Nightly calibration complete: %d/%d cities calibrated", len(results), len(cities))
    return results
