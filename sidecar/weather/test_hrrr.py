"""Tests for HRRR integration module."""

from datetime import datetime, timezone

import pytest

from weather.hrrr import (
    HRRRForecast,
    is_available,
    is_same_day,
)


def test_is_available_returns_bool():
    """is_available should return a boolean."""
    result = is_available()
    assert isinstance(result, bool)


def test_hrrr_forecast_dataclass():
    """HRRRForecast should store all fields."""
    f = HRRRForecast(
        max_temp_f=42.5,
        init_time="2026-02-12T12:00:00+00:00",
        valid_hours=12,
        lat=40.77,
        lon=-73.87,
    )
    assert f.max_temp_f == 42.5
    assert f.valid_hours == 12
    assert f.lat == 40.77


def test_is_same_day_today():
    """is_same_day should return True for today's date."""
    today = datetime.now(timezone.utc).strftime("%Y-%m-%d")
    assert is_same_day(today) is True


def test_is_same_day_tomorrow():
    """is_same_day should return False for a future date."""
    assert is_same_day("2099-12-31") is False


def test_is_same_day_yesterday():
    """is_same_day should return False for a past date."""
    assert is_same_day("2020-01-01") is False
