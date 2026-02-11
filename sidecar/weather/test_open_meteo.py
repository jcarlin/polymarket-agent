"""Tests for Open-Meteo ensemble client."""

import pytest
from weather.open_meteo import (
    CITY_CONFIGS,
    EnsembleForecast,
    _celsius_to_fahrenheit,
    _extract_daily_max_for_date,
    fetch_ensemble,
    fetch_all_cities,
)
from zoneinfo import ZoneInfo


def test_city_configs_complete():
    """All 20 cities should be configured."""
    assert len(CITY_CONFIGS) == 20


def test_city_configs_have_required_fields():
    """Each city must have name, ICAO, lat, lon, timezone."""
    for code, cfg in CITY_CONFIGS.items():
        assert cfg.name, f"{code} missing name"
        assert cfg.icao.startswith("K"), f"{code} ICAO should start with K"
        assert -90 <= cfg.lat <= 90, f"{code} lat out of range"
        assert -180 <= cfg.lon <= 180, f"{code} lon out of range"
        # Verify timezone is valid
        ZoneInfo(cfg.timezone)


def test_celsius_to_fahrenheit():
    assert abs(_celsius_to_fahrenheit(0.0) - 32.0) < 0.01
    assert abs(_celsius_to_fahrenheit(100.0) - 212.0) < 0.01
    assert abs(_celsius_to_fahrenheit(20.0) - 68.0) < 0.01


def test_extract_daily_max_simple():
    """Test extraction of daily max from hourly data."""
    times = [
        "2026-01-15T12:00",
        "2026-01-15T13:00",
        "2026-01-15T14:00",
        "2026-01-15T15:00",
    ]
    # One member with temps 20, 25, 22, 18
    member_values = [[20.0, 25.0, 22.0, 18.0]]
    tz = ZoneInfo("America/New_York")
    maxes = _extract_daily_max_for_date(times, member_values, "2026-01-15", tz)
    # Max is 25C = 77F
    assert len(maxes) == 1
    assert abs(maxes[0] - 77.0) < 0.1


def test_extract_daily_max_filters_by_date():
    """Only hours matching target_date in local time should be included."""
    times = [
        "2026-01-15T23:00",  # Jan 15 in UTC, but could be Jan 15 or 16 local
        "2026-01-16T05:00",  # Jan 16 UTC = Jan 16 local for NYC (EST = UTC-5)
    ]
    member_values = [[30.0, 10.0]]  # 30C on Jan 15, 10C on Jan 16 (UTC)
    tz = ZoneInfo("America/New_York")
    # For NYC (UTC-5), 23:00 UTC = 18:00 EST Jan 15, 05:00 UTC = 00:00 EST Jan 16
    maxes = _extract_daily_max_for_date(times, member_values, "2026-01-15", tz)
    assert len(maxes) == 1
    # Should only get the 30C reading (18:00 EST Jan 15)
    assert abs(maxes[0] - _celsius_to_fahrenheit(30.0)) < 0.1


@pytest.mark.asyncio
async def test_fetch_ensemble_unknown_city():
    """Unknown city should return None."""
    result = await fetch_ensemble("UNKNOWN", "2026-01-15")
    assert result is None


def test_ensemble_forecast_dataclass():
    """EnsembleForecast should combine gefs + ecmwf."""
    ef = EnsembleForecast(
        city="NYC",
        station_icao="KLGA",
        forecast_date="2026-01-15",
        gefs_daily_max=[70.0, 71.0, 72.0],
        ecmwf_daily_max=[69.0, 70.0],
        all_members=[70.0, 71.0, 72.0, 69.0, 70.0],
    )
    assert len(ef.all_members) == 5
    assert ef.city == "NYC"


@pytest.mark.asyncio
async def test_fetch_all_cities_empty_list():
    """Fetching with empty city list should return empty dict."""
    result = await fetch_all_cities("2026-01-15", cities=[])
    assert result == {}
