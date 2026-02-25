"""Tests for NWS API client."""

import pytest
import httpx

from weather.nws import (
    fetch_nws_forecast,
    clear_grid_cache,
)


@pytest.fixture(autouse=True)
def _clear_cache():
    """Clear grid cache before each test."""
    clear_grid_cache()
    yield
    clear_grid_cache()


# Sample NWS /points response
POINTS_RESPONSE = {
    "properties": {
        "forecast": "https://api.weather.gov/gridpoints/OKX/33,37/forecast",
        "forecastHourly": "https://api.weather.gov/gridpoints/OKX/33,37/forecast/hourly",
        "forecastGridData": "https://api.weather.gov/gridpoints/OKX/33,37",
    }
}

# Sample NWS forecast response
FORECAST_RESPONSE = {
    "properties": {
        "periods": [
            {
                "number": 1,
                "name": "Today",
                "startTime": "2026-02-12T06:00:00-05:00",
                "endTime": "2026-02-12T18:00:00-05:00",
                "isDaytime": True,
                "temperature": 37,
                "temperatureUnit": "F",
            },
            {
                "number": 2,
                "name": "Tonight",
                "startTime": "2026-02-12T18:00:00-05:00",
                "endTime": "2026-02-13T06:00:00-05:00",
                "isDaytime": False,
                "temperature": 28,
                "temperatureUnit": "F",
            },
            {
                "number": 3,
                "name": "Thursday",
                "startTime": "2026-02-13T06:00:00-05:00",
                "endTime": "2026-02-13T18:00:00-05:00",
                "isDaytime": True,
                "temperature": 42,
                "temperatureUnit": "F",
            },
        ]
    }
}


@pytest.mark.asyncio
async def test_fetch_nws_forecast_success():
    """Should return temperature for matching daytime period."""
    transport = httpx.MockTransport(
        lambda req: _mock_handler(req)
    )
    async with httpx.AsyncClient(transport=transport) as session:
        result = await fetch_nws_forecast(40.7728, -73.8740, "2026-02-12", session)
        assert result == 37.0


@pytest.mark.asyncio
async def test_fetch_nws_forecast_future_date():
    """Should return temperature for a future date in the forecast."""
    transport = httpx.MockTransport(
        lambda req: _mock_handler(req)
    )
    async with httpx.AsyncClient(transport=transport) as session:
        result = await fetch_nws_forecast(40.7728, -73.8740, "2026-02-13", session)
        assert result == 42.0


@pytest.mark.asyncio
async def test_fetch_nws_forecast_date_not_found():
    """Should return None when target date not in forecast periods."""
    transport = httpx.MockTransport(
        lambda req: _mock_handler(req)
    )
    async with httpx.AsyncClient(transport=transport) as session:
        result = await fetch_nws_forecast(40.7728, -73.8740, "2026-03-01", session)
        assert result is None


@pytest.mark.asyncio
async def test_fetch_nws_forecast_api_failure():
    """Should return None on API failure without raising."""
    transport = httpx.MockTransport(
        lambda req: httpx.Response(500, text="Internal Server Error")
    )
    async with httpx.AsyncClient(transport=transport) as session:
        result = await fetch_nws_forecast(40.7728, -73.8740, "2026-02-12", session)
        assert result is None


@pytest.mark.asyncio
async def test_grid_cache():
    """Grid URL should be cached after first request."""
    call_count = 0

    def counting_handler(req: httpx.Request) -> httpx.Response:
        nonlocal call_count
        if "/points/" in str(req.url):
            call_count += 1
        return _mock_handler(req)

    transport = httpx.MockTransport(counting_handler)
    async with httpx.AsyncClient(transport=transport) as session:
        # First call should hit /points
        await fetch_nws_forecast(40.7728, -73.8740, "2026-02-12", session)
        assert call_count == 1

        # Second call with same coords should use cache
        await fetch_nws_forecast(40.7728, -73.8740, "2026-02-13", session)
        assert call_count == 1  # No additional /points call


@pytest.mark.asyncio
async def test_celsius_conversion():
    """Should convert Celsius temperatures to Fahrenheit."""
    celsius_response = {
        "properties": {
            "periods": [
                {
                    "startTime": "2026-02-12T06:00:00-05:00",
                    "isDaytime": True,
                    "temperature": 0,
                    "temperatureUnit": "C",
                }
            ]
        }
    }

    def handler(req: httpx.Request) -> httpx.Response:
        if "/points/" in str(req.url):
            return httpx.Response(200, json=POINTS_RESPONSE)
        return httpx.Response(200, json=celsius_response)

    transport = httpx.MockTransport(handler)
    async with httpx.AsyncClient(transport=transport) as session:
        result = await fetch_nws_forecast(40.7728, -73.8740, "2026-02-12", session)
        assert result == 32.0  # 0°C = 32°F


@pytest.mark.asyncio
async def test_nighttime_periods_skipped():
    """Should only match daytime periods."""
    night_only_response = {
        "properties": {
            "periods": [
                {
                    "startTime": "2026-02-12T18:00:00-05:00",
                    "isDaytime": False,
                    "temperature": 28,
                    "temperatureUnit": "F",
                }
            ]
        }
    }

    def handler(req: httpx.Request) -> httpx.Response:
        if "/points/" in str(req.url):
            return httpx.Response(200, json=POINTS_RESPONSE)
        return httpx.Response(200, json=night_only_response)

    transport = httpx.MockTransport(handler)
    async with httpx.AsyncClient(transport=transport) as session:
        result = await fetch_nws_forecast(40.7728, -73.8740, "2026-02-12", session)
        assert result is None  # No daytime period for this date


def _mock_handler(req: httpx.Request) -> httpx.Response:
    """Mock handler for NWS API requests."""
    url = str(req.url)
    if "/points/" in url:
        return httpx.Response(200, json=POINTS_RESPONSE)
    if "/forecast" in url:
        return httpx.Response(200, json=FORECAST_RESPONSE)
    return httpx.Response(404)
