"""Tests for Weather Underground API-based temperature fetcher."""

from unittest.mock import AsyncMock

import httpx
import numpy as np
import pytest

from weather.wu_scraper import (
    _extract_daily_high,
    _wu_cache,
    _wu_forecast_cache,
    fetch_wu_actual,
    fetch_wu_forecast,
    icao_to_wu_path,
    _build_api_url,
    _build_forecast_api_url,
    _extract_forecast_highs,
)


def test_icao_to_wu_path():
    """Known ICAO stations should return (state, city) tuples."""
    assert icao_to_wu_path("KLGA") == ("ny", "new-york")
    assert icao_to_wu_path("KLAX") == ("ca", "los-angeles")
    assert icao_to_wu_path("KORD") == ("il", "chicago")
    assert icao_to_wu_path("KIAH") == ("tx", "houston")
    assert icao_to_wu_path("KPHX") == ("az", "phoenix")
    assert icao_to_wu_path("KPHL") == ("pa", "philadelphia")
    assert icao_to_wu_path("KSAT") == ("tx", "san-antonio")
    assert icao_to_wu_path("KSAN") == ("ca", "san-diego")
    assert icao_to_wu_path("KDFW") == ("tx", "dallas")
    assert icao_to_wu_path("KSJC") == ("ca", "san-jose")
    assert icao_to_wu_path("KATL") == ("ga", "atlanta")
    assert icao_to_wu_path("KMIA") == ("fl", "miami")
    assert icao_to_wu_path("KBOS") == ("ma", "boston")
    assert icao_to_wu_path("KSEA") == ("wa", "seattle")
    assert icao_to_wu_path("KDEN") == ("co", "denver")
    assert icao_to_wu_path("KDCA") == ("va", "washington")
    assert icao_to_wu_path("KMSP") == ("mn", "minneapolis")
    assert icao_to_wu_path("KDTW") == ("mi", "detroit")
    assert icao_to_wu_path("KTPA") == ("fl", "tampa")
    assert icao_to_wu_path("KSTL") == ("mo", "st-louis")


def test_icao_to_wu_path_all_20():
    """All 20 stations should be mapped."""
    from weather.wu_scraper import _ICAO_TO_WU
    assert len(_ICAO_TO_WU) == 20


def test_icao_to_wu_path_unknown():
    """Unknown ICAO station should return None."""
    assert icao_to_wu_path("ZZZZ") is None
    assert icao_to_wu_path("") is None
    assert icao_to_wu_path("KJFK") is None


def test_build_api_url():
    """API URL should use compact date format and correct station."""
    url = _build_api_url("KLGA", "2026-02-11")
    assert "KLGA:9:US" in url
    assert "startDate=20260211" in url
    assert "endDate=20260211" in url
    assert "units=e" in url


def test_extract_daily_high():
    """Should return max temp from hourly observations."""
    data = {
        "observations": [
            {"temp": 32, "valid_time_gmt": 1000},
            {"temp": 35, "valid_time_gmt": 2000},
            {"temp": 41, "valid_time_gmt": 3000},  # daily high
            {"temp": 38, "valid_time_gmt": 4000},
            {"temp": 33, "valid_time_gmt": 5000},
        ]
    }
    assert _extract_daily_high(data) == 41.0


def test_extract_daily_high_with_nulls():
    """Should skip observations with null temps."""
    data = {
        "observations": [
            {"temp": 32, "valid_time_gmt": 1000},
            {"temp": None, "valid_time_gmt": 2000},
            {"temp": 45, "valid_time_gmt": 3000},
        ]
    }
    assert _extract_daily_high(data) == 45.0


def test_extract_daily_high_empty():
    """Should return None for empty observations."""
    assert _extract_daily_high({"observations": []}) is None
    assert _extract_daily_high({}) is None


def test_extract_daily_high_all_null():
    """Should return None if all temps are null."""
    data = {
        "observations": [
            {"temp": None},
            {"temp": None},
        ]
    }
    assert _extract_daily_high(data) is None


