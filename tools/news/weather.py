"""Weather for the daily briefing from IPMA (Portugal's weather service; an example provider: Portugal only).

    WEATHER_IPMA_CITY=<globalIdLocal> WEATHER_IPMA_AREA=<idAreaAviso> WEATHER_PLACE=<name> \
    python -I tools/news/weather.py fetch --out weather.json --cache last-good.json [--lang en|pt] [--hints rules.yaml]
    python -I tools/news/weather.py selftest

Sources (IPMA open data, no key):
  daily     https://api.ipma.pt/open-data/forecast/meteorology/cities/daily/<city>.json
  hourly    https://api.ipma.pt/public-data/forecast/aggregate/<city>.json   (what ipma.pt's hourly page loads;
            idPeriodo 1 = hourly, 3 = 3-hourly, 24 = daily; -99 = no value)
  labels    https://api.ipma.pt/open-data/weather-type-classe.json, .../wind-speed-daily-classe.json
  warnings  https://api.ipma.pt/open-data/forecast/warnings/warnings_www.json  (your area, yellow or higher)
Find your city's globalIdLocal in https://api.ipma.pt/open-data/distrits-islands.json and the warning area code
(idAreaAviso, e.g. FAR for Faro) in the warnings file. IPMA times are UTC; everything written here is in
LIFEUI_TIMEZONE (set it to your Portuguese time zone: mainland, Madeira or Azores). Outside Portugal, write a fetcher
for your own weather service that returns the same object (see build()), or leave the weather out.

Output: {date, tmin, tmax, precip_prob, wind, weather_type_pt, hourly[], warnings[], summary, line, hints[], source,
fetched_at, stale}. `line` is the summary without its tips; `hints` come from the rules in weather_hints.yaml
(commute rain -> umbrella, heat, cold, wind, IPMA warnings as an "alert" banner) and are recomputed on a stale copy.
Each good result is also written to --cache; when IPMA can't be reached, the cached one is written with stale=true.
Exit codes: 0 fresh, 0 stale (prints "stale"), 1 nothing usable.
"""
import argparse
import datetime as dt
import json
import os
import sys
import tempfile
import time
import urllib.request
from pathlib import Path
from zoneinfo import ZoneInfo

HERE = Path(__file__).resolve().parent
HINTS = HERE / "weather_hints.yaml"
WEEKDAYS = ["mon", "tue", "wed", "thu", "fri", "sat", "sun"]


def local_tz() -> ZoneInfo:
    try:
        return ZoneInfo(os.environ.get("LIFEUI_TIMEZONE") or "UTC")
    except Exception:
        return ZoneInfo("UTC")


LOCAL_TZ = local_tz()
CITY = os.environ.get("WEATHER_IPMA_CITY", "").strip()      # IPMA globalIdLocal of your city
AREA = os.environ.get("WEATHER_IPMA_AREA", "").strip()      # IPMA warning area (idAreaAviso)
PLACE = os.environ.get("WEATHER_PLACE", "").strip()         # the name shown at the start of the weather line
API = "https://api.ipma.pt"


def urls(city: str) -> dict:
    return {
        "daily": f"{API}/open-data/forecast/meteorology/cities/daily/{city}.json",
        "hourly": f"{API}/public-data/forecast/aggregate/{city}.json",
        "types": f"{API}/open-data/weather-type-classe.json",
        "wind": f"{API}/open-data/wind-speed-daily-classe.json",
        "warnings": f"{API}/open-data/forecast/warnings/warnings_www.json",
    }


DAY_HOURS = range(9, 22)  # the hours shown on the weather card
LEVELS = {"yellow": 1, "orange": 2, "red": 3}
LEVEL_PT = {"yellow": "amarelo", "orange": "laranja", "red": "vermelho"}
PARTS = {"pt": [("de manhã", 9, 13), ("à tarde", 13, 19), ("à noite", 19, 22)],
         "en": [("in the morning", 9, 13), ("in the afternoon", 13, 19), ("in the evening", 19, 22)]}
BACKOFF = [0, 2, 5, 15]


def fetch_json(url: str, timeout: int = 20):
    last = None
    for wait in BACKOFF:
        time.sleep(wait)
        try:
            req = urllib.request.Request(url, headers={"User-Agent": "lifeui-news"})
            with urllib.request.urlopen(req, timeout=timeout) as resp:
                return json.load(resp)
        except Exception as e:  # network or bad JSON: retry, then give up
            last = e
    raise RuntimeError(f"{url}: {last}")


