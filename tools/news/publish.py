"""Publish a briefing JSON to LifeUI (POST {LIFEUI_URL}/api/runner/news with the runner token).

    python -I tools/news/publish.py daily  --state DIR [--date YYYY-MM-DD]   DIR/<date>.json + today's calendar and
                                                                           the feedback report -> DIR/<date>.published.json
    python -I tools/news/publish.py file   PATH                            publish PATH as it is (the weekly digest)

Env: LIFEUI_URL, LIFEUI_RUNNER_TOKEN (Agent tab > Settings > New token), GCAL_ICS_URL (optional, for the calendar),
LIFEUI_TIMEZONE. Retries with backoff while offline. Exit 0 = published (or nothing to publish), 1 = refused / failed.
"""
import argparse
import datetime as dt
import json
import os
import subprocess
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import news  # noqa: E402  (LOCAL_TZ)


def load(path: Path):
    try:
        return json.loads(path.read_text())
    except (OSError, ValueError):
        return None


def calendar(state: Path, date: str) -> list | None:
    if not os.environ.get("GCAL_ICS_URL"):
        return None
    out = state / f"calendar-{date}.json"
    r = subprocess.run([sys.executable, "-I", str(HERE / "extras.py"), "calendar", "--out", str(out)],
                       capture_output=True, text=True, timeout=120)
    if r.returncode != 0:
        print(f"calendar: failed ({(r.stderr or r.stdout).strip()[-200:]})", file=sys.stderr)
        return None
    return load(out)


def daily(state: Path, date: str) -> dict | None:
    briefing = load(state / f"{date}.json")
    if briefing is None:
        return None
    extras = dict(briefing.get("extras") or {})
    cal = calendar(state, date)
    if cal is not None:
        extras["calendar"] = cal
    report = load(state / f"feedback-{date}.json")
    if report and not report.get("dry_run"):
        extras["feedback"] = report
    briefing["extras"] = extras
    briefing.pop("sources", None)
    tmp = state / f"{date}.published.tmp"
    tmp.write_text(json.dumps(briefing, indent=2, ensure_ascii=False))
    tmp.replace(state / f"{date}.published.json")
    return briefing


def post(briefing: dict) -> int:
    briefing.pop("sources", None)
    body = json.dumps(briefing).encode()
    url = os.environ["LIFEUI_URL"].rstrip("/") + "/api/runner/news"
    last = None
    for wait in (0, 10, 30, 90):
        time.sleep(wait)
        req = urllib.request.Request(url, data=body, method="POST", headers={
            "Authorization": f"Bearer {os.environ['LIFEUI_RUNNER_TOKEN']}", "Content-Type": "application/json",
            "User-Agent": "lifeui-news"})
        try:
            with urllib.request.urlopen(req, timeout=60) as resp:
                b = json.load(resp)["briefing"]
            print(f"published {b.get('type', 'daily')} {b['date']}: {b['items']} items")
            return 0
        except urllib.error.HTTPError as e:
            detail = e.read().decode(errors="replace")[:300]
            if 400 <= e.code < 500:
                print(f"refused ({e.code}): {detail}", file=sys.stderr)
                return 1
            last = f"{e.code} {detail}"
        except OSError as e:
            last = str(e)
    print(f"gave up: {last}", file=sys.stderr)
    return 1


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    d = sub.add_parser("daily")
    d.add_argument("--state", type=Path, required=True)
    d.add_argument("--date", default=dt.datetime.now(news.LOCAL_TZ).date().isoformat())
    f = sub.add_parser("file")
    f.add_argument("path", type=Path)
    args = ap.parse_args()
    if args.cmd == "daily":
        briefing = daily(args.state, args.date)
        if briefing is None:
            print(f"no briefing for {args.date} in {args.state}: nothing to publish")
            return 0
    else:
        briefing = load(args.path)
        if briefing is None:
            print(f"cannot read {args.path}", file=sys.stderr)
            return 1
    return post(briefing)


if __name__ == "__main__":
    sys.exit(main())