@pytest.mark.asyncio
async def test_fetch_wu_actual_unknown_icao():
    """Unknown ICAO should return None without making HTTP request."""
    _wu_cache.clear()
    result = await fetch_wu_actual("ZZZZ", "2026-01-15")
    assert result is None


@pytest.mark.asyncio
async def test_fetch_wu_actual_returns_none_on_failure():
    """Should return None on HTTP failure."""
    _wu_cache.clear()
    mock_client = AsyncMock(spec=httpx.AsyncClient)
    mock_client.get = AsyncMock(side_effect=httpx.HTTPStatusError(
        "404", request=httpx.Request("GET", "http://test"), response=httpx.Response(404)
    ))
    mock_client.aclose = AsyncMock()

    result = await fetch_wu_actual("KLGA", "2026-01-15", client=mock_client)
    assert result is None


@pytest.mark.asyncio
async def test_fetch_wu_actual_success():
    """Should extract daily high from API JSON response."""
    _wu_cache.clear()

    api_response = {
        "metadata": {"status_code": 200},
        "observations": [
            {"temp": 32, "valid_time_gmt": 1000},
            {"temp": 41, "valid_time_gmt": 2000},
            {"temp": 38, "valid_time_gmt": 3000},
        ]
    }

    mock_response = AsyncMock()
    mock_response.status_code = 200
    mock_response.json = lambda: api_response
    mock_response.raise_for_status = lambda: None

    mock_client = AsyncMock(spec=httpx.AsyncClient)
    mock_client.get = AsyncMock(return_value=mock_response)
    mock_client.aclose = AsyncMock()

    result = await fetch_wu_actual("KLGA", "2026-01-15", client=mock_client)
    assert result == 41.0

    _wu_cache.clear()


@pytest.mark.asyncio
async def test_fetch_wu_actual_caches_result():
    """Subsequent calls with the same args should use cache."""
    _wu_cache.clear()

    api_response = {
        "metadata": {"status_code": 200},
        "observations": [
            {"temp": 68, "valid_time_gmt": 1000},
        ]
    }

    mock_response = AsyncMock()
    mock_response.status_code = 200
    mock_response.json = lambda: api_response
    mock_response.raise_for_status = lambda: None

    mock_client = AsyncMock(spec=httpx.AsyncClient)
    mock_client.get = AsyncMock(return_value=mock_response)
    mock_client.aclose = AsyncMock()

    result1 = await fetch_wu_actual("KLGA", "2026-01-15", client=mock_client)
    assert result1 == 68.0

    # Second call should use cache, not make another HTTP request
    mock_client.get.reset_mock()
    result2 = await fetch_wu_actual("KLGA", "2026-01-15", client=mock_client)
    assert result2 == 68.0
    mock_client.get.assert_not_called()

    _wu_cache.clear()


# ═══════════════════════════════════════════════════
# WU Forecast tests
# ═══════════════════════════════════════════════════


def test_build_forecast_api_url():
    """Forecast URL should use geocode (lat,lon) and v3 endpoint."""
    url = _build_forecast_api_url(40.77, -73.87)
    assert "geocode=40.77,-73.87" in url
    assert "v3/wx/forecast/daily/5day" in url
    assert "units=e" in url
    assert "apiKey=" in url


def test_extract_forecast_highs():
    """Should parse 5-day forecast response into (date, temp) tuples."""
    data = {
        "calendarDayTemperatureMax": [41, 38, 45, 50, 47],
        "validTimeLocal": [
            "2026-02-13T07:00:00-0500",
            "2026-02-14T07:00:00-0500",
            "2026-02-15T07:00:00-0500",
            "2026-02-16T07:00:00-0500",
            "2026-02-17T07:00:00-0500",
        ],
    }
    result = _extract_forecast_highs(data)
    assert len(result) == 5
    assert result[0] == ("2026-02-13", 41.0)
    assert result[2] == ("2026-02-15", 45.0)
    assert result[4] == ("2026-02-17", 47.0)


