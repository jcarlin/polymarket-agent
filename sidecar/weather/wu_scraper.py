"""Weather Underground actual high temperature fetcher.

Uses the Weather.com observations API (WU's parent company) to get hourly
observations for a station/date, then computes daily high as max(temps).

WU's website is an Angular SPA that doesn't serve temperature data in
server-rendered HTML, so we use the JSON API directly instead of scraping.
"""

import logging
from typing import Optional

import httpx

logger = logging.getLogger("weather.wu_scraper")

# Module-level cache: (icao, date_str) -> temperature in Fahrenheit
_wu_cache: dict[tuple[str, str], Optional[float]] = {}

# Forecast cache: (city, date_str) -> forecast high in Fahrenheit
_wu_forecast_cache: dict[tuple[str, str], Optional[float]] = {}

# ICAO station to (state, city) for WU URL construction (kept for backwards compat)
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

# Weather.com public API key (embedded in WU frontend JavaScript)
_WC_API_KEY = "e1f10a1e78da46f5b10a1e78da96f525"


def icao_to_wu_path(icao: str) -> Optional[tuple[str, str]]:
    """Map an ICAO station code to (state, city) tuple for WU URLs.

    Returns None if the ICAO code is not in the known mapping.
    """
    return _ICAO_TO_WU.get(icao)


def _build_api_url(icao: str, date: str) -> str:
    """Build Weather.com historical observations API URL.

    Args:
        icao: Airport ICAO code (e.g. "KLGA")
        date: Date in YYYY-MM-DD format

    Returns:
        API URL for hourly observations on that date.
    """
    date_compact = date.replace("-", "")  # YYYYMMDD
    return (
        f"https://api.weather.com/v1/location/{icao}:9:US/observations/historical.json"
        f"?apiKey={_WC_API_KEY}&units=e&startDate={date_compact}&endDate={date_compact}"
    )


def _extract_daily_high(data: dict) -> Optional[float]:
    """Extract daily high temperature from Weather.com API response.

    The API returns hourly observations. Daily high = max(temp) across all hours.

    Args:
        data: Parsed JSON response from Weather.com API

    Returns:
        Daily high temperature in Fahrenheit, or None if no valid temps found.
    """
    observations = data.get("observations", [])
    if not observations:
        return None

    temps = [
        obs["temp"]
        for obs in observations
        if obs.get("temp") is not None
    ]

    if not temps:
        return None

    return float(max(temps))


async def fetch_wu_actual(
    icao: str,
    date: str,
    client: Optional[httpx.AsyncClient] = None,
) -> Optional[float]:
    """Fetch the actual daily high temperature from Weather.com API.

    Uses the Weather.com observations API (WU's parent company) to get
    hourly observations, then returns the maximum temperature as the daily high.

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

    if icao not in _ICAO_TO_WU:
        logger.warning("Unknown ICAO station: %s", icao)
        _wu_cache[cache_key] = None
        return None

    url = _build_api_url(icao, date)

    close_client = False
    if client is None:
        client = httpx.AsyncClient(timeout=15.0)
        close_client = True

    try:
        resp = await client.get(url, headers={
            "User-Agent": "Mozilla/5.0 (compatible; weather-agent/1.0)",
        })
        resp.raise_for_status()
        data = resp.json()

        temp = _extract_daily_high(data)
        if temp is not None:
            logger.info("WU actual for %s on %s: %.0f°F (from %d observations)",
                        icao, date, temp, len(data.get("observations", [])))
        else:
            logger.warning("No temperature data in API response for %s on %s", icao, date)
        _wu_cache[cache_key] = temp
        return temp
    except Exception as e:
        logger.warning("WU fetch failed for %s on %s: %s", icao, date, e)
        _wu_cache[cache_key] = None
        return None
    finally:
        if close_client:
            await client.aclose()


def _build_forecast_api_url(lat: float, lon: float) -> str:
    """Build Weather.com 5-day forecast API URL using geocode.

    The v3 forecast API uses lat/lon (not ICAO station codes).

    Args:
        lat: Latitude
        lon: Longitude

    Returns:
        API URL for 5-day daily forecast.
    """
    return (
        f"https://api.weather.com/v3/wx/forecast/daily/5day"
        f"?geocode={lat},{lon}&format=json&units=e&language=en-US"
        f"&apiKey={_WC_API_KEY}"
    )


def _extract_forecast_highs(data: dict) -> list[tuple[str, float]]:
    """Extract daily high temperatures from Weather.com 5-day forecast response.

    Args:
        data: Parsed JSON response from Weather.com forecast API

    Returns:
        List of (date_str YYYY-MM-DD, temp_f) tuples for each day with valid data.
    """
    results = []
    max_temps = data.get("calendarDayTemperatureMax", [])
    valid_dates = data.get("validTimeLocal", [])

    if not max_temps or not valid_dates:
        return results

    for i, temp in enumerate(max_temps):
        if i >= len(valid_dates):
            break
        if temp is None:
            continue
        # validTimeLocal is like "2026-02-13T07:00:00-0500" — extract YYYY-MM-DD
        date_str = valid_dates[i][:10] if valid_dates[i] else None
        if date_str:
            results.append((date_str, float(temp)))

    return results


async def fetch_wu_forecast(
    city: str,
    date: str,
    client: Optional[httpx.AsyncClient] = None,
) -> Optional[float]:
    """Fetch WU/Weather.com forecast high for a city and date.

    Uses the Weather.com 5-day daily forecast API. On first call for a city,
    fetches all 5 days and caches them. Returns the requested date's high.

    Args:
        city: City code (e.g. "NYC")
        date: Date string in YYYY-MM-DD format
        client: Optional httpx.AsyncClient to reuse

    Returns:
        Forecast high temperature in Fahrenheit, or None if unavailable.
    """
    cache_key = (city, date)
    if cache_key in _wu_forecast_cache:
        return _wu_forecast_cache[cache_key]

    # Lazy import to avoid circular dependency
    try:
        from weather.open_meteo import CITY_CONFIGS
    except ImportError:
        logger.warning("Cannot import CITY_CONFIGS for WU forecast")
        _wu_forecast_cache[cache_key] = None
        return None

    if city not in CITY_CONFIGS:
        logger.warning("Unknown city for WU forecast: %s", city)
        _wu_forecast_cache[cache_key] = None
        return None

    config = CITY_CONFIGS[city]
    url = _build_forecast_api_url(config.lat, config.lon)

    close_client = False
    if client is None:
        client = httpx.AsyncClient(timeout=15.0)
        close_client = True

    try:
        resp = await client.get(url, headers={
            "User-Agent": "Mozilla/5.0 (compatible; weather-agent/1.0)",
        })
        resp.raise_for_status()
        data = resp.json()

        highs = _extract_forecast_highs(data)
        # Cache all returned days
        for d, temp in highs:
            _wu_forecast_cache[(city, d)] = temp
            logger.debug("WU forecast cached %s/%s: %.0f°F", city, d, temp)

        # If requested date wasn't in the response, cache as None
        if cache_key not in _wu_forecast_cache:
            _wu_forecast_cache[cache_key] = None

        result = _wu_forecast_cache.get(cache_key)
        if result is not None:
            logger.info("WU forecast for %s on %s: %.0f°F", city, date, result)
        else:
            logger.debug("WU forecast not available for %s on %s (outside 5-day window)", city, date)
        return result
    except Exception as e:
        logger.warning("WU forecast fetch failed for %s on %s: %s", city, date, e)
        _wu_forecast_cache[cache_key] = None
        return None
    finally:
        if close_client:
            await client.aclose()
