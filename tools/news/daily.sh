#!/bin/sh
# The daily news run: weather (optional) -> briefing -> publish to LifeUI -> archive to your notes (optional);
# on Sundays also the weekly digest, on the 1st also the monthly feed tuning.
# Run it once a day from the repo folder (tools/server/lifeui-news.timer does that on a Linux server):
#     sh tools/news/daily.sh
# Settings come from the environment or <repo>/.env.agent:
#     LIFEUI_URL, LIFEUI_RUNNER_TOKEN       where to publish (required)
#     LIFEUI_TIMEZONE                       your time zone (default UTC)
#     NEWS_STATE                            working folder (default <repo>/.news-state)
#     NEWS_ARCHIVE_DIR                      a Markdown notes folder (e.g. an Obsidian vault): writes News/<date>.md
#     WEATHER_IPMA_CITY, WEATHER_IPMA_AREA, WEATHER_PLACE, WEATHER_LANG   weather (Portugal, IPMA), optional
#     NEWS_FEEDBACK=dry-run                 on the 1st, only show the proposed tuning
set -eu
REPO=$(cd "$(dirname "$0")/../.." && pwd)
cd "$REPO"
if [ -f .env.agent ]; then set -a; . ./.env.agent; set +a; fi
PY="${PYTHON:-$REPO/.venv/bin/python}"
[ -x "$PY" ] || PY=python3
STATE="${NEWS_STATE:-$REPO/.news-state}"
mkdir -p "$STATE"
DATE=$(TZ="${LIFEUI_TIMEZONE:-UTC}" date +%F)
DOW=$(TZ="${LIFEUI_TIMEZONE:-UTC}" date +%u)   # 7 = Sunday
DOM=$(TZ="${LIFEUI_TIMEZONE:-UTC}" date +%d)

WEATHER=""
if [ -n "${WEATHER_IPMA_CITY:-}" ]; then
  if "$PY" -I tools/news/weather.py fetch --lang "${WEATHER_LANG:-en}" --out "$STATE/weather-$DATE.json" --cache "$STATE/weather-last-good.json"; then
    WEATHER="--weather $STATE/weather-$DATE.json"
  fi
fi

if [ "$DOM" = "01" ]; then
  if [ "${NEWS_FEEDBACK:-}" = "dry-run" ]; then
    "$PY" -I tools/news/extras.py feedback --state "$STATE" --dry-run || true
  else
    "$PY" -I tools/news/extras.py feedback --state "$STATE" || true
  fi
fi

# shellcheck disable=SC2086
"$PY" -I tools/news/news.py run --db "$STATE/news.db" --out "$STATE/$DATE.json" $WEATHER --tuning "$STATE/tuning.yaml"
"$PY" -I tools/news/publish.py daily --state "$STATE" --date "$DATE"
if [ -n "${NEWS_ARCHIVE_DIR:-}" ]; then
  "$PY" -I tools/news/extras.py archive --briefing "$STATE/$DATE.published.json" --vault "$NEWS_ARCHIVE_DIR"
fi

if [ "$DOW" = "7" ]; then
  "$PY" -I tools/news/extras.py weekly --out "$STATE/$DATE-weekly.json" --items-out "$STATE/$DATE-weekly-items.json"
  "$PY" -I tools/news/publish.py file "$STATE/$DATE-weekly.json"
  if [ -n "${NEWS_ARCHIVE_DIR:-}" ]; then
    "$PY" -I tools/news/extras.py archive --briefing "$STATE/$DATE-weekly.json" --weekly-items "$STATE/$DATE-weekly-items.json" --vault "$NEWS_ARCHIVE_DIR"
  fi
fi