def test_extract_forecast_highs_with_nulls():
    """Should skip days with null temperatures."""
    data = {
        "calendarDayTemperatureMax": [41, None, 45, None, 47],
        "validTimeLocal": [
            "2026-02-13T07:00:00-0500",
            "2026-02-14T07:00:00-0500",
            "2026-02-15T07:00:00-0500",
            "2026-02-16T07:00:00-0500",
            "2026-02-17T07:00:00-0500",
        ],
    }
    result = _extract_forecast_highs(data)
    assert len(result) == 3
    dates = [r[0] for r in result]
    assert "2026-02-14" not in dates
    assert "2026-02-16" not in dates


def test_extract_forecast_highs_empty():
    """Should return empty list for missing/empty data."""
    assert _extract_forecast_highs({}) == []
    assert _extract_forecast_highs({"calendarDayTemperatureMax": []}) == []
    assert _extract_forecast_highs({"validTimeLocal": []}) == []


@pytest.mark.asyncio
async def test_fetch_wu_forecast_success():
    """Should fetch forecast and cache all 5 days."""
    _wu_forecast_cache.clear()

    api_response = {
        "calendarDayTemperatureMax": [41, 38, 45, 50, 47],
        "validTimeLocal": [
            "2026-02-13T07:00:00-0500",
            "2026-02-14T07:00:00-0500",
            "2026-02-15T07:00:00-0500",
            "2026-02-16T07:00:00-0500",
            "2026-02-17T07:00:00-0500",
        ],
    }

    mock_response = AsyncMock()
    mock_response.status_code = 200
    mock_response.json = lambda: api_response
    mock_response.raise_for_status = lambda: None

    mock_client = AsyncMock(spec=httpx.AsyncClient)
    mock_client.get = AsyncMock(return_value=mock_response)
    mock_client.aclose = AsyncMock()

    result = await fetch_wu_forecast("NYC", "2026-02-13", client=mock_client)
    assert result == 41.0

    # All 5 days should be cached
    assert ("NYC", "2026-02-13") in _wu_forecast_cache
    assert ("NYC", "2026-02-14") in _wu_forecast_cache
    assert ("NYC", "2026-02-15") in _wu_forecast_cache
    assert _wu_forecast_cache[("NYC", "2026-02-15")] == 45.0

    _wu_forecast_cache.clear()


@pytest.mark.asyncio
async def test_fetch_wu_forecast_cache_hit():
    """Cached value should be returned without API call."""
    _wu_forecast_cache.clear()
    _wu_forecast_cache[("NYC", "2026-02-13")] = 42.0

    mock_client = AsyncMock(spec=httpx.AsyncClient)
    mock_client.get = AsyncMock()

    result = await fetch_wu_forecast("NYC", "2026-02-13", client=mock_client)
    assert result == 42.0
    mock_client.get.assert_not_called()

    _wu_forecast_cache.clear()


@pytest.mark.asyncio
async def test_fetch_wu_forecast_unknown_city():
    """Unknown city should return None without making HTTP request."""
    _wu_forecast_cache.clear()
    result = await fetch_wu_forecast("UNKNOWN_CITY", "2026-02-13")
    assert result is None
    _wu_forecast_cache.clear()


def test_wu_forecast_anchoring():
    """WU forecast anchoring should shift ensemble mean toward WU forecast."""
    from weather.open_meteo import EnsembleForecast
    from weather.probability_model import compute_bucket_probabilities

    rng = np.random.default_rng(42)
    members = list(rng.normal(37.0, 3.0, 82))
    forecast = EnsembleForecast(
        city="NYC",
        station_icao="KLGA",
        forecast_date="2026-02-13",
        gefs_daily_max=members[:31],
        ecmwf_daily_max=members[31:],
        all_members=members,
    )

    result_no_wu = compute_bucket_probabilities(forecast)
    result_with_wu = compute_bucket_probabilities(
        forecast, wu_forecast_high=45.0, wu_forecast_weight=0.5
    )

    # With WU forecast=45 and weight=0.5, mean should shift toward 45
    assert result_with_wu.ensemble_mean > result_no_wu.ensemble_mean, (
        f"WU forecast should shift mean upward: "
        f"{result_with_wu.ensemble_mean} vs {result_no_wu.ensemble_mean}"
    )
    assert result_with_wu.wu_forecast_high == 45.0
    assert result_with_wu.wu_forecast_shift > 0
