"""Tests for Open-Meteo ensemble client."""

import pytest
from weather.open_meteo import (
    CITY_CONFIGS,
    EnsembleForecast,
    HRRRForecast,
    _celsius_to_fahrenheit,
    fetch_ensemble,
    fetch_hrrr,
    fetch_all_cities,
    fetch_nws_for_city,
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
    # New fields should default to empty lists
    assert ef.icon_daily_max == []
    assert ef.gem_daily_max == []


def test_ensemble_forecast_with_icon_gem():
    """EnsembleForecast should support ICON and GEM members."""
    ef = EnsembleForecast(
        city="NYC",
        station_icao="KLGA",
        forecast_date="2026-01-15",
        gefs_daily_max=[70.0] * 31,
        ecmwf_daily_max=[71.0] * 51,
        icon_daily_max=[72.0] * 40,
        gem_daily_max=[73.0] * 21,
        all_members=[70.0] * 31 + [71.0] * 51 + [72.0] * 40 + [73.0] * 21,
    )
    assert len(ef.all_members) == 143
    assert len(ef.icon_daily_max) == 40
    assert len(ef.gem_daily_max) == 21


def test_hrrr_forecast_dataclass():
    """HRRRForecast should store hourly temps and max."""
    hrrr = HRRRForecast(
        city="NYC",
        station_icao="KLGA",
        forecast_date="2026-02-11",
        hourly_temps_f=[35.0, 36.0, 37.0, 38.0, 37.5],
        max_temp_f=38.0,
    )
    assert hrrr.max_temp_f == 38.0
    assert len(hrrr.hourly_temps_f) == 5


@pytest.mark.asyncio
async def test_fetch_hrrr_unknown_city():
    """Unknown city should return None."""
    result = await fetch_hrrr("UNKNOWN", "2026-01-15")
    assert result is None


@pytest.mark.asyncio
async def test_fetch_all_cities_empty_list():
    """Fetching with empty city list should return empty dict."""
    result = await fetch_all_cities("2026-01-15", cities=[])
    assert result == {}


@pytest.mark.asyncio
async def test_fetch_nws_for_city_unknown():
    """Unknown city should return None."""
    result = await fetch_nws_for_city("UNKNOWN", "2026-01-15")
    assert result is None