def num(value) -> float | None:
    try:
        v = float(value)
    except (TypeError, ValueError):
        return None
    return None if v <= -99 else v


def utc_to_local(stamp: str) -> dt.datetime:
    return dt.datetime.fromisoformat(stamp).replace(tzinfo=dt.timezone.utc).astimezone(LOCAL_TZ)


def build(raw: dict, today: dt.date, now: dt.datetime, lang: str = "en") -> dict:
    """Pure: raw IPMA documents -> the weather object."""
    types = {int(t["idWeatherType"]): t for t in raw["types"]["data"]}
    winds = {int(w["classWindSpeed"]): w for w in raw["wind"]["data"]}
    type_label = lambda i: (types.get(int(i)) or {}).get("descWeatherTypePT" if lang == "pt" else "descWeatherTypeEN") if i is not None else None

    day = next((d for d in raw["daily"]["data"] if d["forecastDate"] == today.isoformat()), None)
    if day is None:
        raise ValueError(f"no daily forecast for {today}")
    wind_class = winds.get(int(day["classWindSpeed"]), {})
    wind = {"class": wind_class.get("descClassWindSpeedDailyPT" if lang == "pt" else "descClassWindSpeedDailyEN"), "dir": day.get("predWindDir")}

    hourly = []
    for h in raw["hourly"]:
        if h.get("idPeriodo") != 1:
            continue
        t = utc_to_local(h["dataPrev"])
        if t.date() == today and t.hour in DAY_HOURS:
            hourly.append({"time": t.strftime("%H:%M"), "temp": num(h.get("tMed")), "precip_prob": num(h.get("probabilidadePrecipita")),
                           "wind_kmh": num(h.get("ffVento")), "wind_dir": h.get("ddVento"), "weather_type": type_label(h.get("idTipoTempo"))})
    hourly.sort(key=lambda h: h["time"])

    warnings = []
    for w in raw["warnings"]:
        level = LEVELS.get(w.get("awarenessLevelID"))
        if w.get("idAreaAviso") != AREA or not AREA or not level:
            continue
        start, end = utc_to_local(w["startTime"]), utc_to_local(w["endTime"])
        if end <= now:
            continue
        today_end = dt.datetime.combine(today, dt.time(22), LOCAL_TZ)
        warnings.append({"level": w["awarenessLevelID"], "type": w.get("awarenessTypeName"), "text": (w.get("text") or "").strip(),
                         "start": start.isoformat(timespec="minutes"), "end": end.isoformat(timespec="minutes"),
                         "today": start < today_end})
    warnings.sort(key=lambda w: (-LEVELS[w["level"]], w["start"]))

    weather = {
        "date": today.isoformat(),
        "tmin": num(day.get("tMin")),
        "tmax": num(day.get("tMax")),
        "precip_prob": num(day.get("precipitaProb")),
        "wind": wind,
        "weather_type_pt": (types.get(int(day["idWeatherType"])) or {}).get("descWeatherTypePT"),
        "weather_type": type_label(day["idWeatherType"]),  # in the briefing's language
        "hourly": hourly,
        "warnings": warnings,
        "source": "IPMA",
        "ipma_updated": utc_to_local(raw["daily"]["dataUpdate"]).isoformat(timespec="minutes") if raw["daily"].get("dataUpdate") else None,
        "fetched_at": now.isoformat(timespec="seconds"),
        "stale": False,
    }
    weather["summary"] = summary(weather, lang)
    weather["line"] = summary(weather, lang, tips=False)
    return weather


