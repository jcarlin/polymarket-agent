"""Tests for Weather Underground scraper."""

from unittest.mock import AsyncMock

import httpx
import pytest

from weather.wu_scraper import (
    _parse_wu_html,
    _wu_cache,
    fetch_wu_actual,
    icao_to_wu_path,
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


def test_parse_wu_html():
    """Should extract temperature from WU HTML containing Max Temperature."""
    html = """
    <div class="observation-table">
        <table>
            <tr><td>Max Temperature</td><td>75&deg;F</td></tr>
            <tr><td>Min Temperature</td><td>55&deg;F</td></tr>
        </table>
    </div>
    """
    assert _parse_wu_html(html) == 75.0


def test_parse_wu_html_degree_symbol():
    """Should handle the actual degree symbol."""
    html = '<span>Maximum Temperature</span><span>82\u00b0F</span>'
    assert _parse_wu_html(html) == 82.0


def test_parse_wu_html_fallback():
    """Should use fallback pattern for maxTempValue."""
    html = '<span class="maxTempValue">91</span>'
    assert _parse_wu_html(html) == 91.0


def test_parse_wu_html_no_match():
    """Should return None if no temperature pattern found."""
    html = "<html><body>No weather data here</body></html>"
    assert _parse_wu_html(html) is None


@pytest.mark.asyncio
async def test_fetch_wu_actual_unknown_icao():
    """Unknown ICAO should return None without making HTTP request."""
    # Clear cache to avoid interference
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
async def test_fetch_wu_actual_caches_result():
    """Subsequent calls with the same args should use cache."""
    _wu_cache.clear()

    mock_response = AsyncMock()
    mock_response.status_code = 200
    mock_response.text = '<td>Max Temperature</td><td>68&deg;F</td>'
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
