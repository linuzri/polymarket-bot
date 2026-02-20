#!/usr/bin/env python3
"""
Multi-source weather verification system for Polymarket weather trading.

Fetches forecasts from 4 Open-Meteo models, applies station bias correction,
and identifies tradeable opportunities where 3+ models agree on the same bucket.
"""

import argparse
import json
import sys
import urllib.request
import urllib.parse
from datetime import datetime, timedelta
from typing import Optional

# City configurations: (name, lat, lon, station_icao, unit, bias_correction)
# bias_correction is ADDED to model forecast to match expected station reading
CITIES = {
    "NYC":          {"lat": 40.77, "lon": -73.87, "icao": "KLGA", "unit": "F", "bias": 1.0},
    "London":       {"lat": 51.51, "lon":   0.05, "icao": "EGLC", "unit": "C", "bias": 0.5},
    "Miami":        {"lat": 25.79, "lon": -80.29, "icao": "KMIA", "unit": "F", "bias": 1.0},
    "Dallas":       {"lat": 32.85, "lon": -96.85, "icao": "KDAL", "unit": "F", "bias": 1.0},
    "Chicago":      {"lat": 41.98, "lon": -87.90, "icao": "KORD", "unit": "F", "bias": 1.0},
    "Atlanta":      {"lat": 33.64, "lon": -84.43, "icao": "KATL", "unit": "F", "bias": 1.0},
    "Seattle":      {"lat": 47.45, "lon":-122.31, "icao": "KSEA", "unit": "F", "bias": 1.0},
    "Seoul":        {"lat": 37.56, "lon": 126.80, "icao": "RKSS", "unit": "C", "bias": 0.5},
    "Paris":        {"lat": 49.01, "lon":   2.55, "icao": "LFPG", "unit": "C", "bias": 0.5},
    "Toronto":      {"lat": 43.68, "lon": -79.63, "icao": "CYYZ", "unit": "C", "bias": 0.5},
    "Buenos Aires": {"lat":-34.82, "lon": -58.54, "icao": "SAEZ", "unit": "C", "bias": 0.5},
    "Ankara":       {"lat": 40.13, "lon":  32.00, "icao": "ESBA", "unit": "C", "bias": 0.5},
    "Wellington":   {"lat":-41.33, "lon": 174.81, "icao": "NZWN", "unit": "C", "bias": 0.5},
}

MODELS = ["best_match", "gfs_seamless", "icon_seamless", "ecmwf_ifs025"]


def c_to_f(c: float) -> float:
    return c * 9.0 / 5.0 + 32.0


