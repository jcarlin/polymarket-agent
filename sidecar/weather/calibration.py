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
    bias_offset: float     # mean(wu_actual - pipeline_pre_cal_prediction)
    spread_factor: float   # std(actuals) / std(predictions), clipped [0.8, 2.0]
    sample_size: int


MIN_OBSERVATIONS = 5
SPREAD_FACTOR_MIN = 0.8
SPREAD_FACTOR_MAX = 2.0
BACKFILL_WEIGHT = 0.6
DEFAULT_NWS_WEIGHT = 0.85


def compute_calibration(
    db_path: str,
    nws_weight: float = DEFAULT_NWS_WEIGHT,
) -> dict[str, CalibrationParams]:
    """Compute per-city calibration parameters from weather_actuals table.

    Reads rows from the weather_actuals table with columns:
        city, date, wu_actual_high, ensemble_mean, nws_forecast_high

    Computes bias as actual minus the pipeline's pre-calibration prediction:
      - When both nws_forecast_high and ensemble_mean are available:
        predicted = nws_weight * nws + (1 - nws_weight) * ensemble
        (This matches the NWS anchor blend in the probability pipeline.)
      - When only nws_forecast_high is available: predicted = nws_high
      - When only ensemble_mean is available: predicted = ensemble_mean

    Backfilled rows (source starting with 'backfill') receive reduced weight
    (BACKFILL_WEIGHT=0.6) compared to organic observations (weight=1.0).
    The MIN_OBSERVATIONS check uses the sum of weights (effective_n).

    Only calibrates cities with >= MIN_OBSERVATIONS effective observations.

    Args:
        db_path: Path to SQLite database with weather_actuals table.
        nws_weight: NWS weight used in the pipeline's NWS anchor blend (default 0.85).

    Returns:
        Dict mapping city code to CalibrationParams.
    """
    try:
        conn = sqlite3.connect(db_path)
        conn.row_factory = sqlite3.Row
        cursor = conn.cursor()

        # Try to read source column (may not exist in old DBs)
        has_source = False
        try:
            cursor.execute(
                "SELECT city, wu_actual_high, ensemble_mean, nws_forecast_high, source "
                "FROM weather_actuals"
            )
            has_source = True
        except sqlite3.OperationalError:
            cursor.execute(
                "SELECT city, wu_actual_high, ensemble_mean, nws_forecast_high "
                "FROM weather_actuals"
            )

        rows = cursor.fetchall()
        conn.close()
    except sqlite3.OperationalError as e:
        logger.warning("Failed to read weather_actuals: %s", e)
        return {}

    # Group by city: (actual, predicted, weight)
    city_data: dict[str, list[tuple[float, float, float]]] = {}
    for row in rows:
        city = row["city"]
        actual = row["wu_actual_high"]
        nws_high = row["nws_forecast_high"]
        ensemble = row["ensemble_mean"]

        # Compute predicted to match the pipeline's pre-calibration output:
        # pipeline does: corrected = nws_weight * nws + (1 - nws_weight) * ensemble
        # then adds cal_bias on top. So cal_bias should correct the residual.
        if nws_high is not None and ensemble is not None:
            predicted = nws_weight * nws_high + (1.0 - nws_weight) * ensemble
        elif nws_high is not None:
            predicted = nws_high
        else:
            predicted = ensemble

        if actual is None or predicted is None:
            continue

        # Determine weight based on source
        source = "organic"
        if has_source:
            try:
                source = row["source"] if row["source"] is not None else "organic"
            except (IndexError, KeyError):
                source = "organic"
        weight = BACKFILL_WEIGHT if source.startswith("backfill") else 1.0

        city_data.setdefault(city, []).append(
            (float(actual), float(predicted), weight)
        )

    result: dict[str, CalibrationParams] = {}
    for city, triples in city_data.items():
        weights = np.array([t[2] for t in triples])
        effective_n = float(np.sum(weights))

        if effective_n < MIN_OBSERVATIONS:
            logger.info(
                "Skipping %s: effective_n=%.1f (need %d, %d rows)",
                city, effective_n, MIN_OBSERVATIONS, len(triples),
            )
            continue

        actuals = np.array([t[0] for t in triples])
        predicted = np.array([t[1] for t in triples])

        errors = actuals - predicted
        bias_offset = float(np.average(errors, weights=weights))

        weighted_mean_actual = float(np.average(actuals, weights=weights))
        weighted_mean_predicted = float(np.average(predicted, weights=weights))
        std_actual = float(
            np.sqrt(
                np.average(
                    (actuals - weighted_mean_actual) ** 2, weights=weights
                )
            )
        )
        std_predicted = float(
            np.sqrt(
                np.average(
                    (predicted - weighted_mean_predicted) ** 2, weights=weights
                )
            )
        )

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
            sample_size=len(triples),
        )
        logger.info(
            "Calibration for %s: bias=%.2f, spread=%.2f, n=%d (effective=%.1f)",
            city, bias_offset, spread_factor, len(triples), effective_n,
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