def summary(w: dict, lang: str = "en", tips: bool = True) -> str:
    """One line, e.g. 'Faro: 14–19 °C, 70% rain in the afternoon, moderate wind from W. Take an umbrella.'"""
    pt = lang == "pt"
    fmt = lambda v: f"{v:.0f}" if v is not None else "?"
    temps = f"{fmt(w['tmin'])}–{fmt(w['tmax'])} °C"
    parts = [f"{PLACE}: {temps}" if PLACE else temps]
    if w.get("weather_type_pt") and pt:
        parts.append(w["weather_type_pt"][0].lower() + w["weather_type_pt"][1:])
    # Rain: the wettest part of the day from the hourly data, else the daily probability.
    best = None
    for label, start, end in PARTS[lang]:
        probs = [h["precip_prob"] for h in w["hourly"] if h["precip_prob"] is not None and start <= int(h["time"][:2]) < end]
        if probs and (best is None or max(probs) > best[0]):
            best = (max(probs), label)
    rain = best[0] if best else w.get("precip_prob")
    if rain is not None and rain >= 30:
        parts.append(f"{rain:.0f}% {'chuva' if pt else 'rain'}" + (f" {best[1]}" if best else ""))
    elif rain is not None:
        parts.append("sem chuva" if pt else "no rain")
    if w["wind"].get("class"):
        parts.append(f"{'vento' if pt else 'wind'} {w['wind']['class'].lower()}" + (f" {'de' if pt else 'from'} {w['wind']['dir']}" if w["wind"].get("dir") else ""))
    line = ", ".join(parts) + "."
    if not tips:
        return line
    tips = []
    if rain is not None and rain >= 60:
        tips.append("Leva guarda-chuva." if pt else "Take an umbrella.")
    elif w["tmin"] is not None and w["tmin"] <= 10:
        tips.append("Leva casaco." if pt else "Take a coat.")
    today = [x for x in w["warnings"] if x.get("today")]
    if today:
        top = today[0]
        level = LEVEL_PT.get(top["level"], top["level"]) if pt else top["level"]
        tips.append(f"Aviso {level}: {top['type'].lower()}." if pt else f"{level.capitalize()} warning: {top['type'].lower()}.")
    return " ".join([line, *tips])


def hints(w: dict, config: dict, today: dt.date) -> list[dict]:
    """Pure: the weather object + the rules -> [{id, level, text}] in rule order."""
    commute = config.get("commute") or {}
    windows = []
    for span in commute.get("windows", []):
        a, b = span.split("-")
        windows.append((dt.time.fromisoformat(a.strip()), dt.time.fromisoformat(b.strip())))
    commuting = WEEKDAYS[today.weekday()] in commute.get("days", WEEKDAYS)

    def worst(rows, key):
        rows = [h for h in rows if h.get(key) is not None]
        if not rows:
            return None, ""
        top = max(rows, key=lambda h: h[key])
        return top[key], top["time"]

    in_window = [h for h in w.get("hourly", []) if commuting and any(a <= dt.time.fromisoformat(h["time"]) < b for a, b in windows)]
    rain_day, rain_day_at = worst(w.get("hourly", []), "precip_prob")
    if rain_day is None:
        rain_day, rain_day_at = w.get("precip_prob"), ""
    values = {
        "rain_commute": worst(in_window, "precip_prob"),
        "rain_day": (rain_day, rain_day_at),
        "tmax": (w.get("tmax"), ""),
        "tmin": (w.get("tmin"), ""),
        "wind_kmh": worst(w.get("hourly", []), "wind_kmh"),
    }
    out, matched = [], set()
    for rule in config.get("rules", []):
        if rule.get("unless") in matched:
            continue
        if rule["when"] == "warning":
            for warn in (x for x in w.get("warnings", []) if x.get("today")):
                out.append({"id": rule["id"], "level": rule.get("level", "alert"),
                            "text": rule["text"].format(level=warn["level"], level_pt=LEVEL_PT.get(warn["level"], warn["level"]),
                                                        type=(warn.get("type") or "").lower(), place=PLACE, value="", time="")})
                matched.add(rule["id"])
            continue
        value, at = values.get(rule["when"], (None, ""))
        if value is None or ("min" in rule and value < rule["min"]) or ("max" in rule and value > rule["max"]):
            continue
        out.append({"id": rule["id"], "level": rule.get("level", "info"), "text": rule["text"].format(value=f"{value:.0f}", time=at, level="", level_pt="", type="", place=PLACE)})
        matched.add(rule["id"])
    return out


def load_hints(path: Path | None) -> dict:
    if path is None or not path.exists():
        return {}
    import yaml  # noqa: PLC0415 (only needed when there are rules)
    return yaml.safe_load(path.read_text()) or {}


