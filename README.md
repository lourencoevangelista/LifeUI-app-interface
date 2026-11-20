# LifeUI

A personal, single-user life-management app you host yourself: a timeline of events, a knowledge base about your
life (people, projects, courses, documents, tasks), a daily news briefing about **your** interests, and a read-only
AI agent that answers your inbox and proposes calendar entries. Installable on a phone (PWA).

Rust frontend (Leptos, WASM) and Rust backend (Vercel functions), PostgreSQL (Supabase) with a Drizzle-managed schema,
domain model in OWL + SHACL. Optional home server (Ubuntu) for the news briefing and the agent with a local model
(Ollama).

```
ontology/      OWL ontology + SHACL shapes (source of truth for the domain)
db/            Drizzle schema + migrations (TypeScript; source of truth for the SQL schema)
api/           Vercel Rust functions (one file = one function, each a [[bin]] in Cargo.toml)
web/           Leptos CSR frontend, built with Trunk, installable PWA
tools/         agent, news pipeline, knowledge-base importer, backups, checks
tools/server/  systemd units for a home server (agent runs, daily news)
data-private/  YOUR data (git-ignored, never uploaded: see .gitignore and .vercelignore)
```

## Make it yours

Everything that is about one person's life lives in a few files. Start here:

| What | Where |
| --- | --- |
| Your time zone ("today", "tomorrow", event times, the news day) | `LIFEUI_TIMEZONE` (IANA name, e.g. `Europe/Paris`): in Vercel's environment variables and in `.env.agent` |
| Your main interest or project ("Focus"): its name, the words that mark a task or topic as part of it, the Home card text, what the app calls your server, an optional "ask the briefing" link | `web/src/config.rs` |
| The news you follow: sources (RSS, GitHub repos, Hacker News, Hugging Face, plain pages), keyword topics, who you are (`reader`) and what your focus is (`focus`), the briefing language | `tools/news/sources.yaml` |
| Weather hints: when you are outside (commute windows), thresholds for umbrella / heat / cold / wind, the text | `tools/news/weather_hints.yaml` |
| Weather location (IPMA, Portugal only; replace `tools/news/weather.py` with your own service's fetcher elsewhere) | `WEATHER_IPMA_CITY`, `WEATHER_IPMA_AREA`, `WEATHER_PLACE` in `.env.agent` |
| What the agent reads: GitHub repo, Google Calendar (iCal secret address), notes folders (Markdown / Obsidian) | `.env.agent` (from `tools/agent.env.example`) |
| What you know about your life (people, projects, studies, documents, tasks) | `data-private/owner.ttl` + `data-private/import/*.ttl`, loaded with `tools/import_knowledge.py` (see "Knowledge base import"); `tools/ontology-importer-brief.md` is a brief for an AI agent that drafts these files from your documents |
| The domain model itself (new classes and properties) | `ontology/lifeui.ttl` + `ontology/lifeui.shapes.ttl` (`tools/validate_ontology.py` checks them) |
| Colours | the token block at the top of `web/style.css` |

The rest of this file explains how each part works and how to deploy it.

## Login

One owner, one password, kept in the database only as an **Argon2id hash** (never the password). First visit shows a **first-time setup** form: you enter the `SETUP_CODE` from the server environment (so a stranger who finds the URL first cannot claim the app) and choose your password (12+ characters). After that the setup endpoint refuses forever (the table holds exactly one row). Afterwards you log in with the password.

- Session = random 256-bit token in a `__Host-` cookie (HttpOnly, Secure, SameSite=Strict, 30 days); the database stores only the SHA-256 of the token, so a database leak reveals no live session.
- Guessing is limited: after 5 wrong passwords the login locks for 30 s, doubling up to 15 min; the right password is refused while locked. The same counter covers wrong setup codes.
- POST endpoints reject requests whose `Origin` does not match `Host` (basic CSRF defence) and bodies over 4 KB.
- Files: `src/auth.rs` (pure logic, unit tested), `src/store.rs` (SQL), `src/util.rs` (HTTP helpers), `api/auth_*.rs` (functions), `db/src/schema.ts` (tables `owner_credentials`, `auth_throttle`, `sessions`, all with row-level security).
- Forgot the password? There is no reset by email on purpose: delete the row (`DELETE FROM lifeui.owner_credentials; DELETE FROM lifeui.sessions;`) in the Supabase SQL editor, then use the setup page again with `SETUP_CODE`.

## Data API

The persisted ontology class (events) has endpoints. Everything needs a login session (401 otherwise); changes also need a matching `Origin` (403 otherwise).

| Endpoint | What |
| --- | --- |
| `GET /api/events` | the **timeline**, newest first by when it happened (`occurred_at`, else `started_at`, else `occurred_on`). Filters: `class`, `status`, `from` (inclusive), `to` (exclusive), `limit` (1-200, default 50), `include_superseded=1` |
| `GET /api/events?id=<uuid>` | one event |
| `POST /api/events` | create. Body: `class` (default `Event`; Appointment, Meeting, Milestone, Trip, Workout, Measurement, Transaction, Award), `name`, `status`, one of `occurred_at` / `occurred_on` / `started_at` (+ optional `ended_at`), optional `description`, `sensitivity`, `confidence`, `payload`, `supersedes` |

Rules (they mirror the SHACL shapes and the database CHECK constraints, and `cargo test` fails if the lists drift from `db/src/schema.ts` or `ontology/lifeui.ttl`):
- **Nothing is updated in place or deleted.** To change a fact, `POST` a new one with `"supersedes": "<id>"`: it is created in one transaction, the old row keeps existing and gets `superseded_by`. Replacing the same item twice is a 409.
- A Transaction needs `payload.amount` and `payload.currency` (ISO 4217); a Measurement needs `payload.metric`, `value`, `unit`; an event needs some time; `started_at` must not be after `ended_at`.
- **Secrets are never stored.** Payloads containing fields such as `password`, `token`, `pin`, `api_key` are refused at any depth.
- Every validation problem is returned at once (`400 {"error": "Validation failed.", "details": [...]}`). Facts entered through the app are linked to the source "Entered in the LifeUI app" (UserStated).
- **Three validation layers, in order:** (1) `src/domain.rs` parses the request, applies defaults and gives friendly messages; (2) `src/shapes.rs` is the **authoritative gate**: it builds an RDF graph from the validated item (typed values, never pasted text, so user text cannot inject triples), adds the ontology and runs the REAL shapes file `ontology/lifeui.shapes.ttl` through the `shacl` (rudof) engine, so a rule added to the shapes is enforced without touching Rust; (3) the database CHECK constraints. Only SHACL *violations* reject a write; warnings (e.g. "no account linked yet") do not. The ontology and the shapes are compiled into the functions (`include_str!`), so they must not be in `.vercelignore`. The SHACL engine only checks that a literal is *labelled* as a date or number, not that it is well-formed, so `src/shapes.rs` checks dates, timestamps and numbers itself first.
- Code: `src/domain.rs` (rules, unit tested), `src/shapes.rs` (SHACL gate, unit tested incl. bypass and injection tests), `src/data.rs` (SQL), `api/events.rs`.

## News tab

Every morning a computer you own (the "home server"; any Linux box with Ollama) runs `tools/news/daily.sh`: it
fetches the sources in `tools/news/sources.yaml`, keeps the last day's new items, lets the local model tag and score
each one (importance, and relevance to your focus), writes a "3 things that matter today" intro and publishes the
briefing to the app. With a weather location set it adds the weather and the hints from `weather_hints.yaml`.

The **News** tab shows the **Today** card (missing-briefing banner, weather hints, today's calendar, top 3, open
inbox items), the weather card (hourly 09–21h, warnings, "stale" when the weather service was unreachable), the intro,
a filter (All / AI / Research / Releases / your focus), the items (read / save / dismiss; **Save** also adds a "Read: …"
task to Home's to-dos), a date picker, a **Daily / Weekly** switch (on Sundays a weekly digest with trends and focus
highlights) and a collapsible **Server status** with the **Stay awake tonight** switch. Diagnostics sit under
Agent → Settings → Diagnostics.

Once a month (`tools/news/extras.py feedback`) your reads, saves and dismissals nudge the source weights and topic
boosts (small, bounded steps, logged, reversible with `--revert`), written to a `tuning.yaml` overlay next to the
briefings rather than to `sources.yaml`. With `NEWS_ARCHIVE_DIR` set, each briefing is also written as a Markdown
note (`News/YYYY-MM-DD.md`) into your notes folder.

| Endpoint | Who | What |
| --- | --- | --- |
| `POST /api/runner/news` | runner token | publish the briefing JSON for its date (replaces that date's briefing; read/saved/dismissed marks survive). `vercel.json` rewrites it to `/api/runner?route=news`, so it uses no extra function (Hobby plan: 12). Refused while the agent switch is off. |
| `GET /api/runner` | runner token | `{"enabled", "stay_awake": {"stay_awake_on", "tonight"}}`; the server reads it before its 21:00 shutdown (works with the agent off) |
| `GET /api/news[?date=YYYY-MM-DD][&type=daily\|weekly]` | login | that briefing (default: the newest), the dates that have one, the stay-awake setting |
| `POST /api/news {"kind":"item","id","action"}` | login | `read`, `unread`, `save`, `unsave`, `dismiss`, `undismiss` |
| `POST /api/news {"kind":"stay_awake","on":true\|false}` | login | keep the server on tonight (today's local date) |
| `GET /api/runner?route=signals&days=N` | runner token | the last N days' items with read / saved / dismissed marks (weekly digest, monthly tuning) |

Tables `news_briefings` (one per date and type; `extras` holds the calendar, weekly digest and tuning report) and `news_items` (URL unique); RLS on with no policies like every table. Code: `src/news.rs` (validation + SQL, unit tested), `api/news.rs`, `api/runner.rs`, `web/src/news.rs`, `tools/news/` (`news.py` briefing, `weather.py`, `extras.py` calendar / archive / weekly / feedback, `publish.py`, `daily.sh`). A sample briefing for local work: `tools/news/fixture_briefing.json`. Self-tests: `python -I tools/news/<news|weather|extras>.py selftest`.

The **Vault** (critical information) was removed (migration `0006` drops `critical_information`). Critical information is still part of the ontology and now lives in the knowledge base (Explore) through `tools/import_knowledge.py`.

## Audit log

`lifeui.audit_log` records **who did what and when**: `action`, `target` (an id), the caller's `ip` and a small `detail` object. It never holds names, descriptions, pointers or secrets.

| Action | Recorded when |
| --- | --- |
| `setup_ok`, `setup_failed` | first-time setup succeeds / wrong setup code |
| `login_ok`, `login_failed`, `login_locked` | right password / wrong password / attempt refused while locked |
| `logout` | a live session ends (a forged cookie is not an event) |
| `event_create` | a fact is written; the row is written **in the same transaction**, so a write and its record commit or roll back together |
| `critical_create`, `critical_read` | the removed Vault (kept so older rows stay valid) |
| `news_publish`, `news_item_update`, `agent_stay_awake`, `news_missing` | the home server publishes a briefing / the owner reads, saves or dismisses an item / "Stay awake tonight" changes / no daily briefing after 09:45 local time (once per day) |

- **Append-only, enforced by the database:** triggers refuse `UPDATE`, `DELETE` and `TRUNCATE` (migration `0003`). The API has no way to write or change it.
- `GET /api/audit` (login required, read-only): newest first; filters `action` and `limit` (1-200, default 50).
- The IP comes from `x-forwarded-for` (set by Vercel) and is kept only if it looks like an address.
- The action list lives in `db/src/schema.ts` and `src/store.rs`; `cargo test` fails if they drift.

## Scheduled agent (read-only, cloud + PC)

`tools/agent_runner.py` is the agent. It is **read-only**: it reads a limited snapshot from `/api/runner` (events from 3 days back to
14 days ahead and open tasks, only `Public`/`Personal` rows), answers the Agent-tab inbox and posts one message to the
feed. The model gets no tools, treats everything it reads as data, and the runner token is refused by every other endpoint.

**Event proposals.** When an inbox item asks for a calendar entry ("Dentist Friday 15:00", "Jantar amanhã às 20h"), the
answer carries a proposed event (`class`, `name`, `date`, optional `time` in your local time (`LIFEUI_TIMEZONE`), optional `description`;
classes Event/Appointment/Meeting/Trip/Milestone). It is stored as **text** on the inbox item (`proposal`,
`proposal_status` = pending). The Agent tab shows it with **Add** (creates the event as proposed), **Edit** (opens the event
form filled in) and **Dismiss** (closes the item). Add and Edit save through `POST /api/events` with
`"from_proposal": "<inbox id>"`, with the owner's login and the usual checks (Rust rules + SHACL); the event and the "added"
mark are one transaction, so a proposal becomes at most one event. The runner token still cannot write events. Relative
days in the owner's own words (today/tomorrow/a weekday, English or Portuguese) are worked out by `agent_runner.py`, not
by the model, because local models get weekday arithmetic wrong; the reply text about the event is written by the code too.

There are two kinds of run, and they normally both happen on the home server with a local model:

| Run | Scheduled on | Reads |
|---|---|---|
| `cloud` (morning, midday, evening) | home server, `tools/server/` systemd timers at 09:00 / 12:30 / 19:00 (the server's local time) | LifeUI, a GitHub repo's issues and commits, Google Calendar (iCal secret address); answers the inbox |
| `pc` (evening) | home server, timer at 18:30 (or Windows Task Scheduler, `tools/install_pc_task.ps1`) | LifeUI, notes changed in your Markdown / Obsidian vault (skips `Private/`, `.obsidian`, notes with `private: true`) |

`.github/workflows/agent.yml` no longer runs on a schedule (so the inbox is never claimed twice); start it by hand from the Actions tab
as a Claude backup when the server is down.

**Which model** (`.env.agent`, see `tools/agent.env.example`):

| Setting | Meaning |
|---|---|
| `AGENT_PROVIDER` | `anthropic` (default, Claude API) or `ollama` (local model; called with the standard library only) |
| `AGENT_MODEL` | default `claude-opus-5-5` / `qwen3:8b` |
| `OLLAMA_URL` | default `http://127.0.0.1:11434` |
| `AGENT_NUM_CTX`, `AGENT_NUM_PREDICT` | Ollama context window and answer limit, default 8192 / 2048 tokens. If the snapshot is too big, the lowest-priority items are left out (oldest commits, then issues, notes, far calendar events, far LifeUI events, tasks; never inbox items) and the model is told |
| `AGENT_FALLBACK` | `anthropic`: if the local model fails (down, error, cut off, not JSON), retry once with Claude when `ANTHROPIC_API_KEY` is set. Unset: the run is recorded as failed |

The local model gets the same prompts and must answer in the same JSON shape (Ollama structured outputs, `format` = its JSON schema);
the same parsing, validation and length limits apply. `done_reason: "length"` is the same "cut off (max_tokens)" failure as with Claude.
The run log shows which model answered (`cloud (ollama qwen3:8b): ...`).

Setup: Agent tab > Settings > create a token and switch the agent on, then fill in `.env.agent` (home server: below; Windows PC: copy
`tools/agent.env.example` to `.env.agent` and run the install script; GitHub backup: the repo secrets `LIFEUI_RUNNER_TOKEN`,
`ANTHROPIC_API_KEY` and optionally `GCAL_ICS_URL`). The kill switch stops every run. Test without a model:
`python -I tools/agent_runner.py run --where cloud --slot morning --dry-run` prints what would be sent.
Checks: `python -I tools/agent_runner.py selftest` (offline; the Ollama path runs against a stub server on 127.0.0.1) and the agent
section of `tools/smoke_test.py --local-db` (fake model).

### Home server (Ubuntu 24.04)

Assumes Ollama listening on 127.0.0.1:11434 with the model pulled (`ollama pull qwen3:8b`), a system user `lifeui`
(`useradd --system --home-dir /opt/lifeui --shell /usr/sbin/nologin lifeui`) and, optionally, your notes folder at `/srv/data/Notes`, readable by it.

```bash
sudo git clone https://github.com/<you>/<your-fork> /opt/lifeui && sudo chown -R lifeui: /opt/lifeui
# Python 3.13 (as in CI) in a shared folder: lifeui's home is the repo, so a per-user install would land inside it
sudo env UV_PYTHON_INSTALL_DIR=/opt/uv/python uv python install 3.13
sudo -u lifeui env UV_PYTHON_INSTALL_DIR=/opt/uv/python UV_CACHE_DIR=/tmp/uv-cache uv venv --python 3.13 /opt/lifeui/.venv
sudo -u lifeui env UV_CACHE_DIR=/tmp/uv-cache uv pip install -p /opt/lifeui/.venv -r /opt/lifeui/tools/requirements.txt
sudo install -o lifeui -g lifeui -m 600 /opt/lifeui/tools/agent.env.example /opt/lifeui/.env.agent
sudoedit /opt/lifeui/.env.agent    # LIFEUI_URL, LIFEUI_RUNNER_TOKEN, LIFEUI_TIMEZONE, AGENT_PROVIDER=ollama, OBSIDIAN_VAULT=/srv/data/Notes, ...
sudo /opt/lifeui/tools/server/install_server.sh          # checks the setup, installs the units, enables the timers
sudo systemctl start lifeui-news && journalctl -u lifeui-news -n 30   # the first news briefing
sudo -u lifeui /opt/lifeui/.venv/bin/python -I /opt/lifeui/tools/agent_runner.py run --where cloud --slot morning --dry-run
sudo systemctl start lifeui-agent@cloud-morning && journalctl -u lifeui-agent@cloud-morning -n 30
```

`lifeui-agent@.service` is one oneshot run; its instance is `<where>-<slot>` (`cloud-morning`, `pc-evening`, ...). It runs as `lifeui`
with `ProtectSystem=strict`, `NoNewPrivileges` and no capabilities; it sees your notes folder (`BindReadOnlyPaths`) read-only and nothing else under `/srv`.
`lifeui-news.service` runs `tools/news/daily.sh` every morning at 08:30 (`lifeui-news.timer`), keeping its working files in `/var/lib/lifeui-news`.
The timers are `Persistent=true`, so runs missed while the server was off happen at the next boot (one at a time). After pulling
changes: `sudo -u lifeui git -C /opt/lifeui pull` and, if `tools/server/` changed, re-run `install_server.sh`.
On Linux the venv's Python is `.venv/bin/python` (Windows: `.venv\Scripts\python.exe`); `scripts/py.mjs` picks the right one, and
the PostgreSQL-based tests use `/usr/lib/postgresql/<version>/bin` (or `PG_BIN`).

## Design system (frontend polish)

The frontend stays **Leptos (Rust/WASM)**. shadcn/ui, Radix Themes, Base UI and Vaul are React libraries, so their *design language* is re-implemented here instead of adding a React runtime:

| Piece | Where | What it follows |
| --- | --- | --- |
| Tokens, light and dark (system setting), components | `web/style.css` | shadcn/ui token convention (HSL variables), Radix Themes shapes (radius scale, soft tinted badges) |
| Icons, cards, tabs, bottom sheet markup | `web/src/ui.rs`, `web/src/main.rs` | Lucide-style line icons drawn in `Icon`; accessible roles and `aria-current`, keyboard on every clickable row |
| Bottom sheet (add and "New version" forms) | native `<dialog id="sheet">` + `lifeui.sheet` in `web/public/lifeui.js` | Vaul-style drag: follows the finger 1:1, rubber-band when pulled up, dismiss past 40% of the height or on a quick flick, snap-back otherwise; centered modal on desktop |
| Animated counters | `web/public/number-flow.js` (vendored, see `web/vendor/README.md`) | [NumberFlow](https://github.com/barvian/number-flow) |
| Motion and mobile rules | `.claude/skills/` (Emil Kowalski's design-engineering skills, MIT: `emil-design-eng`, `mobile-native`, `gestures`, `break-ui`) | ease-out entrances under 300 ms, press feedback `scale(.97)`, only `transform` and `opacity` animate, hover only on hover-capable devices, `dvh`, safe areas, 16 px inputs, 44 px touch targets, `prefers-reduced-motion` |

**Palette** (all in the token block at the top of `web/style.css`, light and dark): steely blue-grey surfaces and text, **muted blue** for primary actions, links and focus, **dark green** for success (Completed, Public) and the add button, and **vibrant red** for danger (Secret, errors, overdue, Blocked) plus the small pop on the active tab. `node scripts/contrast.mjs` (part of `npm run check`) reads those tokens and fails if a text pair drops below WCAG AA (4.5:1).

The class names the browser test looks for (`.tabs`, `.tab`, `.fab`, `.card`, `.row`, `.pick`, `.detail`, `.links`, `.problems`, `.warn`, `.logout`) are a contract between `style.css`, `ui.rs` and `tools/ui_test.py`. To see the screens, run `UI_SCREENSHOT_DIR=data-private\shots` with `tools/ui_test.py`: it saves dark and light, phone and desktop images of every screen and both sheets.

## Monorepo with Turborepo (local development and CI)

The repo mixes Rust, TypeScript and Python. [Turborepo](https://turborepo.com) runs one task name across all of them, in dependency order, and **caches results**: if a package's inputs did not change, the task is replayed from the cache instead of re-running.

| Package (folder) | Language | `check` | `test` | `build` |
| --- | --- | --- | --- | --- |
| `lifeui-api` (`src/`, with `api/*.rs`) | Rust | `cargo check --bins` | `cargo test --lib` | `cargo build --bins --release` (never cached) |
| `lifeui-web` (`web/`) | Rust to WASM | `cargo check` for wasm32 | | `trunk build --release` (caches `dist/`) |
| `lifeui-db` (`db/`) | TypeScript | `tsc --noEmit` + `drizzle-kit check` | | |
| `lifeui-tools` (`tools/`) | Python | ontology + SHACL validation | importer and backup self-tests | |

```powershell
npm install            # once (npm workspaces: one lockfile at the root)
npm run check          # = turbo run check   (all four packages in parallel, ~10 s, instant when cached)
npm run test           # = turbo run test
npm run e2e            # = web build, then tools/smoke_test.py --local-db and tools/ui_test.py (never cached)
npm run ci             # check + test + e2e
npx turbo run check --filter=lifeui-db   # one package only
```

- The `package.json` files in `src/`, `web/` and `tools/` are **stubs** so Turborepo can see those folders; the Rust crate is still defined by the root `Cargo.toml`, and nothing was moved (Vercel's Rust runtime needs `Cargo.toml` and `api/` at the root).
- **Production is unchanged:** Vercel still runs `scripts/vercel-build.sh` (`vercel.json`), and `installCommand` is overridden so it does not run `npm install`.
- `scripts/py.mjs` finds the project's Python (`.venv\Scripts\python.exe` on Windows, `.venv/bin/python` elsewhere) so the same command works on every machine.
- `migrate` is deliberately **not** a Turborepo task: it writes to the live database and must be run by hand (`npm --prefix db run migrate`).
- The cache lives in `.turbo/cache` (git-ignored). Do not enable Turborepo's remote cache: the `tools` tasks use `data-private/` as input.

## Local checks

```powershell
.venv\Scripts\python.exe tools\validate_ontology.py        # ontology + shapes + data
cargo check -p lifeui-api                                  # Rust functions
cargo check -p web --target wasm32-unknown-unknown         # frontend
cd db; npx tsc --noEmit; npx drizzle-kit generate          # schema
cd web; trunk build --release                              # -> web/dist
cargo test --lib                                           # unit tests (47)
.venv\Scripts\python.exe -I tools\smoke_test.py            # runs the app locally (no database) and tests it
.venv\Scripts\python.exe -I tools\smoke_test.py --local-db # + throwaway local PostgreSQL: login + events + entities/Explore API + audit log + agent + News (278 checks)
```

`tools/smoke_test.py` builds and starts the two Rust functions, serves `web/dist` through `tools/dev_server.py` (a local stand-in for Vercel that forwards `/api/*`), checks file types, manifest, API answers, and loads the page in headless Edge so the WASM app and the JS glue really run. If a `.env` with `DATABASE_URL` exists it also tests the real database (secrets are masked in the output). Passkeys cannot be tested this way (they need a real authenticator).

## Deploy (needs your own accounts)

1. Fork or copy this repo into your own GitHub account (a **private** repo is a good idea once `data-private/` or your settings hold anything personal).
2. **Supabase**: create a project at supabase.com. Pick a region near you; use a long random database password. In the project's **Connect** dialog copy two connection strings (replace `[YOUR-PASSWORD]`; never commit them):
   - **Transaction pooler** (port **6543**, user `postgres.PROJECT_REF`) -> used by the app as `DATABASE_URL`.
   - **Session pooler** (port **5432**, same host and user) -> used only for migrations as `DATABASE_URL_MIGRATE`.
   The "direct connection" is IPv6-only on the free plan, so avoid it. Add `?sslmode=require` if the string has no `sslmode`.
3. **Vercel**: import the repo (or `npx vercel` from this folder). Add env vars for Production and Preview: `DATABASE_URL` (transaction pooler URL), `LIFEUI_TIMEZONE` (your IANA time zone) and `SETUP_CODE` (a random string of 16+ characters you invent, e.g. 4 random words; it is only needed once, for the first-time setup page, and is never stored in the database). Then redeploy: variables only reach new builds. Match the function region to Supabase (Project Settings -> Functions -> region, e.g. `lhr1` London for Supabase `eu-west-2`).
4. **Migrate** (creates the `lifeui` schema and its tables, RLS on): copy `.env.example` to `.env`, fill in `DATABASE_URL_MIGRATE` in an editor, then `cd db; npm run migrate` (reads `../.env`; the connection is TLS-verified against `db/certs/supabase-prod-ca-2021.crt`). Do not use `drizzle-kit migrate`: it rejects Supabase's certificate chain (`SELF_SIGNED_CERT_IN_CHAIN`) and the usual workaround turns certificate checking off. The Rust functions also verify the server (CA compiled in from the same file, see `src/store.rs`); the certificate expires in 2031.
5. Build the frontend (`cd web; trunk build --release`) and deploy with the prebuilt `web/dist`.
6. Turn on Vercel Authentication (Settings -> Deployment Protection, "All Deployments").
7. In Supabase, if the dashboard offers it, turn the **Data API** off (Project Settings -> API): the app talks to Postgres directly, so nothing needs it. The tables are also outside `public` and have RLS on without policies.

Notes: free Supabase projects pause after about a week without activity (first request after that is slow or needs a restore); the app and the migration use different pooler modes on purpose (transaction mode forbids prepared statements, which `api/db_ping.rs` therefore disables).

## Knowledge base import

The Turtle files in `data-private/` (git-ignored, they hold your personal data) are the master copy of everything the app knows about you.
Write them by hand from the ontology (`ontology/lifeui.ttl`; every entity needs `life:name`, `life:hasSource`, `life:confidence` and `life:hasSensitivity`), or let an AI agent draft them from your documents with `tools/ontology-importer-brief.md` (Claude Code: the `ontology-importer` agent in `.claude/agents/`).
`tools/import_knowledge.py` loads them into the database:

| Turtle entity | Table | On re-run |
|---|---|---|
| `life:Source` | `lifeui.sources` | inserted if absent |
| `life:Event` and subclasses | `lifeui.events` | inserted if absent, **never overwritten** (the app owns them) |
| everything else, `life:CriticalInformation` included (pointers only, never secrets) | `lifeui.entities` (+ `lifeui.entity_links` for relations) | refreshed in one transaction (origin `import`) |

```powershell
npm --prefix db run migrate                                        # once: creates entities and entity_links (migration 0002)
.venv\Scripts\python.exe -I tools\import_knowledge.py plan          # what would be loaded; touches no database
.venv\Scripts\python.exe -I tools\import_knowledge.py selftest      # full import on a throwaway local PostgreSQL, 20 checks
.venv\Scripts\python.exe -I tools\import_knowledge.py apply --yes   # writes to DATABASE_URL_MIGRATE in .env (the live database)
```

`apply` runs the SHACL and OWL-RL validator first and refuses to import if anything is wrong. The app reads the imported entities
through `GET /api/entities` (the **Explore** tab). To change an imported entity, edit the Turtle and re-run the import.
Browser test of the whole app (including the bottom-sheet gestures; the knowledge-base part runs when `data-private/owner.ttl` exists): `cd web; trunk build --release`, then `.venv\Scripts\python.exe -I tools\ui_test.py` (needs Microsoft Edge and a local PostgreSQL).
