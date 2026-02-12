"""Automated calibration from historical weather actuals data."""

import json
import logging
import sqlite3
from dataclasses import asdict, dataclass
import numpy as np

logger = logging.getLogger("weather.calibration")


@dataclass
class CalibrationParams:
    city: str
    bias_offset: float     # mean(wu_actual_high - ensemble_mean)
    spread_factor: float   # std(actuals) / std(predictions), clipped [0.8, 2.0]
    sample_size: int


MIN_OBSERVATIONS = 20
SPREAD_FACTOR_MIN = 0.8
SPREAD_FACTOR_MAX = 2.0


def compute_calibration(db_path: str) -> dict[str, CalibrationParams]:
    """Compute per-city calibration parameters from weather_actuals table.

    Reads rows from the weather_actuals table with columns:
        city, date, wu_actual_high, ensemble_mean

    Only calibrates cities with >= 20 observations.

    Returns:
        Dict mapping city code to CalibrationParams.
    """
    try:
        conn = sqlite3.connect(db_path)
        conn.row_factory = sqlite3.Row
        cursor = conn.cursor()

        cursor.execute(
            "SELECT city, wu_actual_high, ensemble_mean FROM weather_actuals"
        )
        rows = cursor.fetchall()
        conn.close()
    except sqlite3.OperationalError as e:
        logger.warning("Failed to read weather_actuals: %s", e)
        return {}

    # Group by city
    city_data: dict[str, list[tuple[float, float]]] = {}
    for row in rows:
        city = row["city"]
        actual = row["wu_actual_high"]
        predicted = row["ensemble_mean"]
        if actual is None or predicted is None:
            continue
        city_data.setdefault(city, []).append((float(actual), float(predicted)))

    result: dict[str, CalibrationParams] = {}
    for city, pairs in city_data.items():
        if len(pairs) < MIN_OBSERVATIONS:
            logger.info(
                "Skipping %s: only %d observations (need %d)",
                city, len(pairs), MIN_OBSERVATIONS,
            )
            continue

        actuals = np.array([p[0] for p in pairs])
        predicted = np.array([p[1] for p in pairs])

        bias_offset = float(np.mean(actuals - predicted))

        std_actual = float(np.std(actuals))
        std_predicted = float(np.std(predicted))

        if std_predicted > 0:
            spread_factor = np.clip(
                std_actual / std_predicted,
                SPREAD_FACTOR_MIN,
                SPREAD_FACTOR_MAX,
            )
        else:
            spread_factor = 1.0

        result[city] = CalibrationParams(
            city=city,
            bias_offset=round(bias_offset, 4),
            spread_factor=round(float(spread_factor), 4),
            sample_size=len(pairs),
        )
        logger.info(
            "Calibration for %s: bias=%.2f, spread=%.2f, n=%d",
            city, bias_offset, spread_factor, len(pairs),
        )

    return result


def save_calibration(params: dict[str, CalibrationParams], path: str) -> None:
    """Save calibration parameters to a JSON file."""
    data = {city: asdict(cp) for city, cp in params.items()}
    with open(path, "w") as f:
        json.dump(data, f, indent=2)
    logger.info("Saved calibration for %d cities to %s", len(params), path)


def load_calibration(path: str) -> dict[str, CalibrationParams]:
    """Load calibration parameters from a JSON file.

    Returns empty dict if file not found or invalid.
    """
    try:
        with open(path) as f:
            data = json.load(f)
        result: dict[str, CalibrationParams] = {}
        for city, values in data.items():
            result[city] = CalibrationParams(
                city=values["city"],
                bias_offset=values["bias_offset"],
                spread_factor=values["spread_factor"],
                sample_size=values["sample_size"],
            )
        logger.info("Loaded calibration for %d cities from %s", len(result), path)
        return result
    except FileNotFoundError:
        logger.warning("Calibration file not found: %s", path)
        return {}
    except (json.JSONDecodeError, KeyError) as e:
        logger.warning("Failed to parse calibration file %s: %s", path, e)
        return {}