def fetch(out: Path, cache: Path, lang: str, getter=fetch_json, now: dt.datetime | None = None, rules: dict | None = None) -> str:
    now = now or dt.datetime.now(LOCAL_TZ)
    try:
        if not CITY:
            raise RuntimeError("WEATHER_IPMA_CITY is not set (your city's IPMA globalIdLocal)")
        raw = {key: getter(url) for key, url in urls(CITY).items()}
        weather = build(raw, now.date(), now, lang)
        status = "fresh"
        write(cache, weather)
    except Exception as e:
        if not cache.exists():
            raise RuntimeError(f"IPMA unreachable and no cached forecast: {e}")
        weather = json.loads(cache.read_text())
        weather.update(stale=True, stale_reason=str(e)[:300])
        weather.setdefault("line", summary(weather, lang, tips=False))
        status = "stale"
    try:
        weather["hints"] = hints(weather, rules or {}, dt.date.fromisoformat(weather["date"]))
    except Exception as e:  # a broken rules file must not cost the weather
        weather["hints"] = []
        weather["hints_error"] = str(e)[:200]
    write(out, weather)
    return f"{status}: {weather['summary']}"


def write(path: Path, data: dict) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_suffix(".tmp")
    tmp.write_text(json.dumps(data, indent=2, ensure_ascii=False) + "\n")
    tmp.replace(path)


# ------------------------------------------------------------------------------------------------ selftest

def fixture() -> dict:
    """A trimmed copy of real IPMA answers (2026-11-19 and a summer day for the DST check)."""
    hourly = [
        {"idPeriodo": 24, "dataPrev": "2026-11-19T00:00:00", "tMin": "12.8", "tMax": "18.4"},
        {"idPeriodo": 1, "dataPrev": "2026-11-19T08:00:00", "tMed": "13.0", "probabilidadePrecipita": "-99.0", "ffVento": "10.0", "ddVento": "W", "idTipoTempo": 2},
        {"idPeriodo": 1, "dataPrev": "2026-11-19T09:00:00", "tMed": "14.0", "probabilidadePrecipita": "10.0", "ffVento": "12.0", "ddVento": "W", "idTipoTempo": 2},
        {"idPeriodo": 1, "dataPrev": "2026-11-19T15:00:00", "tMed": "18.0", "probabilidadePrecipita": "70.0", "ffVento": "20.0", "ddVento": "NW", "idTipoTempo": 9},
        {"idPeriodo": 1, "dataPrev": "2026-11-19T22:00:00", "tMed": "13.5", "probabilidadePrecipita": "0.0", "ffVento": "8.0", "ddVento": "N", "idTipoTempo": 1},
        {"idPeriodo": 3, "dataPrev": "2026-11-19T12:00:00", "tMed": "17.0"},
        {"idPeriodo": 1, "dataPrev": "2026-07-01T08:00:00", "tMed": "25.0", "probabilidadePrecipita": "0.0", "ffVento": "5.0", "ddVento": "N", "idTipoTempo": 1},
    ]
    return {
        "daily": {"dataUpdate": "2026-11-19T07:56:08", "data": [
            {"forecastDate": "2026-11-19", "tMin": "12.8", "tMax": "18.4", "precipitaProb": "20.0", "predWindDir": "W", "classWindSpeed": 2, "idWeatherType": 9},
            {"forecastDate": "2026-07-01", "tMin": "8.0", "tMax": "31.0", "precipitaProb": "0.0", "predWindDir": "N", "classWindSpeed": 1, "idWeatherType": 1}]},
        "hourly": hourly,
        "types": {"data": [{"idWeatherType": 1, "descWeatherTypePT": "Céu limpo", "descWeatherTypeEN": "Clear sky"},
                           {"idWeatherType": 2, "descWeatherTypePT": "Céu pouco nublado", "descWeatherTypeEN": "Partly cloudy"},
                           {"idWeatherType": 9, "descWeatherTypePT": "Chuva/aguaceiros", "descWeatherTypeEN": "Rain/showers"}]},
        "wind": {"data": [{"classWindSpeed": "1", "descClassWindSpeedDailyPT": "Fraco", "descClassWindSpeedDailyEN": "Weak"},
                          {"classWindSpeed": "2", "descClassWindSpeedDailyPT": "Moderado", "descClassWindSpeedDailyEN": "Moderate"}]},
        "warnings": [
            {"idAreaAviso": "FAR", "awarenessLevelID": "yellow", "awarenessTypeName": "Agitação Marítima", "text": "Ondas de 4 a 5 m.", "startTime": "2026-11-19T12:25:00", "endTime": "2026-11-20T09:00:00"},
            {"idAreaAviso": "FAR", "awarenessLevelID": "orange", "awarenessTypeName": "Precipitação", "text": "Chuva forte.", "startTime": "2026-11-19T14:00:00", "endTime": "2026-11-19T18:00:00"},
            {"idAreaAviso": "FAR", "awarenessLevelID": "green", "awarenessTypeName": "Vento", "text": "", "startTime": "2026-11-19T00:00:00", "endTime": "2026-11-22T00:00:00"},
            {"idAreaAviso": "PTO", "awarenessLevelID": "red", "awarenessTypeName": "Vento", "text": "Porto.", "startTime": "2026-11-19T00:00:00", "endTime": "2026-11-22T00:00:00"},
            {"idAreaAviso": "FAR", "awarenessLevelID": "yellow", "awarenessTypeName": "Vento", "text": "Já passou.", "startTime": "2026-11-18T00:00:00", "endTime": "2026-11-19T06:00:00"}],
    }


