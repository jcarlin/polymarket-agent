"""Open-Meteo Ensemble API client for weather forecasting."""

import asyncio
import logging
from dataclasses import dataclass, field
from datetime import datetime, timedelta
from zoneinfo import ZoneInfo
from typing import Optional

import httpx

logger = logging.getLogger("weather.open_meteo")

OPEN_METEO_ENSEMBLE_URL = "https://ensemble-api.open-meteo.com/v1/ensemble"


@dataclass
class CityConfig:
    name: str
    icao: str
    lat: float
    lon: float
    timezone: str


# 20 US cities with airport ICAO codes for Weather Underground resolution
CITY_CONFIGS: dict[str, CityConfig] = {
    "NYC": CityConfig("New York", "KLGA", 40.7728, -73.8740, "America/New_York"),
    "LAX": CityConfig("Los Angeles", "KLAX", 33.9425, -118.4081, "America/Los_Angeles"),
    "CHI": CityConfig("Chicago", "KORD", 41.9742, -87.9073, "America/Chicago"),
    "HOU": CityConfig("Houston", "KIAH", 29.9902, -95.3368, "America/Chicago"),
    "PHX": CityConfig("Phoenix", "KPHX", 33.4373, -112.0078, "America/Phoenix"),
    "PHL": CityConfig("Philadelphia", "KPHL", 39.8744, -75.2424, "America/New_York"),
    "SAN": CityConfig("San Antonio", "KSAT", 29.5337, -98.4698, "America/Chicago"),
    "SDG": CityConfig("San Diego", "KSAN", 32.7336, -117.1831, "America/Los_Angeles"),
    "DAL": CityConfig("Dallas", "KDFW", 32.8998, -97.0403, "America/Chicago"),
    "SJC": CityConfig("San Jose", "KSJC", 37.3626, -121.9291, "America/Los_Angeles"),
    "ATL": CityConfig("Atlanta", "KATL", 33.6407, -84.4277, "America/New_York"),
    "MIA": CityConfig("Miami", "KMIA", 25.7959, -80.2870, "America/New_York"),
    "BOS": CityConfig("Boston", "KBOS", 42.3656, -71.0096, "America/New_York"),
    "SEA": CityConfig("Seattle", "KSEA", 47.4502, -122.3088, "America/Los_Angeles"),
    "DEN": CityConfig("Denver", "KDEN", 39.8561, -104.6737, "America/Denver"),
    "DCA": CityConfig("Washington DC", "KDCA", 38.8512, -77.0402, "America/New_York"),
    "MSP": CityConfig("Minneapolis", "KMSP", 44.8848, -93.2223, "America/Chicago"),
    "DTW": CityConfig("Detroit", "KDTW", 42.2162, -83.3554, "America/New_York"),
    "TPA": CityConfig("Tampa", "KTPA", 27.9755, -82.5332, "America/New_York"),
    "STL": CityConfig("St. Louis", "KSTL", 38.7487, -90.3700, "America/Chicago"),
}


@dataclass
class EnsembleForecast:
    city: str
    station_icao: str
    forecast_date: str  # YYYY-MM-DD
    gefs_daily_max: list[float] = field(default_factory=list)  # 31 members
    ecmwf_daily_max: list[float] = field(default_factory=list)  # 51 members
    all_members: list[float] = field(default_factory=list)  # combined 82


def _celsius_to_fahrenheit(c: float) -> float:
    return c * 9.0 / 5.0 + 32.0


