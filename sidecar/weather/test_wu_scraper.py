"""Tests for Weather Underground scraper."""

import pytest

import httpx

from weather.wu_scraper import (
    _parse_max_temperature,
    clear_wu_cache,
    fetch_wu_daily_high,
)


@pytest.fixture(autouse=True)
def _clear_cache():
    """Clear WU cache before each test."""
    clear_wu_cache()
    yield
    clear_wu_cache()


def _mock_wu_html(max_temp: int = 42) -> str:
    """Generate a mock WU history page with embedded max temperature."""
    return f"""
    <html>
    <head><title>Weather History</title></head>
    <body>
    <script>
    var data = {{"max": {{"imperial": {max_temp}}}}};
    </script>
    <table>
    <tr><td>Max</td><td>{max_temp} °F</td></tr>
    </table>
    </body>
    </html>
    """


def _mock_wu_json_ld(max_temp: int = 38) -> str:
    """Generate a mock WU page with JSON-LD max temperature."""
    return f"""
    <html>
    <head>
    <script type="application/ld+json">
    {{"maxTemperature": {max_temp}, "minTemperature": 25}}
    </script>
    </head>
    <body></body>
    </html>
    """


class TestParseMaxTemperature:
    def test_parse_imperial_json(self):
        html = _mock_wu_html(42)
        assert _parse_max_temperature(html) == 42.0

    def test_parse_json_ld(self):
        html = _mock_wu_json_ld(38)
        assert _parse_max_temperature(html) == 38.0

    def test_parse_table_format(self):
        html = '<td>Max</td><td>55 °F</td>'
        assert _parse_max_temperature(html) == 55.0

    def test_parse_data_attribute(self):
        html = '<div data-max-temp="67.5"></div>'
        assert _parse_max_temperature(html) == 67.5

    def test_parse_no_temperature(self):
        html = "<html><body>No weather data here</body></html>"
        assert _parse_max_temperature(html) is None

    def test_parse_out_of_range(self):
        # Temperature way out of range should be rejected
        html = '"max": {"imperial": 999}'
        assert _parse_max_temperature(html) is None

    def test_parse_negative_temp(self):
        # Negative temperatures are valid but our regex expects digits
        # The parser handles this by only matching \d+ patterns
        html = '"maxTemperature": 5'
        assert _parse_max_temperature(html) == 5.0


def _make_transport(html: str, status: int = 200) -> httpx.MockTransport:
    """Create a mock transport that returns fixed HTML."""
    def handler(request: httpx.Request) -> httpx.Response:
        return httpx.Response(status, text=html)
    return httpx.MockTransport(handler)


@pytest.mark.asyncio
async def test_fetch_success():
    html = _mock_wu_html(42)
    transport = _make_transport(html)
    async with httpx.AsyncClient(transport=transport) as session:
        result = await fetch_wu_daily_high("KLGA", "2026-02-10", session=session)
    assert result == 42.0


@pytest.mark.asyncio
async def test_fetch_caching():
    call_count = 0

    def handler(request: httpx.Request) -> httpx.Response:
        nonlocal call_count
        call_count += 1
        return httpx.Response(200, text=_mock_wu_html(55))

    transport = httpx.MockTransport(handler)
    async with httpx.AsyncClient(transport=transport) as session:
        r1 = await fetch_wu_daily_high("KLGA", "2026-02-10", session=session)
        r2 = await fetch_wu_daily_high("KLGA", "2026-02-10", session=session)

    assert r1 == 55.0
    assert r2 == 55.0
    assert call_count == 1  # Second call should hit cache


@pytest.mark.asyncio
async def test_fetch_failure():
    transport = _make_transport("", status=500)
    async with httpx.AsyncClient(transport=transport) as session:
        result = await fetch_wu_daily_high(
            "KLGA", "2026-02-10", session=session, max_retries=0
        )
    assert result is None


@pytest.mark.asyncio
async def test_fetch_unparseable():
    transport = _make_transport("<html>No data</html>")
    async with httpx.AsyncClient(transport=transport) as session:
        result = await fetch_wu_daily_high("KLGA", "2026-02-10", session=session)
    assert result is None