def selftest() -> int:
    passed = failed = 0

    def check(ok: bool, name: str) -> None:
        nonlocal passed, failed
        passed, failed = (passed + 1, failed) if ok else (passed, failed + 1)
        print(("PASS " if ok else "FAIL ") + name)

    global LOCAL_TZ, CITY, AREA, PLACE
    LOCAL_TZ, CITY, AREA, PLACE = ZoneInfo("Atlantic/Madeira"), "2310300", "FAR", "Funchal"  # sample place (UTC+0, DST)
    raw = fixture()
    now = dt.datetime(2026, 11, 19, 9, 5, tzinfo=LOCAL_TZ)
    w = build(raw, now.date(), now, "pt")
    check((w["tmin"], w["tmax"], w["precip_prob"]) == (12.8, 18.4, 20.0), "daily min/max/precipitation come from the daily file")
    check(w["wind"] == {"class": "Moderado", "dir": "W"} and w["weather_type_pt"] == "Chuva/aguaceiros", "wind class and weather type get their PT labels")
    check([h["time"] for h in w["hourly"]] == ["09:00", "15:00"], "hourly keeps only today 09-21 local time, drops 3-hourly/daily rows and 22:00")
    check(w["hourly"][0]["precip_prob"] == 10.0 and num("-99.0") is None, "-99 means no value")
    check([x["level"] for x in w["warnings"]] == ["orange", "yellow"], "warnings: our area only, yellow or higher, not expired, worst first")
    check(w["warnings"][1]["start"] == "2026-11-19T12:25+00:00", "winter: UTC equals Madeira time")
    later = build({**raw, "warnings": [{**raw["warnings"][1], "startTime": "2026-11-21T12:00:00", "endTime": "2026-11-21T18:00:00"}]}, now.date(), now, "pt")
    check(later["warnings"][0]["today"] is False and "Aviso" not in later["summary"], "a warning for a later day is listed but not in today's summary")
    summer = utc_to_local("2026-07-01T08:00:00")
    check(summer.hour == 9 and summer.utcoffset() == dt.timedelta(hours=1), "summer: 08:00 UTC is 09:00 Madeira (DST)")
    ws = build(raw, dt.date(2026, 7, 1), dt.datetime(2026, 7, 1, 9, 0, tzinfo=LOCAL_TZ), "pt")
    check([h["time"] for h in ws["hourly"]] == ["09:00"], "summer hourly rows are shifted by DST")
    check(w["summary"].startswith("Funchal: 13–18 °C, chuva/aguaceiros, 70% chuva à tarde, vento moderado de W.") and "Leva guarda-chuva." in w["summary"]
          and "Aviso laranja: precipitação." in w["summary"], "PT summary: temps, wettest part of the day, wind, umbrella, worst warning")
    check(build(raw, now.date(), now, "en")["summary"].startswith("Funchal: 13–18 °C, 70% rain in the afternoon"), "EN summary")
    check("Take a coat." in summary({**ws, "warnings": []}) and "no rain" in summary(ws) and "sem chuva" in summary(ws, "pt"), "cold morning -> coat; no rain is said")
    check(w["line"] == w["summary"].split(" Leva")[0] and "Aviso" not in w["line"], "line = the summary without its tips")

    rules = load_hints(HINTS)
    check(rules["commute"]["windows"] == ["08:00-09:30", "17:00-19:00"], "weather_hints.yaml: the example commute windows")
    day = dt.date(2026, 11, 19)  # a Thursday
    hrs = lambda pairs: [{"time": t, "precip_prob": p, "wind_kmh": 10.0} for t, p in pairs]
    base = {"tmin": 12.0, "tmax": 18.0, "precip_prob": 20.0, "warnings": []}
    ids = lambda w_, cfg=rules, d=day: [h["id"] for h in hints(w_, cfg, d)]
    got = hints({**base, "hourly": hrs([("08:00", 10.0), ("09:00", 65.0), ("13:00", 90.0)])}, rules, day)
    check([h["id"] for h in got] == ["umbrella"] and got[0]["text"] == "Take an umbrella: 65% chance of rain at 09:00." and got[0]["level"] == "warn",
          "rain >= 60% at 09:00 (inside 08:00-09:30) -> umbrella; the 13:00 rain is not repeated as a day hint")
    check(ids({**base, "hourly": hrs([("09:00", 55.0), ("11:00", 95.0), ("19:00", 70.0)])}) == ["rain"],
          "rain only outside the windows (11:00, 19:00 is the window's end) -> day rain hint, no umbrella")
    check(ids({**base, "hourly": hrs([("08:00", 80.0)])}, rules, dt.date(2026, 11, 22)) == ["rain"], "commute days are respected (Sunday)")
    check(ids({**base, "tmax": 31.0, "hourly": []}) == ["heat"] and ids({**base, "tmax": 29.9, "hourly": []}) == [], "tmax >= 30 °C -> heat hint")
    check(ids({**base, "tmin": 4.0, "hourly": []}) == ["cold"], "tmin <= 5 °C -> cold hint")
    alert = hints({**base, "hourly": [], "warnings": [{"level": "orange", "type": "Precipitação", "today": True},
                                                     {"level": "yellow", "type": "Vento", "today": False}]}, rules, day)
    check([(h["id"], h["level"], h["text"]) for h in alert] == [("warning", "alert", "IPMA orange warning for Funchal: precipitação.")],
          "an IPMA warning for today -> alert banner; a later day's warning is not")
    check(hints({**base, "hourly": []}, {}, day) == [], "no rules file -> no hints")

    with tempfile.TemporaryDirectory() as tmp:
        out, cache = Path(tmp, "w.json"), Path(tmp, "last-good.json")
        fetch_fixture = lambda url: raw[next(k for k, u in urls(CITY).items() if u == url)]
        msg = fetch(out, cache, "pt", getter=fetch_fixture, now=now, rules=rules)
        check(msg.startswith("fresh") and json.loads(cache.read_text())["stale"] is False, "a good fetch is written and cached")
        check([h["id"] for h in json.loads(out.read_text())["hints"]] == ["rain", "warning", "warning"],
              "fetch adds the hints (fixture: 70% at 15:00 is outside the windows; two warnings today)")

        def offline(url):
            raise OSError("network is unreachable")
        msg = fetch(out, cache, "pt", getter=offline, now=now)
        got = json.loads(out.read_text())
        check(msg.startswith("stale") and got["stale"] is True and "unreachable" in got["stale_reason"] and got["tmax"] == 18.4,
              "offline: the cached forecast is written, marked stale")
        cache.unlink()
        CITY = ""
        try:
            fetch(out, cache, "en", getter=fetch_fixture, now=now)
            check(False, "no city configured fails")
        except RuntimeError as e:
            check("WEATHER_IPMA_CITY" in str(e), "no city configured fails with a clear message")
        CITY = "2310300"
        try:
            fetch(out, cache, "pt", getter=offline, now=now)
            check(False, "offline without a cache fails")
        except RuntimeError:
            check(True, "offline without a cache fails")

    print(f"{passed} passed, {failed} failed")
    return 1 if failed else 0


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    f = sub.add_parser("fetch", help="fetch today's forecast")
    f.add_argument("--out", type=Path, required=True)
    f.add_argument("--cache", type=Path, required=True)
    f.add_argument("--lang", choices=["en", "pt"], default="en")
    f.add_argument("--hints", type=Path, default=HINTS, help="hint rules (default: weather_hints.yaml next to this script)")
    sub.add_parser("selftest", help="offline checks on fixture data")
    args = ap.parse_args()
    if args.cmd == "selftest":
        return selftest()
    try:
        print(fetch(args.out, args.cache, args.lang, rules=load_hints(args.hints)))
        return 0
    except Exception as e:
        print(f"weather: {e}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
