"""Weather Underground scraper for historical daily high temperatures."""

import logging
import re
from typing import Optional

import httpx

logger = logging.getLogger("weather.wu_scraper")

# Module-level cache: (icao, date_str) -> temperature in Fahrenheit
_wu_cache: dict[tuple[str, str], Optional[float]] = {}

# ICAO station to (state, city) for WU URL construction
_ICAO_TO_WU: dict[str, tuple[str, str]] = {
    "KLGA": ("ny", "new-york"),
    "KLAX": ("ca", "los-angeles"),
    "KORD": ("il", "chicago"),
    "KIAH": ("tx", "houston"),
    "KPHX": ("az", "phoenix"),
    "KPHL": ("pa", "philadelphia"),
    "KSAT": ("tx", "san-antonio"),
    "KSAN": ("ca", "san-diego"),
    "KDFW": ("tx", "dallas"),
    "KSJC": ("ca", "san-jose"),
    "KATL": ("ga", "atlanta"),
    "KMIA": ("fl", "miami"),
    "KBOS": ("ma", "boston"),
    "KSEA": ("wa", "seattle"),
    "KDEN": ("co", "denver"),
    "KDCA": ("va", "washington"),
    "KMSP": ("mn", "minneapolis"),
    "KDTW": ("mi", "detroit"),
    "KTPA": ("fl", "tampa"),
    "KSTL": ("mo", "st-louis"),
}

# Regex to extract the daily high from WU history page HTML.
# The page typically has "Max Temperature" followed by a temperature value.
_MAX_TEMP_PATTERN = re.compile(
    r'Max(?:imum)?\s*Temperature.*?(\d{1,3})\s*(?:&deg;|°)\s*F',
    re.IGNORECASE | re.DOTALL,
)

# Fallback: look for a temperature value near "Max" in a table-like structure
_MAX_TEMP_FALLBACK = re.compile(
    r'"maxTempValue"[^>]*>(\d{1,3})<',
    re.IGNORECASE,
)


def icao_to_wu_path(icao: str) -> Optional[tuple[str, str]]:
    """Map an ICAO station code to (state, city) tuple for WU URLs.

    Returns None if the ICAO code is not in the known mapping.
    """
    return _ICAO_TO_WU.get(icao)


def _parse_wu_html(html: str) -> Optional[float]:
    """Extract daily high temperature from Weather Underground history HTML.

    Returns temperature in Fahrenheit, or None if parsing fails.
    """
    match = _MAX_TEMP_PATTERN.search(html)
    if match:
        return float(match.group(1))

    match = _MAX_TEMP_FALLBACK.search(html)
    if match:
        return float(match.group(1))

    return None


def _build_wu_url(state: str, city: str, icao: str, date: str) -> str:
    """Build a Weather Underground history URL.

    date should be YYYY-MM-DD format.
    """
    # WU uses YYYY-M-D (no zero-padding required, but it works either way)
    parts = date.split("-")
    year = parts[0]
    month = str(int(parts[1]))  # strip leading zero
    day = str(int(parts[2]))    # strip leading zero
    return (
        f"https://www.wunderground.com/history/daily/us/"
        f"{state}/{city}/{icao}/date/{year}-{month}-{day}"
    )


async def fetch_wu_actual(
    icao: str,
    date: str,
    client: Optional[httpx.AsyncClient] = None,
) -> Optional[float]:
    """Fetch the actual daily high temperature from Weather Underground.

    Args:
        icao: Airport ICAO code (e.g. "KLGA")
        date: Date string in YYYY-MM-DD format
        client: Optional httpx.AsyncClient to reuse

    Returns:
        Temperature in Fahrenheit, or None if unavailable.
    """
    cache_key = (icao, date)
    if cache_key in _wu_cache:
        return _wu_cache[cache_key]

    path = icao_to_wu_path(icao)
    if path is None:
        logger.warning("Unknown ICAO station: %s", icao)
        _wu_cache[cache_key] = None
        return None

    state, city = path
    url = _build_wu_url(state, city, icao, date)

    close_client = False
    if client is None:
        client = httpx.AsyncClient(timeout=15.0)
        close_client = True

    try:
        resp = await client.get(url, headers={
            "User-Agent": "Mozilla/5.0 (compatible; weather-agent/1.0)",
        })
        resp.raise_for_status()
        temp = _parse_wu_html(resp.text)
        if temp is not None:
            logger.info("WU actual for %s on %s: %.0f°F", icao, date, temp)
        else:
            logger.warning("Could not parse WU page for %s on %s", icao, date)
        _wu_cache[cache_key] = temp
        return temp
    except Exception as e:
        logger.warning("WU fetch failed for %s on %s: %s", icao, date, e)
        _wu_cache[cache_key] = None
        return None
    finally:
        if close_client:
            await client.aclose()