def get_bucket(temp: float, unit: str) -> str:
    """Convert temperature to market bucket string.
    US (°F): 2°F ranges like '32-34°F'
    Celsius: 1°C ranges like '5-6°C'
    """
    if unit == "F":
        step = 2
        base = int(temp // step) * step
        return f"{base}-{base + step}°F"
    else:
        step = 1
        base = int(temp // step) * step
        if temp < 0 and temp != base:
            base -= 1
        return f"{base}-{base + step}°C"


def fetch_all_models(lat: float, lon: float, date_str: str) -> dict:
    """Fetch daily max temperature from all 4 models in a single API call."""
    models_param = ",".join(m for m in MODELS if m != "best_match")
    url = (
        f"https://api.open-meteo.com/v1/forecast?"
        f"latitude={lat}&longitude={lon}"
        f"&daily=temperature_2m_max&timezone=auto"
        f"&start_date={date_str}&end_date={date_str}"
        f"&models={models_param}"
    )
    try:
        with urllib.request.urlopen(url, timeout=20) as resp:
            data = json.loads(resp.read().decode())
        result = {}
        daily = data.get("daily", {})
        for key, vals in daily.items():
            if key == "time":
                continue
            # key like "temperature_2m_max_gfs_seamless" or "temperature_2m_max" (best_match)
            model = key.replace("temperature_2m_max_", "") if "_" in key.replace("temperature_2m_max", "", 1) else "best_match"
            if model == "temperature_2m_max":
                model = "best_match"
            if vals and vals[0] is not None:
                result[model] = float(vals[0])
        return result
    except Exception as e:
        print(f"  [warn] fetch error: {e}", file=sys.stderr)
        return {}


def analyze_city(city_name: str, date_str: str) -> dict:
    """Analyze one city: fetch all models in one call, apply bias, find consensus bucket."""
    cfg = CITIES[city_name]
    results = {}

    model_temps = fetch_all_models(cfg["lat"], cfg["lon"], date_str)
    for model, temp_c in model_temps.items():
        if cfg["unit"] == "F":
            temp = c_to_f(temp_c) + cfg["bias"]
        else:
            temp = temp_c + cfg["bias"]
        results[model] = {"temp": round(temp, 1), "bucket": get_bucket(temp, cfg["unit"])}

    if not results:
        return {"city": city_name, "error": "no data"}

    # Find consensus bucket
    bucket_counts = {}
    for m, r in results.items():
        b = r["bucket"]
        bucket_counts[b] = bucket_counts.get(b, 0) + 1

    best_bucket = max(bucket_counts, key=bucket_counts.get)
    consensus = bucket_counts[best_bucket]
    total = len(results)
    consensus_prob = consensus / total

    # Placeholder market price (would come from Polymarket API)
    market_price = 0.25  # default assumption
    edge = consensus_prob - market_price

    tradeable = consensus >= 3 and total >= 3

    return {
        "city": city_name,
        "date": date_str,
        "unit": cfg["unit"],
        "models": results,
        "bucket": best_bucket,
        "consensus": f"{consensus}/{total}",
        "consensus_prob": consensus_prob,
        "market_price": market_price,
        "edge": edge,
        "tradeable": tradeable,
    }


def format_table(results: list[dict]) -> str:
    """Format results as a readable table."""
    header = f"{'City':<15} {'Date':<12} {'Bucket':<12} {'Consensus':<10} {'Prob':<6} {'Mkt':<6} {'Edge':<7} {'Signal'}"
    sep = "-" * len(header)
    lines = [sep, header, sep]

    for r in results:
        if "error" in r:
            lines.append(f"{r['city']:<15} {'ERROR'}")
            continue
        signal = "[TRADE]" if r["tradeable"] else "[SKIP]"
        lines.append(
            f"{r['city']:<15} {r['date']:<12} {r['bucket']:<12} "
            f"{r['consensus']:<10} {r['consensus_prob']:<6.0%} "
            f"{r['market_price']:<6.2f} {r['edge']:<+7.0%} {signal}"
        )

    # Detail: per-model temps
    lines.append(sep)
    lines.append("\nModel Details:")
    for r in results:
        if "error" in r:
            continue
        lines.append(f"\n  {r['city']}:")
        for model in MODELS:
            if model in r["models"]:
                m = r["models"][model]
                marker = " <<<" if m["bucket"] == r["bucket"] else ""
                lines.append(f"    {model:<20} {m['temp']:>6.1f}°{r['unit']}  [{m['bucket']}]{marker}")

    lines.append("")
    return "\n".join(lines)


def run(date_str: Optional[str] = None, cities: Optional[list[str]] = None) -> list[dict]:
    """Main entry point for module use. Returns list of analysis results."""
    if date_str is None:
        date_str = (datetime.now() + timedelta(days=1)).strftime("%Y-%m-%d")
    if cities is None:
        cities = list(CITIES.keys())

    results = []
    for city in cities:
        if city not in CITIES:
            print(f"Unknown city: {city}", file=sys.stderr)
            continue
        results.append(analyze_city(city, date_str))

    return results


def main():
    parser = argparse.ArgumentParser(description="Multi-source weather verification for Polymarket")
    parser.add_argument("--date", default=None, help="Target date YYYY-MM-DD (default: tomorrow)")
    parser.add_argument("--cities", nargs="*", default=None, help="Cities to check (default: all)")
    args = parser.parse_args()

    date_str = args.date
    if date_str is None:
        date_str = (datetime.now() + timedelta(days=1)).strftime("%Y-%m-%d")

    def safe_print(msg):
        print(msg.encode('ascii', 'replace').decode('ascii'))

    safe_print(f"Multi-Source Weather Verification -- {date_str}")
    safe_print(f"   Models: {', '.join(MODELS)}")
    safe_print(f"   Bias correction: +1F (US), +0.5C (EU/intl)\n")

    results = run(date_str, args.cities)
    table = format_table(results)
    safe_print(table)

    tradeable = [r for r in results if r.get("tradeable")]
    safe_print(f"Tradeable opportunities: {len(tradeable)}/{len(results)}")

    if not tradeable:
        sys.exit(1)


if __name__ == "__main__":
    main()