def _extract_daily_max_for_date(
    hourly_times: list[str],
    member_values: list[list[float]],
    target_date: str,
    tz: ZoneInfo,
) -> list[float]:
    """Extract the daily max temperature for each ensemble member on target_date in local time."""
    daily_maxes: list[float] = []
    for member_temps in member_values:
        member_max: Optional[float] = None
        for i, time_str in enumerate(hourly_times):
            # Open-Meteo returns times in UTC as ISO 8601
            utc_dt = datetime.fromisoformat(time_str.replace("Z", "+00:00"))
            if utc_dt.tzinfo is None:
                utc_dt = utc_dt.replace(tzinfo=ZoneInfo("UTC"))
            local_dt = utc_dt.astimezone(tz)
            if local_dt.strftime("%Y-%m-%d") == target_date:
                val = member_temps[i] if i < len(member_temps) else None
                if val is not None:
                    if member_max is None or val > member_max:
                        member_max = val
        if member_max is not None:
            daily_maxes.append(_celsius_to_fahrenheit(member_max))
    return daily_maxes


async def fetch_ensemble(
    city: str,
    date: str,
    session: Optional[httpx.AsyncClient] = None,
    max_retries: int = 3,
) -> Optional[EnsembleForecast]:
    """Fetch ensemble forecast from Open-Meteo for a city/date."""
    config = CITY_CONFIGS.get(city)
    if config is None:
        logger.warning("Unknown city: %s", city)
        return None

    tz = ZoneInfo(config.timezone)
    # Request target date and next day to cover full local-day hours
    target = datetime.strptime(date, "%Y-%m-%d")
    end_date = (target + timedelta(days=1)).strftime("%Y-%m-%d")

    params = {
        "latitude": config.lat,
        "longitude": config.lon,
        "hourly": "temperature_2m",
        "start_date": date,
        "end_date": end_date,
        "timezone": "UTC",
    }

    close_session = False
    if session is None:
        session = httpx.AsyncClient(timeout=30.0)
        close_session = True

    gefs_maxes: list[float] = []
    ecmwf_maxes: list[float] = []

    try:
        for model_name, model_param in [("gefs", "gfs_seamless"), ("ecmwf", "ecmwf_ifs025")]:
            last_err: Optional[Exception] = None
            resp = None
            for attempt in range(max_retries):
                try:
                    model_params = {**params, "models": model_param}
                    resp = await session.get(OPEN_METEO_ENSEMBLE_URL, params=model_params)
                    resp.raise_for_status()
                    break
                except Exception as e:
                    last_err = e
                    if attempt < max_retries - 1:
                        await asyncio.sleep(1.0 * (attempt + 1))
            else:
                logger.error(
                    "Failed to fetch %s for %s after %d retries: %s",
                    model_name, city, max_retries, last_err,
                )
                return None

            data = resp.json()
            hourly = data.get("hourly", {})
            times = hourly.get("time", [])

            # Extract member columns: temperature_2m_member01, temperature_2m_member02, ...
            member_values: list[list[float]] = []
            for key in sorted(hourly.keys()):
                if key.startswith("temperature_2m_member"):
                    member_values.append(hourly[key])

            daily_maxes = _extract_daily_max_for_date(times, member_values, date, tz)

            if model_name == "gefs":
                gefs_maxes = daily_maxes
            else:
                ecmwf_maxes = daily_maxes

        all_members = gefs_maxes + ecmwf_maxes
        return EnsembleForecast(
            city=city,
            station_icao=config.icao,
            forecast_date=date,
            gefs_daily_max=gefs_maxes,
            ecmwf_daily_max=ecmwf_maxes,
            all_members=all_members,
        )
    finally:
        if close_session:
            await session.aclose()


async def fetch_all_cities(
    date: str,
    cities: Optional[list[str]] = None,
    max_concurrent: int = 5,
) -> dict[str, EnsembleForecast]:
    """Fetch ensemble forecasts for multiple cities in parallel."""
    if cities is None:
        cities = list(CITY_CONFIGS.keys())

    semaphore = asyncio.Semaphore(max_concurrent)
    results: dict[str, EnsembleForecast] = {}

    async def fetch_one(city: str) -> None:
        async with semaphore:
            result = await fetch_ensemble(city, date)
            if result is not None:
                results[city] = result

    tasks = []
    for city in cities:
        tasks.append(asyncio.create_task(fetch_one(city)))
    await asyncio.gather(*tasks)

    return results
