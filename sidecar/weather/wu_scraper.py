"""Weather Underground historical data scraper.

Fetches actual observed daily high temperatures from Weather Underground
history pages. Used to validate our forecast model against the actual
resolution source (Polymarket resolves via WU, not NWS).
"""

import asyncio
import logging
import re
from typing import Optional

import httpx

logger = logging.getLogger("weather.wu_scraper")

WU_HISTORY_URL = "https://www.wunderground.com/history/daily/{icao}/date/{date}"
WU_USER_AGENT = (
    "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 "
    "(KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36"
)

# In-memory cache: (icao, date) -> observed high °F
_wu_cache: dict[tuple[str, str], float] = {}

# Rate limiting: minimum seconds between WU requests
_WU_RATE_LIMIT_SECS = 2.0
_last_request_time: float = 0.0


async def fetch_wu_daily_high(
    icao: str,
    date: str,
    session: Optional[httpx.AsyncClient] = None,
    max_retries: int = 2,
) -> Optional[float]:
    """Fetch the observed daily high temperature from Weather Underground.

    Args:
        icao: ICAO airport code (e.g., "KLGA")
        date: Date string in YYYY-MM-DD format
        session: Optional async HTTP client
        max_retries: Number of retry attempts

    Returns:
        Observed daily high temperature in °F, or None on failure.
    """
    global _last_request_time

    cache_key = (icao, date)
    if cache_key in _wu_cache:
        return _wu_cache[cache_key]

    close_session = False
    if session is None:
        session = httpx.AsyncClient(
            timeout=15.0,
            headers={"User-Agent": WU_USER_AGENT},
            follow_redirects=True,
        )
        close_session = True

    try:
        url = WU_HISTORY_URL.format(icao=icao, date=date)

        last_err: Optional[Exception] = None
        html: Optional[str] = None

        for attempt in range(max_retries + 1):
            try:
                # Rate limiting
                import time

                now = time.monotonic()
                elapsed = now - _last_request_time
                if elapsed < _WU_RATE_LIMIT_SECS and _last_request_time > 0:
                    await asyncio.sleep(_WU_RATE_LIMIT_SECS - elapsed)

                _last_request_time = time.monotonic()

                resp = await session.get(url)
                resp.raise_for_status()
                html = resp.text
                break
            except Exception as e:
                last_err = e
                if attempt < max_retries:
                    await asyncio.sleep(2.0 * (attempt + 1))

        if html is None:
            logger.warning(
                "WU fetch failed for %s on %s after %d retries: %s",
                icao, date, max_retries + 1, last_err,
            )
            return None

        temp = _parse_max_temperature(html)
        if temp is not None:
            _wu_cache[cache_key] = temp
            logger.info("WU actual high for %s on %s: %.0f°F", icao, date, temp)
        else:
            logger.warning("Could not parse max temperature from WU page for %s on %s", icao, date)

        return temp

    finally:
        if close_session:
            await session.aclose()


def _parse_max_temperature(html: str) -> Optional[float]:
    """Extract the daily maximum temperature from WU history page HTML.

    WU embeds weather data in the page. We look for common patterns:
    1. JSON-LD or embedded data with max temperature
    2. The "Max" row in the daily observations table
    """
    # Pattern 1: Look for temperature max in the history summary table
    # WU pages typically have "Max" temperature in a table with °F values
    # The pattern "Max</td>" or similar followed by a temperature value
    max_temp_patterns = [
        # Pattern: embedded JSON with maxTemperature or temperatureMax
        r'"maxTemperature"\s*:\s*(\d+\.?\d*)',
        r'"temperatureMax"\s*:\s*(\d+\.?\d*)',
        # Pattern: table cell with max temp - common WU format
        r'Max[^<]*</(?:td|span|div)>[^<]*<(?:td|span|div)[^>]*>\s*(\d+)\s*°?\s*F?',
        # Pattern: "Max Temp" or "Maximum" followed by number
        r'(?:Max(?:imum)?(?:\s+Temp(?:erature)?)?)[^\d]*(\d+)\s*°?\s*F',
        # Pattern: data attribute with max temp
        r'data-max[_-]?temp(?:erature)?="(\d+\.?\d*)"',
        # Pattern: WU-specific JSON in script tags
        r'"max"\s*:\s*\{\s*"imperial"\s*:\s*(\d+\.?\d*)',
    ]

    for pattern in max_temp_patterns:
        match = re.search(pattern, html, re.IGNORECASE | re.DOTALL)
        if match:
            try:
                temp = float(match.group(1))
                # Sanity check: temperature should be in reasonable range
                if -60 <= temp <= 140:
                    return temp
            except (ValueError, IndexError):
                continue

    return None


def clear_wu_cache() -> None:
    """Clear the WU data cache (useful for testing)."""
    _wu_cache.clear()
