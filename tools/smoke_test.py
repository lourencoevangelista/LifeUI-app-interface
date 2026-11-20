#!/usr/bin/env python
"""Local smoke test for LifeUI. Run from anywhere:

    .venv\\Scripts\\python.exe -I tools\\smoke_test.py               # no database: static files, API errors, page error screen
    .venv\\Scripts\\python.exe -I tools\\smoke_test.py --local-db    # + temporary local PostgreSQL: the FULL login flow

It builds the Rust functions, starts them on local ports, starts tools/dev_server.py (serves web/dist and
proxies /api), checks files and API answers over HTTP, and loads the pages in headless Edge so the real
WASM app runs in a browser engine.

--local-db starts a throwaway PostgreSQL cluster (from the installed PostgreSQL's program files, on its
own port, trust auth, in a temp folder that is deleted afterwards), applies db/migrations/*.sql and
runs the first-time-setup, login, session, lockout and lock-down checks. SAFETY: those checks create the
owner password, so they run ONLY against that throwaway database, never against the DATABASE_URL in
.env (which may point at Supabase). Without --local-db a DATABASE_URL from .env is used for a read-only
connectivity check, with secrets masked in all output.

Exit code is non-zero if any check fails. Not covered: passkeys (need a real authenticator), the logged-in
screen in a browser (headless Edge cannot carry the session cookie), Vercel-specific behaviour.
"""
from __future__ import annotations

import argparse
import datetime as dt
import hashlib
import html
import json
import os
import re
import shutil
import socket
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request
from pathlib import Path
from urllib.parse import urlparse

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(Path(__file__).resolve().parent))
from localpg import LocalPostgres, find_pg_bin, free_port, EXE  # noqa: E402
DIST = ROOT / "web" / "dist"
PORT_WEB = 8787
FUNCTIONS = {"health": 3101, "db_ping": 3102, "auth_status": 3103, "auth_setup": 3104, "auth_login": 3105, "auth_logout": 3106,
             "events": 3107, "news": 3108, "entities": 3109, "audit": 3110, "agent": 3111, "runner": 3112}
SETUP_CODE = "local-test-setup-code-0123456789"
TEST_PASSWORD = "correct horse battery staple 42"
COOKIE = "__Host-lifeui_session"
PG_ROOT = Path(r"C:\Program Files\PostgreSQL")
EDGE_CANDIDATES = [
    r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe",
    r"C:\Program Files\Microsoft\Edge\Application\msedge.exe",
]

results: list[tuple[str, str, str]] = []  # (status, name, detail) status: PASS / FAIL / SKIP
secrets: list[str] = []


def mask(text: str) -> str:
    for s in secrets:
        if s:
            text = text.replace(s, "***")
    return text


def record(status: str, name: str, detail: str = "") -> None:
    results.append((status, name, mask(detail)))
    print(f"[{status}] {name}" + (f": {mask(detail)}" if detail else ""), flush=True)


def check(condition: bool, name: str, detail: str = "") -> bool:
    record("PASS" if condition else "FAIL", name, detail)
    return condition


def load_env() -> dict[str, str]:
    env: dict[str, str] = {}
    path = ROOT / ".env"
    if path.exists():
        for line in path.read_text(encoding="utf-8").splitlines():
            line = line.strip()
            if not line or line.startswith("#") or "=" not in line:
                continue
            key, value = line.split("=", 1)
            env[key.strip()] = value.strip().strip('"').strip("'")
    return env


def wait_port(port: int, timeout: float = 20) -> bool:
    end = time.time() + timeout
    while time.time() < end:
        with socket.socket() as s:
            s.settimeout(0.5)
            if s.connect_ex(("127.0.0.1", port)) == 0:
                return True
        time.sleep(0.3)
    return False


# ----------------------------------------------------------------------------- HTTP helpers
def api(method: str, path: str, body=None, cookie: str | None = None, headers: dict | None = None, raw: bytes | None = None):
    """Returns (status, response headers lower-cased with repeated values joined by ' | ', parsed JSON or None)."""
    data = raw if raw is not None else (json.dumps(body).encode() if body is not None else None)
    request = urllib.request.Request(f"http://127.0.0.1:{PORT_WEB}{path}", data=data, method=method)
    if data is not None:
        request.add_header("Content-Type", "application/json")
    if cookie:
        request.add_header("Cookie", f"{COOKIE}={cookie}")
    for key, value in (headers or {}).items():
        request.add_header(key, value)
    try:
        response = urllib.request.urlopen(request, timeout=60)
    except urllib.error.HTTPError as e:
        response = e
    payload = response.read()
    hdrs: dict[str, str] = {}
    for key, value in response.headers.items():
        hdrs[key.lower()] = f"{hdrs[key.lower()]} | {value}" if key.lower() in hdrs else value
    try:
        parsed = json.loads(payload)
    except Exception:
        parsed = None
    return response.status, hdrs, parsed, payload


def get(path: str):
    status, hdrs, _, payload = api("GET", path)
    return status, hdrs, payload


def cookie_from(hdrs: dict[str, str]) -> str | None:
    match = re.search(rf"{re.escape(COOKIE)}=([^;\s|]+)", hdrs.get("set-cookie", ""))
    return match.group(1) if match else None


def run(cmd: list[str], cwd: Path, name: str) -> bool:
    p = subprocess.run(cmd, cwd=cwd, capture_output=True, text=True)
    if p.returncode != 0:
        record("FAIL", name, (p.stderr or p.stdout)[-500:])
        return False
    record("PASS", name)
    return True


# ----------------------------------------------------------------------------- the checks
def check_static() -> None:
    status, headers, body = get("/")
    check(status == 200 and b"<html" in body.lower() and b"wasm" in body, "GET / serves the app shell", f"status {status}")
    check(b'rel="icon"' in body, "page declares a tab icon (no /favicon.ico 404)")
    status, headers, body = get("/manifest.webmanifest")
    try:
        manifest = json.loads(body)
        good = status == 200 and manifest.get("name") == "LifeUI" and len(manifest.get("icons", [])) >= 2
    except Exception:
        good = False
    check(good and "manifest+json" in headers.get("content-type", ""), "manifest.webmanifest valid and typed application/manifest+json", headers.get("content-type", ""))
    for path, kind in (("/sw.js", "javascript"), ("/lifeui.js", "javascript"), ("/logo-180.png", "image/png"),
                       ("/logo-192.png", "image/png"), ("/logo-512.png", "image/png")):
        status, headers, body = get(path)
        ctype = headers.get("content-type", "")
        check(status == 200 and len(body) > 100 and kind in ctype, f"GET {path}", f"{status} {ctype}")
    wasm = next(DIST.glob("*_bg.wasm"), None)
    if wasm:
        status, headers, body = get("/" + wasm.name)
        check(status == 200 and headers.get("content-type") == "application/wasm" and body[:4] == b"\0asm",
              "wasm served with application/wasm", f"{status} {headers.get('content-type', '')} {len(body)//1024} KB")


def check_api_without_database() -> None:
    status, _, data, _ = api("GET", "/api/health")
    check(status == 200 and data and data.get("ok") is True, "GET /api/health", f"{status} {data}")
    status, _, data, _ = api("GET", "/api/db_ping")
    check(status == 503 and data and data.get("ok") is False and data.get("stage") == "env",
          "db_ping without a database reports stage 'env' (no crash, no detail leaked)", f"{status} {data}")
    status, _, data, _ = api("GET", "/api/auth_status")
    check(status == 503 and data and data.get("error") == "Database unavailable.", "auth_status without a database is a clean 503", f"{status} {data}")
    status, _, data, _ = api("POST", "/api/auth_setup", {"setup_code": "x", "password": "y"})
    check(status == 503 and data and "SETUP_CODE" in data.get("error", ""), "auth_setup is disabled while SETUP_CODE is not configured", f"{status}")
    status, _, data, _ = api("GET", "/api/nope")
    check(status == 404, "unknown API route is a clean 404", str(status))


def check_request_guards() -> None:
    status, headers, _, _ = api("GET", "/api/auth_login")
    check(status == 405 and headers.get("allow") == "POST", "auth_login refuses GET with 405 and Allow: POST", f"{status}")
    status, _, data, _ = api("POST", "/api/auth_login", {"password": "x"}, headers={"Origin": "https://evil.example", "Host": f"127.0.0.1:{PORT_WEB}"})
    check(status == 403, "cross-site POST is refused (Origin does not match Host)", f"{status}")


def check_database_connectivity() -> None:
    status, _, data, _ = api("GET", "/api/db_ping")
    check(status == 200 and data and data.get("ok") is True and data.get("stage") == "done", "db_ping reaches the database", f"{status} {data}")
    check("version" not in (data or {}) and "db_version" not in (data or {}), "db_ping does not reveal the server version")


def check_remote_auth_status() -> str | None:
    """Read-only check against the real database: the login tables exist and the queries work through the pooler."""
    status, _, data, _ = api("GET", "/api/auth_status")
    good = status == 200 and data and data.get("ok") is True and isinstance(data.get("setup_required"), bool)
    check(good, "auth_status works against the REAL database (read-only: tables exist, pooler queries work)", f"{status} {data}")
    if not good:
        return None
    return "setup" if data["setup_required"] else "login"


def check_auth_flow(pg: LocalPostgres) -> None:
    # Fresh database
    status, _, data, _ = api("GET", "/api/auth_status")
    check(status == 200 and data == {"ok": True, "setup_required": True, "authenticated": False}, "fresh database: setup required", str(data))
    status, _, data, _ = api("POST", "/api/auth_login", {"password": TEST_PASSWORD})
    check(status == 409, "login before setup is refused (409)", f"{status}")

    # Setup: wrong code, weak password, good request
    status, _, data, _ = api("POST", "/api/auth_setup", {"setup_code": "wrong-code", "password": TEST_PASSWORD})
    check(status == 403 and data and data.get("error") == "Wrong setup code.", "setup with a wrong code is refused (403)", f"{status}")
    status, _, data, _ = api("POST", "/api/auth_setup", {"setup_code": SETUP_CODE, "password": "short"})
    check(status == 400, "setup with a weak password is refused (400)", f"{status} {data.get('error') if data else ''}")
    status, headers, data, _ = api("POST", "/api/auth_setup", {"setup_code": SETUP_CODE, "password": TEST_PASSWORD})
    token = cookie_from(headers)
    set_cookie = headers.get("set-cookie", "")
    check(status == 200 and token, "setup with the right code and a good password succeeds and starts a session", f"{status}")
    check(all(flag in set_cookie for flag in ("HttpOnly", "Secure", "SameSite=Strict", "Path=/")) and "Domain" not in set_cookie,
          "session cookie is __Host-, HttpOnly, Secure, SameSite=Strict", "")
    if not token:
        return

    # What the database really holds
    stored = pg.psql("SELECT password_hash FROM lifeui.owner_credentials").stdout.strip()
    check(stored.startswith("$argon2id$v=19$") and TEST_PASSWORD not in stored, "database holds an Argon2id hash, not the password", stored[:18] + "...")
    digest = hashlib.sha256(token.encode()).hexdigest()
    check(pg.psql(f"SELECT count(*) FROM lifeui.sessions WHERE token_hash = '{digest}'").stdout.strip() == "1"
          and pg.psql(f"SELECT count(*) FROM lifeui.sessions WHERE token_hash = '{token}'").stdout.strip() == "0",
          "database stores only the SHA-256 of the session token, not the token")

    # Session behaviour
    status, _, data, _ = api("GET", "/api/auth_status", cookie=token)
    check(data == {"ok": True, "setup_required": False, "authenticated": True}, "status with the cookie: authenticated", str(data))
    status, _, data, _ = api("GET", "/api/auth_status")
    check(data == {"ok": True, "setup_required": False, "authenticated": False}, "status without the cookie: not authenticated", str(data))
    status, _, data, _ = api("GET", "/api/auth_status", cookie="forged-token-value")
    check(data and data.get("authenticated") is False, "a forged cookie is not authenticated")
    status, _, data, _ = api("POST", "/api/auth_setup", {"setup_code": SETUP_CODE, "password": "another long password 1"})
    check(status == 409, "second setup is refused: the password cannot be overwritten (409)", f"{status}")
    big = b'{"password": "' + b"a" * 6000 + b'"}'
    status, _, data, _ = api("POST", "/api/auth_login", raw=big)
    check(status == 400, "oversized login body is refused (400)", f"{status}")
    status, _, data, _ = api("POST", "/api/auth_login", raw=b"not json")
    check(status == 400, "malformed JSON is refused (400)", f"{status}")

    # Logout kills the session server-side
    status, headers, _, _ = api("POST", "/api/auth_logout", {}, cookie=token)
    check(status == 200 and "Max-Age=0" in headers.get("set-cookie", ""), "logout clears the cookie", f"{status}")
    status, _, data, _ = api("GET", "/api/auth_status", cookie=token)
    check(data and data.get("authenticated") is False, "the old token no longer works after logout")
    check(pg.psql("SELECT count(*) FROM lifeui.sessions").stdout.strip() == "0", "logout deleted the session row")

    # Lockout: 5 wrong passwords, then locked even for the right one
    codes = [api("POST", "/api/auth_login", {"password": f"wrong password number {i}"})[0] for i in range(5)]
    check(codes == [401, 401, 401, 401, 401], "five wrong passwords are each refused (401)", str(codes))
    status, headers, data, _ = api("POST", "/api/auth_login", {"password": "wrong password number 6"})
    check(status == 429 and headers.get("retry-after", "0").isdigit() and int(headers["retry-after"]) > 0, "6th attempt is locked out (429 with Retry-After)", f"{status} retry-after={headers.get('retry-after')}")
    status, _, _, _ = api("POST", "/api/auth_login", {"password": TEST_PASSWORD})
    check(status == 429, "the RIGHT password is also refused while locked (429)", f"{status}")
    pg.psql("UPDATE lifeui.auth_throttle SET locked_until = NULL, failed_count = 0")  # skip the 30 s wait
    status, headers, _, _ = api("POST", "/api/auth_login", {"password": TEST_PASSWORD})
    new_token = cookie_from(headers)
    check(status == 200 and new_token and new_token != token, "after the lock expires the right password logs in with a NEW token", f"{status}")
    check(pg.psql("SELECT failed_count FROM lifeui.auth_throttle").stdout.strip() == "0", "successful login resets the failure counter")
    status, _, data, _ = api("GET", "/api/auth_status", cookie=new_token)
    check(data and data.get("authenticated") is True, "the new session works")

    # Lock-down: the Supabase browser role must not reach these tables
    p = pg.psql("SELECT 1 FROM lifeui.owner_credentials", role="postgres", check_ok=False)  # sanity: owner can read
    check(p.returncode == 0, "owner role can read the table (sanity)")
    p = pg.psql("SET ROLE anon; SELECT count(*) FROM lifeui.owner_credentials", check_ok=False)
    check(p.returncode != 0 and "permission denied" in p.stderr, "anon role: permission denied on the lifeui schema", p.stderr.strip()[:80])
    pg.psql("GRANT USAGE ON SCHEMA lifeui TO anon; GRANT SELECT ON ALL TABLES IN SCHEMA lifeui TO anon")  # worst case: someone exposes the schema
    p = pg.psql("SET ROLE anon; SELECT count(*) FROM lifeui.owner_credentials", check_ok=False)
    check(p.returncode == 0 and p.stdout.strip().splitlines()[-1] == "0", "even with SELECT granted, row-level security hides every row from anon", p.stdout.strip()[-20:] or p.stderr.strip()[:80])
    rls = pg.psql("SELECT count(*) FROM pg_tables WHERE schemaname = 'lifeui' AND NOT rowsecurity").stdout.strip()
    check(rls == "0", "row-level security is ON for every table in the lifeui schema", f"tables without RLS: {rls}")

    check_domain_flow(pg, new_token)
    check_entities_flow(pg, new_token)
    check_audit_flow(pg, new_token)
    check_agent_flow(pg, new_token)
    check_runner_flow(pg, new_token)
    check_agent_runner_script(pg, new_token)
    check_news_flow(pg, new_token)


def check_agent_runner_script(pg: LocalPostgres, token: str) -> None:
    """tools/agent_runner.py end to end against the local server with a fake model (no network, no Claude call),
    plus the read-only `context` action: what the agent can see and, above all, what it cannot."""
    def owner(body):
        return api("POST", "/api/agent", body, cookie=token)

    def sql(query: str) -> str:
        return pg.psql(query).stdout.strip()

    s, _, d, _ = api("GET", "/api/agent?view=settings", cookie=token)
    for t in d["tokens"]:  # the earlier flow left five active tokens: free one slot
        if not t["revoked_at"]:
            owner({"kind": "token_revoke", "id": t["id"]})
            break
    s, _, d, _ = owner({"kind": "token_create", "label": "script test"})
    tok = d["token"]
    owner({"kind": "switch", "enabled": True})

    def runner(body):
        return api("POST", "/api/runner", body, headers={"Authorization": f"Bearer {tok}"})

    onto = "https://lifeui.example/ontology#"
    pg.psql(
        "INSERT INTO lifeui.events (iri, class_iri, name, description, status, sensitivity, occurred_at) VALUES "
        f"('urn:t:near', '{onto}Meeting', 'Context visible meeting', 'public text', 'Planned', 'Public', now() + interval '2 days'), "
        f"('urn:t:secret', '{onto}Meeting', 'Context hidden sensitive', 'x', 'Planned', 'Sensitive', now() + interval '2 days'), "
        f"('urn:t:topsecret', '{onto}Meeting', 'Context hidden secret', 'x', 'Planned', 'Secret', now() + interval '2 days'), "
        f"('urn:t:far', '{onto}Meeting', 'Context too far ahead', 'x', 'Planned', 'Public', now() + interval '60 days'), "
        f"('urn:t:old', '{onto}Meeting', 'Context too old', 'x', 'Planned', 'Public', now() - interval '30 days')")
    pg.psql(
        "INSERT INTO lifeui.entities (iri, class_iri, name, status, sensitivity, props) VALUES "
        f"('urn:t:task-open', '{onto}Task', 'Context open task', 'InProgress', 'Personal', '{{\"dueDate\": \"2099-01-01\"}}'), "
        f"('urn:t:task-done', '{onto}Task', 'Context done task', 'Completed', 'Personal', '{{}}'), "
        f"('urn:t:task-secret', '{onto}Task', 'Context secret task', 'Planned', 'Secret', '{{}}'), "
        f"('urn:t:course', '{onto}Course', 'Context a course', NULL, 'Personal', '{{}}')")

    # --- the context action: scoped, read-only, only for a running run
    s, _, d, _ = runner({"action": "run_start", "slot": "morning"})
    run_id = d["run"]["id"]
    check(runner({"action": "context", "run_id": "nope"})[0] == 400, "context needs a run id (400)")
    check(runner({"action": "context", "run_id": "00000000-0000-4000-8000-000000000000"})[0] == 409, "context for an unknown run is refused (409)")
    check(runner({"action": "context", "run_id": run_id, "sql": "select 1"})[0] == 400, "context takes no extra fields (400)")
    s, _, d, _ = runner({"action": "context", "run_id": run_id})
    ctx = (d or {}).get("context", {})
    names = [e["name"] for e in ctx.get("events", [])]
    check(s == 200 and names == ["Context visible meeting"], "context: only Public/Personal events in the window (-3..+14 days)", str(names))
    task_names = [t["name"] for t in ctx.get("tasks", [])]
    check("Context open task" in task_names and not any(n in task_names for n in ("Context done task", "Context secret task", "Context a course", "Algebra")),
          "context: open tasks only (no done, no Secret, no other classes)", str(task_names))
    check("Context hidden" not in json.dumps(ctx) and "critical" not in json.dumps(ctx).lower(), "context holds nothing Sensitive or Secret")
    runner({"action": "run_finish", "run_id": run_id, "status": "ok"})
    check(runner({"action": "context", "run_id": run_id})[0] == 409, "context is refused once the run has finished (409)")

    # --- the script itself, fake model
    owner({"kind": "inbox", "body": "What is on this week?"})
    env = {k: v for k, v in os.environ.items() if k not in ("ANTHROPIC_API_KEY", "GITHUB_TOKEN")}
    env.update({"LIFEUI_URL": f"http://127.0.0.1:{PORT_WEB}", "LIFEUI_RUNNER_TOKEN": tok, "LIFEUI_FAKE_MODEL": "1",
                "GITHUB_REPOSITORY": "", "GCAL_ICS_URL": "", "OBSIDIAN_VAULT": ""})
    script = [sys.executable, "-I", str(ROOT / "tools" / "agent_runner.py")]

    def go(*args, e=None):
        return subprocess.run([*script, "run", *args], env=e or env, capture_output=True, text=True, timeout=120)

    before = sql("SELECT count(*) FROM lifeui.agent_messages")
    p = go("--where", "cloud", "--slot", "morning")
    check(p.returncode == 0 and "Run ok" in p.stdout, "agent_runner cloud/morning exits 0", (p.stdout + p.stderr)[-200:])
    check(tok not in p.stdout + p.stderr, "the runner token is never printed")
    check(sql("SELECT status || '|' || reply FROM lifeui.agent_inbox WHERE body = 'What is on this week?'") == "done|Fake reply.", "the inbox item was answered by the script")
    check(sql("SELECT kind || '|' || origin || '|' || slot FROM lifeui.agent_messages ORDER BY created_at DESC LIMIT 1") == "briefing|agent|morning", "the script posted a briefing to the feed (origin agent)")
    check(sql("SELECT status FROM lifeui.agent_runs ORDER BY started_at DESC LIMIT 1") == "ok", "the script closed its run as ok")
    check(sql("SELECT count(*) FROM lifeui.events WHERE name LIKE 'Context%'") == "5" and sql("SELECT count(*) FROM lifeui.agent_messages") == str(int(before) + 1),
          "the script wrote no events and exactly one feed message")

    # --- an event proposal: the agent prepares it (text only), the owner adds it with their login
    owner({"kind": "inbox", "body": "Dentist tomorrow at 15:00"})
    events_before = sql("SELECT count(*) FROM lifeui.events")
    p = go("--where", "cloud", "--slot", "evening")
    item = sql("SELECT id || '|' || status || '|' || proposal_status || '|' || (proposal->>'name') || '|' || (proposal->>'time') FROM lifeui.agent_inbox WHERE body = 'Dentist tomorrow at 15:00'")
    pid = item.split("|")[0]
    check(p.returncode == 0 and item.endswith("|needs_answer|pending|Dentist|15:00"), "a calendar request gets a PROPOSED event and waits for the owner", item + (p.stdout + p.stderr)[-150:])
    check(sql("SELECT count(*) FROM lifeui.events") == events_before, "the agent's proposal created no event")
    s, _, d, _ = api("GET", "/api/agent?view=inbox", cookie=token)
    shown = next((i for i in (d or {}).get("items", []) if i["id"] == pid), {})
    check(s == 200 and shown.get("proposal_status") == "pending" and shown.get("proposal", {}).get("class") == "Appointment", "the inbox view shows the proposal to the owner")
    proposed = {"class": "Appointment", "name": "Dentist", "status": "Planned", "occurred_at": "2030-01-10T15:00:00Z", "from_proposal": pid}
    check(api("POST", "/api/events", proposed, headers={"Authorization": f"Bearer {tok}"})[0] == 401, "a runner token cannot add the proposed event (401)")
    check(api("POST", "/api/events", {**proposed, "supersedes": "00000000-0000-4000-8000-000000000000"}, cookie=token)[0] == 400, "a proposed event cannot also supersede one (400)")
    s, _, d, _ = api("POST", "/api/events", proposed, cookie=token)
    event_id = (d or {}).get("event", {}).get("id", "")
    check(s == 201 and sql(f"SELECT status || '|' || proposal_status || '|' || proposal_event_id FROM lifeui.agent_inbox WHERE id = '{pid}'") == f"done|added|{event_id}",
          "Add: the event is created and the item is closed as added, linked to it", str(s))
    check(api("POST", "/api/events", proposed, cookie=token)[0] == 409 and sql("SELECT count(*) FROM lifeui.events") == str(int(events_before) + 1),
          "a proposal is added only once (409, no second event)")
    check(sql(f"SELECT count(*) FROM lifeui.audit_log WHERE action = 'agent_inbox_update' AND target = '{pid}' AND detail->>'proposal' = 'added'") == "1", "adding a proposal is audited")
    pg.psql("INSERT INTO lifeui.agent_inbox (body, status, reply, proposal, proposal_status) VALUES "
            "('Gym Monday', 'needs_answer', 'Prepared.', '{\"class\": \"Event\", \"name\": \"Gym\", \"date\": \"2030-01-14\"}', 'pending')")
    gid = sql("SELECT id FROM lifeui.agent_inbox WHERE body = 'Gym Monday'")
    check(owner({"kind": "proposal_dismiss", "id": "nope"})[0] == 400, "dismiss needs a UUID (400)")
    s, _, d, _ = owner({"kind": "proposal_dismiss", "id": gid})
    check(s == 200 and d["item"]["status"] == "cancelled" and d["item"]["proposal_status"] == "dismissed", "Dismiss closes the item as cancelled", str(s))
    check(owner({"kind": "proposal_dismiss", "id": gid})[0] == 409 and owner({"kind": "proposal_dismiss", "id": pid})[0] == 409, "an added or dismissed proposal cannot be dismissed (409)")
    check(api("POST", "/api/events", {**proposed, "from_proposal": gid}, cookie=token)[0] == 409, "a dismissed proposal cannot be added (409)")
    p2 = pg.psql("UPDATE lifeui.agent_inbox SET proposal = NULL WHERE id = '" + gid + "'", check_ok=False)
    check(p2.returncode != 0, "the database keeps a proposal and its status together (CHECK)")

    p = go("--where", "cloud", "--slot", "midday")
    check(p.returncode == 0 and sql("SELECT status FROM lifeui.agent_runs ORDER BY started_at DESC LIMIT 1") == "skipped" and sql("SELECT count(*) FROM lifeui.agent_messages") == str(int(before) + 2),
          "midday with an empty inbox is skipped (no message, no cost)", (p.stdout + p.stderr)[-150:])

    owner({"kind": "inbox", "body": "Left for the cloud run"})
    p = go("--where", "pc", "--slot", "evening")
    check(p.returncode == 0 and sql("SELECT status FROM lifeui.agent_inbox WHERE body = 'Left for the cloud run'") == "new", "the PC run never touches the inbox", (p.stdout + p.stderr)[-150:])

    # a failing model call: the run is marked failed and claimed items are released
    env_bad = {**env, "LIFEUI_FAKE_MODEL": "0", "ANTHROPIC_API_KEY": "sk-ant-invalid-test-key-000000", "ANTHROPIC_BASE_URL": "http://127.0.0.1:9"}
    p = go("--where", "cloud", "--slot", "evening", e=env_bad)
    check(p.returncode == 1 and sql("SELECT status FROM lifeui.agent_runs ORDER BY started_at DESC LIMIT 1") == "failed", "a model failure marks the run failed (exit 1)", (p.stdout + p.stderr)[-150:])
    check(sql("SELECT status FROM lifeui.agent_inbox WHERE body = 'Left for the cloud run'") == "needs_answer", "a failed run never leaves an item stuck as picked_up")
    check("sk-ant-invalid" not in p.stdout + p.stderr and "sk-ant-invalid" not in sql("SELECT string_agg(summary, ' ') FROM lifeui.agent_runs"), "the API key never reaches the output or the run log")

    owner({"kind": "switch", "enabled": False})
    runs = sql("SELECT count(*) FROM lifeui.agent_runs")
    p = go("--where", "cloud", "--slot", "morning")
    check(p.returncode == 0 and "switched off" in p.stdout and sql("SELECT count(*) FROM lifeui.agent_runs") == runs, "with the kill switch off the script opens no run")


def check_audit_flow(pg: LocalPostgres, token: str) -> None:
    """Phase 7 audit trail: everything the earlier flows did must be on record, and the record must be safe."""
    def n(where: str = "true") -> int:
        return int(pg.psql(f"SELECT count(*) FROM lifeui.audit_log WHERE {where}").stdout.strip())

    def audit(query=""):
        s, _, d, _ = api("GET", "/api/audit" + query, cookie=token)
        return s, d

    # --- login required, read-only
    check(api("GET", "/api/audit")[0] == 401, "GET /api/audit without a session is refused (401)")
    check(api("GET", "/api/audit", cookie="forged-token-value")[0] == 401, "a forged cookie cannot read the audit log (401)")
    check(api("POST", "/api/audit", {"action": "login_ok"}, cookie=token)[0] == 405, "POST /api/audit is refused (405): the log cannot be written through the API")

    # --- what the earlier flows did is on record, exactly
    expected = {"setup_failed": 1, "setup_ok": 1, "login_failed": 5, "login_locked": 2, "login_ok": 1, "logout": 1}
    got = {a: n(f"action = '{a}'") for a in expected}
    check(got == expected, "login history: wrong setup code, setup, 5 wrong passwords, 2 locked attempts, 1 login, 1 logout", str(got))
    events_rows = int(pg.psql("SELECT count(*) FROM lifeui.events").stdout.strip())
    check(n("action = 'event_create'") == events_rows and events_rows > 0, "every event row has exactly one audit record (refused writes left none)", f"{events_rows} events")
    check(pg.psql("SELECT count(*) FROM lifeui.audit_log a JOIN lifeui.events e ON e.id::text = a.target WHERE a.action = 'event_create'").stdout.strip() == str(events_rows),
          "each event record points at its event (target = id)")

    # --- a write and a read made now are added, with the caller's IP
    before_events = n("action = 'event_create'")
    s, _, d, _ = api("POST", "/api/events", {"class": "Milestone", "name": "Audited", "status": "Completed", "occurred_on": "2026-06-01"},
                     cookie=token, headers={"X-Forwarded-For": "198.51.100.4, 10.0.0.1"})
    new_id = (d or {}).get("event", {}).get("id", "")
    check(s == 201 and n("action = 'event_create'") == before_events + 1, "a new event adds one audit record", str(s))
    check(pg.psql(f"SELECT ip || '/' || (detail->>'class') FROM lifeui.audit_log WHERE target = '{new_id}'").stdout.strip() == "198.51.100.4/Milestone",
          "the record has the first forwarded IP address and the event class")
    api("POST", "/api/events", {"name": "Odd header", "status": "Planned", "occurred_on": "2026-06-02"}, cookie=token,
        headers={"X-Forwarded-For": "not an ip; DROP TABLE x"})
    check(n("action = 'event_create'") == before_events + 2, "a second write adds one more audit record")
    check(pg.psql("SELECT count(*) FROM lifeui.audit_log WHERE action = 'event_create' AND ip IS NOT NULL AND ip !~ '^[0-9a-fA-F.:]+$'").stdout.strip() == "0"
          and pg.psql("SELECT count(*) FROM lifeui.audit_log WHERE ip ILIKE '%drop%'").stdout.strip() == "0", "a malformed forwarding header is discarded, never stored")
    api("POST", "/api/events", {"name": "Refused", "status": "Done"}, cookie=token)
    check(n("action = 'event_create'") == before_events + 2, "a refused write (400) leaves no audit record")
    api("POST", "/api/auth_logout", {}, cookie="forged-token-value")
    check(n("action = 'logout'") == 1, "logout with a forged cookie is not recorded")

    # --- no secrets, no content
    check(n("detail::text ILIKE '%passport%' OR detail::text ILIKE '%vault://%' OR detail::text ILIKE '%hunter2%' OR detail::text ILIKE '%password%' OR target ILIKE '%passport%'") == 0,
          "the audit log holds no names, pointers or secrets, only ids, classes and counts")
    check(pg.psql(f"SELECT count(*) FROM lifeui.audit_log WHERE detail::text LIKE '%{TEST_PASSWORD}%'").stdout.strip() == "0", "no password reached the audit log")

    # --- the API view
    s, d = audit()
    entries = (d or {}).get("entries", [])
    check(s == 200 and d["count"] == len(entries) and 0 < len(entries) <= 50, "GET /api/audit lists entries (default 50)", str(s))
    times = [e["at"] for e in entries]
    check(times == sorted(times, reverse=True), "entries are newest first")
    check(set(entries[0]) == {"id", "at", "action", "target", "ip", "detail"}, "each entry has id, at, action, target, ip, detail", str(sorted(entries[0])))
    s, d = audit("?action=event_create&limit=1")
    newest = pg.psql("SELECT target FROM lifeui.audit_log WHERE action = 'event_create' ORDER BY at DESC LIMIT 1").stdout.strip()
    check(s == 200 and d["count"] == 1 and d["entries"][0]["action"] == "event_create" and d["entries"][0]["target"] == newest,
          "action filter and limit: the newest event_create is the last one made")
    s, d = audit("?action=login_failed")
    check(s == 200 and d["count"] == 5 and all(e["action"] == "login_failed" for e in d["entries"]), "action filter: the 5 failed logins", str(d.get("count")))
    for query in ("?limit=0", "?limit=201", "?limit=abc", "?action=drop_table", "?action=login_ok%27%20OR%201=1"):
        check(audit(query)[0] == 400, f"bad filter {query} is refused (400)")
    check(audit("?limit=0&action=nope")[1].get("details", []).__len__() == 2, "two bad filters: both problems are listed")

    # --- append-only, enforced by the database itself
    before = n()
    for sql, label in (("UPDATE lifeui.audit_log SET action = 'login_ok'", "UPDATE"), ("DELETE FROM lifeui.audit_log", "DELETE"), ("TRUNCATE lifeui.audit_log", "TRUNCATE")):
        p = pg.psql(sql, check_ok=False)
        check(p.returncode != 0 and "append-only" in p.stderr, f"the database refuses {label} on the audit log", p.stderr.strip()[:80])
    check(n() == before, "the audit log is unchanged after the attempts")
    p = pg.psql("INSERT INTO lifeui.audit_log (action) VALUES ('made_up')", check_ok=False)
    check(p.returncode != 0 and "audit_log_action_check" in p.stderr, "the database refuses an unknown action (CHECK)", p.stderr.strip()[:80])


def check_agent_flow(pg: LocalPostgres, token: str) -> None:
    """The Agent tab: inbox (owner -> agent), feed (agent -> owner), run log. Login required, validated, audited without content."""
    def post(body, **kw):
        return api("POST", "/api/agent", body, cookie=token, **kw)

    def view(name, query=""):
        s, _, d, _ = api("GET", f"/api/agent?view={name}{query}", cookie=token)
        return s, d

    def audits(action: str) -> int:
        return int(pg.psql(f"SELECT count(*) FROM lifeui.audit_log WHERE action = '{action}'").stdout.strip())

    # --- login required, same-origin only
    check(api("GET", "/api/agent")[0] == 401, "GET /api/agent without a session is refused (401)")
    check(api("POST", "/api/agent", {"kind": "inbox", "body": "x"})[0] == 401, "POST /api/agent without a session is refused (401)")
    check(api("GET", "/api/agent", cookie="forged-token-value")[0] == 401, "a forged cookie cannot read the agent inbox (401)")
    check(post({"kind": "inbox", "body": "x"}, headers={"Origin": "https://evil.example", "Host": f"127.0.0.1:{PORT_WEB}"})[0] == 403, "cross-site POST to /api/agent is refused (403)")

    # --- empty to begin with
    for name in ("inbox", "feed", "runs"):
        s, d = view(name)
        check(s == 200 and d["count"] == 0 and d["items"] == [], f"the {name} view starts empty", str(s))

    # --- inbox: create, validate, status
    before = audits("agent_inbox_create")
    s, _, d, _ = post({"kind": "inbox", "body": "  Add dentist Friday 15:00 ✓ "})
    item = (d or {}).get("item", {})
    check(s == 201 and item.get("body") == "Add dentist Friday 15:00 ✓" and item.get("status") == "new", "an inbox item is saved trimmed, status new, unicode intact", str(s))
    check(audits("agent_inbox_create") == before + 1, "creating an inbox item adds one audit record")
    check(pg.psql("SELECT count(*) FROM lifeui.audit_log WHERE detail::text ILIKE '%dentist%' OR target ILIKE '%dentist%'").stdout.strip() == "0", "the audit log never holds the inbox text")
    for bad, label in (({"kind": "inbox", "body": "   "}, "blank text"), ({"kind": "inbox", "body": "x" * 2001}, "2001 characters"),
                       ({"kind": "inbox", "body": "x", "status": "done"}, "an unknown field"), ({"kind": "nope"}, "an unknown kind"),
                       ({"kind": "inbox_status", "id": "not-a-uuid", "status": "done"}, "a bad id"),
                       ({"kind": "inbox_status", "id": item.get("id", ""), "status": "picked_up"}, "an agent-only status")):
        check(post(bad)[0] == 400, f"{label} is refused (400)")
    check(int(pg.psql("SELECT count(*) FROM lifeui.agent_inbox").stdout.strip()) == 1, "refused writes left no rows")
    s, _, d, _ = post({"kind": "inbox_status", "id": "00000000-0000-4000-8000-000000000000", "status": "done"})
    check(s == 404, "changing an unknown inbox item answers 404", str(s))

    post({"kind": "inbox", "body": "Second task"})
    s, _, d, _ = post({"kind": "inbox_status", "id": item["id"], "status": "done"})
    check(s == 200 and d["item"]["status"] == "done", "the owner can mark an item done", str(s))
    check(audits("agent_inbox_update") == 1, "a status change adds one audit record")
    s, d = view("inbox")
    check(s == 200 and [i["body"] for i in d["items"]] == ["Second task", "Add dentist Friday 15:00 ✓"], "open items are listed before finished ones")

    # --- feed
    s, _, d, _ = post({"kind": "message", "message_kind": "briefing", "slot": "morning", "title": "Morning briefing", "body": "Today: 2 events"})
    check(s == 201 and d["message"]["origin"] == "manual", "a hand-written feed message is saved with origin 'manual'", str(s))
    post({"kind": "message", "message_kind": "note", "title": "Later", "body": "second"})
    s, d = view("feed")
    check(s == 200 and [m["title"] for m in d["items"]] == ["Later", "Morning briefing"], "the feed lists newest first")
    for bad in ({"kind": "message", "message_kind": "gossip", "title": "t", "body": "b"}, {"kind": "message", "message_kind": "note", "title": "", "body": "b"},
                {"kind": "message", "message_kind": "note", "slot": "night", "title": "t", "body": "b"}):
        check(post(bad)[0] == 400, "an invalid feed message is refused (400)")

    # --- views and limits
    check(view("runs")[1]["items"] == [], "the run log is empty until the runner exists")
    check(view("nope")[0] == 400, "an unknown view is refused (400)")
    check(view("feed", "&limit=0")[0] == 400 and view("feed", "&limit=201")[0] == 400, "limit outside 1-200 is refused (400)")
    check(view("feed", "&limit=1")[1]["count"] == 1, "limit=1 returns one message")

    # --- the database is the last line of defence
    for sql, label in (("INSERT INTO lifeui.agent_inbox (body, status) VALUES ('x', 'made_up')", "unknown inbox status"),
                       ("INSERT INTO lifeui.agent_inbox (body) VALUES ('')", "empty inbox text"),
                       ("INSERT INTO lifeui.agent_messages (kind, title, body) VALUES ('gossip', 't', 'b')", "unknown message kind"),
                       ("INSERT INTO lifeui.agent_runs (slot) VALUES ('night')", "unknown run slot")):
        p = pg.psql(sql, check_ok=False)
        check(p.returncode != 0 and "check" in p.stderr.lower(), f"the database refuses {label} (CHECK)", p.stderr.strip()[:80])


def check_runner_flow(pg: LocalPostgres, token: str) -> None:
    """The scheduled agent: kill switch (off by default), runner tokens (hashed, revocable, scoped), the runner API."""
    def owner(body):
        return api("POST", "/api/agent", body, cookie=token)

    def runner(body, bearer):
        return api("POST", "/api/runner", body, headers={"Authorization": f"Bearer {bearer}"})

    def sql(query: str) -> str:
        return pg.psql(query).stdout.strip()

    # --- the switch starts OFF; settings need a login
    check(api("GET", "/api/agent?view=settings")[0] == 401, "settings need a login (401)")
    s, _, d, _ = api("GET", "/api/agent?view=settings", cookie=token)
    check(s == 200 and d["enabled"] is False and d["tokens"] == [], "the agent starts switched OFF with no tokens", str(s))

    # --- tokens: label rules, shown once, only the hash is stored
    check(owner({"kind": "token_create", "label": " "})[0] == 400, "a token needs a label (400)")
    s, _, d, _ = owner({"kind": "token_create", "label": "my pc"})
    t1, t1_id = (d or {}).get("token", ""), (d or {}).get("id", "")
    check(s == 201 and t1.startswith("lua_") and len(t1) > 40, "a runner token is created and shown once", str(s))
    check(sql(f"SELECT count(*) FROM lifeui.agent_tokens WHERE token_hash = '{t1}'") == "0" and sql("SELECT count(*) FROM lifeui.agent_tokens WHERE token_hash ~ '^[0-9a-f]{64}$'") == "1",
          "only the SHA-256 of the token is stored, never the token")
    s, _, d, _ = api("GET", "/api/agent?view=settings", cookie=token)
    check(t1 not in json.dumps(d) and "token_hash" not in json.dumps(d) and d["tokens"][0]["label"] == "my pc", "the settings view never shows the token or its hash")
    check(sql("SELECT count(*) FROM lifeui.audit_log WHERE action = 'agent_token_create'") == "1", "creating a token is audited")
    check(sql(f"SELECT count(*) FROM lifeui.audit_log WHERE detail::text LIKE '%{t1}%' OR target LIKE '%{t1}%'") == "0", "the token never reaches the audit log")

    # --- the runner API needs a valid token and is not reachable with the owner's session
    check(api("GET", "/api/runner")[0] == 401, "GET /api/runner without a token is refused (401)")
    check(api("GET", "/api/runner", cookie=token)[0] == 401, "the owner's login cookie does not open the runner API (401)")
    check(runner({"action": "run_start", "slot": "morning"}, "lua_forged")[0] == 401, "a forged runner token is refused (401)")
    check(api("GET", "/api/runner", headers={"Authorization": f"Basic {t1}"})[0] == 401, "only the Bearer scheme is accepted (401)")
    s, _, d, _ = api("GET", "/api/runner", headers={"Authorization": f"Bearer {t1}"})
    check(s == 200 and d["enabled"] is False, "a valid token can ask whether the agent is enabled", str(s))
    check(sql(f"SELECT count(*) FROM lifeui.agent_tokens WHERE id = '{t1_id}' AND last_used_at IS NOT NULL") == "1", "using a token records last_used_at")
    # the owner's endpoints do not accept a runner token either
    check(api("GET", "/api/events", headers={"Authorization": f"Bearer {t1}"})[0] == 401, "a runner token cannot read events (401)")
    check(api("GET", "/api/agent?view=inbox", headers={"Authorization": f"Bearer {t1}"})[0] == 401, "a runner token cannot use the owner's agent endpoint (401)")
    check(api("GET", "/api/events", headers={"Authorization": f"Bearer {t1}"})[0] == 401, "a runner token cannot read events (401)")

    # --- kill switch: off means every runner write is refused
    s, _, d, _ = runner({"action": "run_start", "slot": "morning"}, t1)
    check(s == 403, "while switched off the runner is refused (403)", str(s))
    check(sql("SELECT count(*) FROM lifeui.agent_runs") == "0", "no run was created while switched off")
    check(owner({"kind": "switch", "enabled": "yes"})[0] == 400, "the switch needs a real boolean (400)")
    s, _, d, _ = owner({"kind": "switch", "enabled": True})
    check(s == 200 and d["enabled"] is True and sql("SELECT count(*) FROM lifeui.audit_log WHERE action = 'agent_switch'") == "1", "the owner switches the agent on (audited)", str(s))

    # --- a full scheduled run (start from an empty inbox: the owner-side flow above left items behind)
    pg.psql("DELETE FROM lifeui.agent_inbox")
    owner({"kind": "inbox", "body": "Add dentist Friday 15:00"})
    owner({"kind": "inbox", "body": "Buy a train ticket"})
    for bad, label in (({"action": "run_start", "slot": "manual"}, "slot manual"), ({"action": "run_start", "slot": "night"}, "slot night"),
                       ({"action": "create_event", "name": "x"}, "an action outside the runner's set"), ({"action": "vault_read"}, "reading the Vault"),
                       ({"action": "run_start", "slot": "morning", "extra": 1}, "an unknown field")):
        check(runner(bad, t1)[0] == 400, f"the runner is refused {label} (400)")
    s, _, d, _ = runner({"action": "run_start", "slot": "morning"}, t1)
    run_id = (d or {}).get("run", {}).get("id", "")
    check(s == 201 and d["run"]["status"] == "running", "the runner opens a morning run", str(s))
    check(runner({"action": "inbox_claim", "run_id": "nope"}, t1)[0] == 400, "a bad run id is refused (400)")
    check(runner({"action": "inbox_claim", "run_id": "00000000-0000-4000-8000-000000000000"}, t1)[0] == 409, "claiming for an unknown run is refused (409)")
    s, _, d, _ = runner({"action": "inbox_claim", "run_id": run_id, "limit": 1}, t1)
    check(s == 200 and d["count"] == 1 and d["items"][0]["body"] == "Add dentist Friday 15:00" and d["items"][0]["status"] == "picked_up", "the runner takes the oldest new item (picked_up)", str(s))
    s, _, d, _ = runner({"action": "inbox_claim", "run_id": run_id}, t1)
    check(s == 200 and d["count"] == 1 and d["items"][0]["body"] == "Buy a train ticket", "a second claim only gets items nobody has taken")
    s, _, d, _ = runner({"action": "inbox_claim", "run_id": run_id}, t1)
    check(s == 200 and d["count"] == 0, "claiming again returns nothing (no double pick-up)")
    first_id = sql("SELECT id FROM lifeui.agent_inbox ORDER BY created_at LIMIT 1")
    s, _, d, _ = runner({"action": "inbox_reply", "id": first_id, "status": "done", "reply": "Added to the calendar."}, t1)
    check(s == 200 and d["item"]["status"] == "done" and d["item"]["reply"] == "Added to the calendar.", "the runner answers an item it picked up", str(s))
    check(runner({"action": "inbox_reply", "id": first_id, "status": "done", "reply": "again"}, t1)[0] == 409, "an item that is already answered cannot be answered again (409)")
    check(runner({"action": "inbox_reply", "id": first_id, "status": "new", "reply": "x"}, t1)[0] == 400, "the runner cannot set an arbitrary status (400)")
    second_id = sql("SELECT id FROM lifeui.agent_inbox WHERE body = 'Buy a train ticket'")
    good_p = {"class": "Appointment", "name": "Dentist", "date": "2030-01-10", "time": "15:00"}
    for bad, label in (({**good_p, "class": "Transaction"}, "a class outside the proposal set"), ({**good_p, "date": "2030-02-30"}, "an impossible date"),
                       ({**good_p, "time": "3pm"}, "a time that is not HH:MM"), ({**good_p, "sensitivity": "Public"}, "an unknown proposal field")):
        check(runner({"action": "inbox_reply", "id": second_id, "status": "needs_answer", "reply": "x", "proposal": bad}, t1)[0] == 400, f"a proposal with {label} is refused (400)")
    check(runner({"action": "inbox_reply", "id": second_id, "status": "done", "reply": "x", "proposal": good_p}, t1)[0] == 400, "a proposal must wait for the owner (status done refused, 400)")
    owner({"kind": "inbox", "body": "Untouched by the runner"})
    untouched = sql("SELECT id FROM lifeui.agent_inbox WHERE body = 'Untouched by the runner'")
    check(runner({"action": "inbox_reply", "id": untouched, "status": "done", "reply": "x"}, t1)[0] == 409, "an item the runner has not picked up cannot be answered (409)")
    s, _, d, _ = runner({"action": "message", "run_id": run_id, "message_kind": "briefing", "slot": "morning", "title": "Morning briefing", "body": "Today: 1 event"}, t1)
    check(s == 201 and d["message"]["origin"] == "agent", "the runner posts a briefing with origin 'agent'", str(s))
    check(runner({"action": "message", "run_id": run_id, "message_kind": "gossip", "title": "t", "body": "b"}, t1)[0] == 400, "an invalid message is refused (400)")
    s, _, d, _ = runner({"action": "run_finish", "run_id": run_id, "status": "ok", "summary": "2 items handled", "tool_calls": 4, "tokens": 1200}, t1)
    check(s == 200 and d["run"]["status"] == "ok" and d["run"]["finished_at"] and d["run"]["tokens"] == 1200, "the runner finishes the run with a summary and counts", str(s))
    check(runner({"action": "run_finish", "run_id": run_id, "status": "ok"}, t1)[0] == 409, "a finished run cannot be finished again (409)")
    check(runner({"action": "message", "run_id": run_id, "message_kind": "note", "title": "t", "body": "b"}, t1)[0] == 409, "a finished run cannot post any more (409)")
    s, _, d, _ = api("GET", "/api/agent?view=runs", cookie=token)
    check(s == 200 and d["count"] == 1 and d["items"][0]["summary"] == "2 items handled", "the owner sees the run in the Runs view")
    check(sql("SELECT count(*) FROM lifeui.audit_log WHERE action = 'agent_run'") == "2", "a run is audited at start and at finish")
    check(sql("SELECT count(*) FROM lifeui.audit_log WHERE detail::text ILIKE '%dentist%' OR detail::text ILIKE '%briefing%' OR detail::text ILIKE '%handled%'") == "0", "the audit log holds no inbox, feed or summary text")

    # --- kill switch mid-flight, revoke, limits
    owner({"kind": "switch", "enabled": False})
    check(runner({"action": "run_start", "slot": "midday"}, t1)[0] == 403, "switching off stops the runner at once (403)")
    owner({"kind": "switch", "enabled": True})
    check(owner({"kind": "token_revoke", "id": "nope"})[0] == 400, "revoking needs a UUID (400)")
    check(owner({"kind": "token_revoke", "id": "00000000-0000-4000-8000-000000000000"})[0] == 404, "revoking an unknown token answers 404")
    s, _, d, _ = owner({"kind": "token_create", "label": "second"})
    t2 = d["token"]
    check(owner({"kind": "token_revoke", "id": t1_id})[0] == 200, "the owner revokes the first token")
    check(runner({"action": "run_start", "slot": "midday"}, t1)[0] == 401, "a revoked token stops working (401)")
    check(api("GET", "/api/runner", headers={"Authorization": f"Bearer {t1}"})[0] == 401, "a revoked token cannot even read the status (401)")
    check(runner({"action": "run_start", "slot": "midday"}, t2)[0] == 201, "the second token still works")
    check(owner({"kind": "token_revoke", "id": t1_id})[0] == 404, "a token cannot be revoked twice (404)")
    check(sql("SELECT count(*) FROM lifeui.agent_tokens WHERE revoked_at IS NOT NULL") == "1", "the revoked token stays in the table for the record")
    for i in range(4):  # t2 is the only active token now, so four more make five
        check(owner({"kind": "token_create", "label": f"extra {i}"})[0] == 201, f"token {i + 2} of 5 is created")
    check(owner({"kind": "token_create", "label": "one too many"})[0] == 409, "at most 5 runner tokens can be active at once (409)")
    check(sql("SELECT count(*) FROM lifeui.agent_tokens WHERE revoked_at IS NULL") == "5", "the limit holds: still 5 active tokens")
    p = pg.psql("INSERT INTO lifeui.agent_settings (id, enabled) VALUES (2, true)", check_ok=False)
    check(p.returncode != 0 and "check" in p.stderr.lower(), "the database allows only one settings row (CHECK)", p.stderr.strip()[:80])


def check_news_flow(pg: LocalPostgres, token: str) -> None:
    """News tab: the home server publishes with a runner token (POST /api/runner/news, a vercel.json rewrite), the owner
    reads, marks and dismisses; a re-publish replaces the briefing and keeps the marks; the stay-awake switch."""
    def owner(path, body):
        return api("POST", path, body, cookie=token)

    def sql(query: str) -> str:
        return pg.psql(query).stdout.strip()

    fixture = json.loads((ROOT / "tools" / "news" / "fixture_briefing.json").read_text(encoding="utf-8"))
    # a runner token of our own (the runner tests left the maximum of 5 active: revoke one first)
    spare = sql("SELECT id FROM lifeui.agent_tokens WHERE revoked_at IS NULL ORDER BY created_at LIMIT 1")
    if spare:
        owner("/api/agent", {"kind": "token_revoke", "id": spare})
    s, _, d, _ = owner("/api/agent", {"kind": "token_create", "label": "news"})
    bearer = {"Authorization": f"Bearer {d.get('token', '')}"}
    owner("/api/agent", {"kind": "switch", "enabled": True})

    def publish(body):
        return api("POST", "/api/runner/news", body, headers=bearer)

    # --- who may do what
    check(api("GET", "/api/news")[0] == 401, "the News tab needs a login (401)")
    check(api("POST", "/api/runner/news", fixture)[0] == 401, "publishing needs a runner token (401)")
    check(api("POST", "/api/runner/news", fixture, cookie=token)[0] == 401, "the owner's cookie is not a runner token (401)")
    s, _, d, _ = api("GET", "/api/news", cookie=token)
    check(s == 200 and d["briefing"] is None and d["dates"] == [] and d["stay_awake"]["tonight"] is False, "no briefing yet, stay-awake off", str(s))

    # --- publish the real fixture through the rewrite
    s, _, d, _ = publish(fixture)
    check(s == 201 and d["briefing"]["items"] == len(fixture["items"]), "the home server publishes a briefing (201)", f"{s} {d}")
    s, _, d, _ = api("GET", "/api/news", cookie=token)
    b = d.get("briefing") or {}
    check(s == 200 and b.get("date") == fixture["date"] and [i["url"] for i in b.get("items", [])] == [i["url"] for i in fixture["items"]],
          "the owner reads it back, items in the published order")
    check(b.get("weather", {}).get("summary") == fixture["weather"]["summary"] and b.get("server_status", {}).get("gpu_temp_c") == fixture["server_status"]["gpu_temp_c"],
          "weather and server status come back as published")
    check(d["dates"] == [fixture["date"]], "the date picker lists the date")
    first = b["items"][0]

    # --- marks
    check(owner("/api/news", {"kind": "item", "id": first["id"], "action": "delete"})[0] == 400, "an unknown action is refused (400)")
    check(owner("/api/news", {"kind": "item", "id": "00000000-0000-4000-8000-000000000000", "action": "read"})[0] == 404, "an unknown item answers 404")
    s, _, d, _ = owner("/api/news", {"kind": "item", "id": first["id"], "action": "save"})
    check(s == 200 and d["item"]["saved_at"] and d["item"]["read_at"], "saving an item also marks it read", str(s))
    task = "SELECT name || '|' || status || '|' || origin || '|' || (props->>'url') || '|' || (props->>'list') FROM lifeui.entities WHERE class_iri LIKE '%#Task' AND origin = 'app'"
    check(sql(task) == f"Read: {first['title']}|Planned|app|{first['url']}|reading", "saving files a reading-list Task (origin app)", sql(task))
    owner("/api/news", {"kind": "item", "id": first["id"], "action": "save"})
    check(sql("SELECT count(*) FROM lifeui.entities WHERE origin = 'app'") == "1", "saving twice keeps one task")
    s, _, d, _ = api("GET", "/api/entities?class=Task", cookie=token)
    check(s == 200 and any(e["name"].startswith("Read: ") for e in d.get("entities", [])), "the reading-list task shows with the other tasks (Home)")
    owner("/api/news", {"kind": "item", "id": first["id"], "action": "unsave"})
    check(sql("SELECT count(*) FROM lifeui.entities WHERE origin = 'app'") == "0", "unsaving removes the task")
    owner("/api/news", {"kind": "item", "id": first["id"], "action": "save"})
    second = b["items"][1]
    owner("/api/news", {"kind": "item", "id": second["id"], "action": "dismiss"})
    check(sql(f"SELECT dismissed_at IS NOT NULL FROM lifeui.news_items WHERE id = '{second['id']}'") == "t", "dismiss is stored")
    s, _, _, _ = api("POST", "/api/news", {"kind": "item", "id": first["id"], "action": "read"}, cookie=token,
                     headers={"Origin": "https://evil.example", "Host": f"127.0.0.1:{PORT_WEB}"})
    check(s == 403, "a cross-site POST to /api/news is refused (403)", str(s))

    # --- re-publish the same day: replaced, marks survive, dropped items go
    again = json.loads(json.dumps(fixture))
    again["intro"] = "3 things that matter today\n- republished"
    again["items"][0]["summary"] = "Updated summary."
    dropped = again["items"].pop()
    s, _, d, _ = publish(again)
    check(s == 201, "publishing the same date again is accepted", str(s))
    s, _, d, _ = api("GET", f"/api/news?date={fixture['date']}", cookie=token)
    b = d.get("briefing") or {}
    check(sql("SELECT count(*) FROM lifeui.news_briefings") == "1" and "republished" in b.get("intro", ""), "the briefing for that date is replaced, not duplicated")
    kept = next((i for i in b.get("items", []) if i["url"] == first["url"]), {})
    check(kept.get("summary") == "Updated summary." and kept.get("saved_at") and kept.get("id") == first["id"], "the owner's marks survive a re-publish")
    check(dropped["url"] not in [i["url"] for i in b.get("items", [])] and len(b.get("items", [])) == len(fixture["items"]) - 1, "an item left out of the new version is removed")

    # --- validation and the kill switch
    bad = json.loads(json.dumps(fixture))
    bad["items"][0]["url"] = "javascript:alert(1)"
    bad["items"][1]["importance"] = 42
    s, _, d, _ = publish(bad)
    check(s == 400 and len(d.get("details", [])) >= 2, "a bad briefing is refused with every problem listed (400)", str(d)[:120])
    check(api("POST", "/api/runner/news", raw=b"x" * (300 * 1024), headers=bearer)[0] in (400, 413), "an oversized publish is refused")
    owner("/api/agent", {"kind": "switch", "enabled": False})
    check(publish(fixture)[0] == 403, "with the agent switched off, publishing is refused (403)")

    # --- stay awake tonight: the owner sets it, the server reads it (even with the agent off)
    s, _, d, _ = owner("/api/news", {"kind": "stay_awake", "on": True})
    check(s == 200 and d["stay_awake"]["tonight"] is True, "the owner turns on 'stay awake tonight'", str(s))
    s, _, d, _ = api("GET", "/api/runner", headers=bearer)
    check(s == 200 and d["stay_awake"]["tonight"] is True and d["enabled"] is False, "the home server sees it through GET /api/runner, agent off or not", str(d))
    owner("/api/news", {"kind": "stay_awake", "on": False})
    check(api("GET", "/api/runner", headers=bearer)[2]["stay_awake"]["tonight"] is False, "turning it off is seen too")

    check_news_phase6(pg, token, bearer, fixture)

    # --- audit: actions, never text
    check(sql("SELECT count(*) FROM lifeui.audit_log WHERE action = 'news_publish'") == "4", "each publish is audited")
    check(sql("SELECT count(*) FROM lifeui.audit_log WHERE action = 'agent_stay_awake'") == "2", "the stay-awake switch is audited")
    check(sql("SELECT count(*) FROM lifeui.audit_log WHERE action LIKE 'news%' AND (detail::text ILIKE '%gemini%' OR detail::text ILIKE '%http%')") == "0",
          "the audit log holds no news titles or links")


def check_news_phase6(pg: LocalPostgres, token: str, bearer: dict, fixture: dict) -> None:
    """Today card (missing briefing after 09:45), signals for the server, the weekly digest, extras."""
    from zoneinfo import ZoneInfo  # noqa: PLC0415

    def owner(path, body):
        return api("POST", path, body, cookie=token)

    def sql(query: str) -> str:
        return pg.psql(query).stdout.strip()

    local = dt.datetime.now(ZoneInfo(os.environ.get("LIFEUI_TIMEZONE") or "UTC"))  # the functions use the same zone
    today, late = local.date().isoformat(), local.strftime("%H:%M") >= "09:45"
    owner("/api/agent", {"kind": "switch", "enabled": True})

    # --- Today: missing briefing (logged once a day), inbox count, LifeUI events today
    owner("/api/agent", {"kind": "inbox", "body": "read this"})
    open_items = int(sql("SELECT count(*) FROM lifeui.agent_inbox WHERE status IN ('new', 'picked_up', 'needs_answer')"))
    s, _, d, _ = api("GET", "/api/news", cookie=token)
    t = d.get("today") or {}
    check(s == 200 and t.get("date") == today and t.get("inbox_open") == open_items >= 1, "GET /api/news has today's date and the open inbox count", str(t))
    check(t.get("missing") is late, f"no briefing today: missing={late} at {local:%H:%M} local time (banner after 09:45)", str(t))
    api("GET", "/api/news", cookie=token)
    check(sql("SELECT count(*) FROM lifeui.audit_log WHERE action = 'news_missing'") == ("1" if late else "0"), "a missing briefing is logged once per day")

    # --- today's briefing with extras (calendar, feedback report)
    daily = json.loads(json.dumps(fixture))
    daily["date"] = today
    for n, it in enumerate(daily["items"]):
        it["url"] = f"https://today.example/{n}"
    daily["extras"] = {"calendar": [{"time": "15:00", "title": "Dentist"}], "feedback": {"month": "2026-10", "items": 40, "changes": []}}
    s, _, d, _ = api("POST", "/api/runner/news", daily, headers=bearer)
    check(s == 201, "today's briefing with extras is published", f"{s} {d}")
    s, _, d, _ = api("GET", "/api/news", cookie=token)
    check(d["today"]["missing"] is False and d["briefing"]["extras"]["calendar"][0]["title"] == "Dentist", "today: not missing; extras come back")

    # --- signals for the home server (runner token only)
    first = d["briefing"]["items"][0]
    owner("/api/news", {"kind": "item", "id": first["id"], "action": "save"})
    check(api("GET", "/api/runner?route=signals&days=7", cookie=token)[0] == 401, "signals need a runner token (401)")
    check(api("GET", "/api/runner?route=signals&days=0", headers=bearer)[0] == 400, "signals: days must be 1-120 (400)")
    s, _, d, _ = api("GET", "/api/runner?route=signals&days=7", headers=bearer)
    mine = [i for i in d.get("items", []) if i["url"] == first["url"]]
    check(s == 200 and mine and mine[0]["saved"] is True and mine[0]["read"] is True and "intro" not in d, "signals: the week's items with the owner's marks", str(s))

    # --- weekly digest: points at daily items, shares their marks
    urls = [i["url"] for i in daily["items"][:3]]
    weekly = {"date": today, "type": "weekly", "intro": "Week", "generated_by_model": "test", "generated_at": local.isoformat(timespec="seconds"),
              "items": [], "extras": {"digest": {"urls": urls, "highlights": urls[2:], "trends": [{"title": "Agents", "text": "Many."}]}}}
    s, _, d, _ = api("POST", "/api/runner/news", weekly, headers=bearer)
    check(s == 201, "a weekly digest is published", f"{s} {d}")
    bad = json.loads(json.dumps(weekly))
    bad["items"] = [daily["items"][0]]
    check(api("POST", "/api/runner/news", bad, headers=bearer)[0] == 400, "a weekly digest with items of its own is refused (400)")
    s, _, d, _ = api("GET", "/api/news?type=weekly", cookie=token)
    wb = d.get("briefing") or {}
    check(s == 200 and [i["url"] for i in wb.get("items", [])] == urls and wb["extras"]["digest"]["trends"][0]["title"] == "Agents",
          "the weekly view lists the digest's items in order, with trends")
    check(wb["items"][0]["saved_at"] and wb["items"][0]["id"] == first["id"], "weekly and daily share the same item (marks included)")
    s, _, d, _ = api("GET", "/api/news", cookie=token)
    check(d["briefing"]["type"] == "daily" and len(d["briefing"]["items"]) == len(daily["items"]), "the daily briefing keeps all its items")


def check_entities_flow(pg: LocalPostgres, token: str) -> None:
    """The read-only knowledge-base endpoint behind the Explore tab: /api/entities."""
    base = "https://lifeui.example/id/"
    onto = "https://lifeui.example/ontology#"

    def ents(query=""):
        s, _, d, _ = api("GET", "/api/entities" + query, cookie=token)
        return s, d

    def counts() -> str:
        return pg.psql("SELECT (SELECT count(*) FROM lifeui.entities) || '/' || (SELECT count(*) FROM lifeui.entity_links)").stdout.strip()

    # --- login required, read-only
    s, _, _, _ = api("GET", "/api/entities")
    check(s == 401, "GET /api/entities without a session is refused (401)", str(s))
    s, _, _, _ = api("GET", "/api/entities", cookie="forged-token-value")
    check(s == 401, "a forged cookie cannot read entities (401)", str(s))
    s, _, _, _ = api("POST", "/api/entities", {"name": "x"}, cookie=token)
    check(s == 405, "POST /api/entities is refused (405): the endpoint is read-only", str(s))

    # --- empty state
    s, d = ents()
    check(s == 200 and d.get("count") == 0 and d.get("entities") == [] and d.get("classes") == [], "empty knowledge base: 200 with no entities and no classes", str(d)[:80])

    # --- seed (the importer does this in production; the endpoint only reads)
    pg.psql(
        "INSERT INTO lifeui.entities (iri, class_iri, name, status, props) VALUES "
        f"('{base}task-report', '{onto}Task', 'Write report', 'InProgress', '{{\"description\": \"quarterly\"}}'), "
        f"('{base}task-milk', '{onto}Task', 'Buy 100%_milk', NULL, '{{}}'), "
        f"('{base}course-algebra', '{onto}Course', 'Algebra', NULL, '{{}}'); "
        "INSERT INTO lifeui.entity_links (subject_iri, predicate, object_iri) VALUES "
        f"('{base}task-report', 'relatesTo', '{base}course-algebra')")
    seeded = counts()
    check(seeded == "3/1", "seeded 3 entities and 1 link", seeded)

    # --- list: order, class counts, filters
    s, d = ents()
    got = [(e["class"], e["name"]) for e in d.get("entities", [])]
    check(s == 200 and d["count"] == 3 and got == [("Course", "Algebra"), ("Task", "Buy 100%_milk"), ("Task", "Write report")],
          "list is sorted by class, then name", str(got))
    check(d["classes"] == [{"class": "Task", "count": 2}, {"class": "Course", "count": 1}], "class counts, biggest first", str(d["classes"]))
    for query, expected, label in (("?class=Task", ["Buy 100%_milk", "Write report"], "class filter"),
                                   ("?class=Banana", [], "class filter with no match"),
                                   ("?q=report", ["Write report"], "name search"),
                                   ("?q=ALGEBRA", ["Algebra"], "name search ignores case"),
                                   ("?q=100%25", ["Buy 100%_milk"], "'%' in a search is matched literally"),
                                   ("?q=%25", ["Buy 100%_milk"], "a lone '%' is not a wildcard"),
                                   ("?q=%20%20", ["Algebra", "Buy 100%_milk", "Write report"], "a blank search means no filter"),
                                   ("?limit=1", ["Algebra"], "limit")):
        s, d = ents(query)
        names = [e["name"] for e in d.get("entities", [])]
        check(s == 200 and names == expected, f"entities {label}", str(names))
    s, d = ents("?class=Task&limit=1")
    check(s == 200 and d["count"] == 1 and sum(c["count"] for c in d["classes"]) == 3, "the class counts ignore the filters (they describe the whole base)", str(d.get("classes")))

    # --- validation: every problem at once
    for query in ("?limit=0", "?limit=201", "?limit=abc", "?class=Bad%20Class", "?class=" + "A" * 61, "?q=" + "x" * 101):
        s, d = ents(query)
        check(s == 400, f"bad filter {query[:24]} is refused (400)", str(s))
    s, d = ents("?limit=0&class=Bad%20Class&q=" + "x" * 101)
    check(s == 400 and len(d.get("details", [])) == 3, "three bad filters: all three problems are listed", str(d.get("details")))

    # --- one entity, with relations in both directions
    s, d = ents(f"?iri={base}task-report")
    out = d.get("links_out", []) if d else []
    check(s == 200 and d["entity"]["name"] == "Write report" and d["entity"]["class"] == "Task" and d["entity"]["status"] == "InProgress"
          and d["entity"]["props"] == {"description": "quarterly"} and d["links_in"] == []
          and [(l["predicate"], l["name"], l["class"], l["kind"]) for l in out] == [("relatesTo", "Algebra", "Course", "entity")],
          "one entity: properties and the outgoing relation with the target's name", str(d)[:100])
    s, d = ents(f"?iri={base}course-algebra")
    check(s == 200 and d["links_out"] == [] and [(l["predicate"], l["name"]) for l in d["links_in"]] == [("relatesTo", "Write report")],
          "the same relation is shown as incoming on the other entity")
    s, _ = ents(f"?iri={base}nothing-here")
    check(s == 404, "an unknown entity is 404", str(s))
    event_iri = pg.psql("SELECT iri FROM lifeui.events LIMIT 1").stdout.strip()
    s, _ = ents(f"?iri={event_iri}")
    check(event_iri.startswith(base) and s == 404, "an event identifier is 404 here (events live in the Timeline)", f"{s} {event_iri[-12:]}")
    for label, iri in (("outside this knowledge base", "https://evil.example/id/x"), ("with a space", f"{base}a%20b"), ("too long", base + "a" * 300)):
        s, _ = ents(f"?iri={iri}")
        check(s == 400, f"identifier {label} is refused (400)", str(s))
    s, _ = ents(f"?iri={base}x%27%20OR%20%271%27=%271")
    check(s == 400, "a SQL-injection-looking identifier is refused (400)", str(s))
    s, d = ents(f"?class=Task%27%3B%20DROP%20TABLE%20lifeui.entities%3B--")
    check(s == 400 and counts() == seeded, "an injection-looking class is refused and nothing changed", str(s))
    s, d = ents("?q=%27%3B%20DROP%20TABLE%20lifeui.entities%3B--")
    check(s == 200 and d["count"] == 0 and counts() == seeded, "an injection-looking search is just text: no match, nothing changed", str(s))

    check(counts() == seeded, "reading never changed a row (entities/entity_links counts unchanged)")


def check_domain_flow(pg: LocalPostgres, token: str) -> None:
    """Events (timeline, versioning), with a live session."""
    def post(path, body, **kw):
        return api("POST", path, body, cookie=token, **kw)

    def events(query=""):
        s, _, d, _ = api("GET", "/api/events" + query, cookie=token)
        return s, d

    # --- login required everywhere
    for method, path, body in (("GET", "/api/events", None), ("POST", "/api/events", {"name": "x"})):
        s, _, _, _ = api(method, path, body)
        check(s == 401, f"{method} {path} without a session is refused (401)", str(s))
    s, _, _, _ = api("GET", "/api/events", cookie="forged-token-value")
    check(s == 401, "a forged cookie cannot read events (401)", str(s))
    s, _, _, _ = api("POST", "/api/events", {"name": "x"}, cookie=token, headers={"Origin": "https://evil.example", "Host": f"127.0.0.1:{PORT_WEB}"})
    check(s == 403, "cross-site POST to /api/events is refused (403)", str(s))

    # --- create + defaults
    s, _, d, _ = post("/api/events", {"class": "Milestone", "name": "First milestone", "status": "Completed",
                                      "occurred_at": "2026-05-29T10:00:00Z", "description": "smoke test"})
    e1 = (d or {}).get("event", {})
    check(s == 201 and e1.get("class") == "Milestone" and e1.get("iri", "").startswith("https://lifeui.example/id/event-")
          and e1.get("confidence") == 1.0 and e1.get("sensitivity") == "Personal" and e1.get("superseded_by") is None,
          "create an event: 201 with class, IRI, default confidence 1.0 and sensitivity Personal", f"{s} {e1.get('class')}")
    id1 = e1.get("id", "")
    check(pg.psql("SELECT s.kind FROM lifeui.events e JOIN lifeui.sources s ON s.id = e.source_id").stdout.strip() == "UserStated",
          "the event is linked to the 'UserStated' source (provenance recorded)")

    # --- validation (all problems at once) and secrets
    s, _, d, _ = post("/api/events", {"name": "x", "status": "Done"})
    check(s == 400 and d and d.get("error") == "Validation failed." and len(d.get("details", [])) >= 2, "invalid event: 400 listing every problem", str((d or {}).get("details")))
    s, _, d, _ = post("/api/events", {"class": "Transaction", "name": "Coffee", "status": "Completed", "occurred_on": "2026-05-30",
                                      "payload": {"amount": -3.5, "currency": "euro"}})
    check(s == 400 and "ISO 4217" in json.dumps(d), "Transaction with a bad currency is refused (400)", str(s))
    s, _, d, _ = post("/api/events", {"name": "x", "status": "Planned", "occurred_on": "2026-01-01", "payload": {"nested": {"Password": "hunter2"}}})
    check(s == 400 and "secrets are never stored" in json.dumps(d), "a secret field hidden in the payload is refused (400)", str(s))
    s, _, d, _ = post("/api/events", {"name": "x", "status": "Planned", "occurred_on": "2026-01-01", "colour": "red"})
    check(s == 400, "an unknown field is refused (400)", str(s))
    check(pg.psql("SELECT count(*) FROM lifeui.events WHERE payload::text ILIKE '%hunter2%'").stdout.strip() == "0", "no refused secret reached the database")

    # --- timeline: ordering and filters
    post("/api/events", {"class": "Transaction", "name": "Coffee", "status": "Completed", "occurred_on": "2026-05-30", "payload": {"amount": -3.5, "currency": "EUR"}})
    post("/api/events", {"class": "Appointment", "name": "Dentist", "status": "Planned", "started_at": "2026-06-01T09:00:00Z"})
    post("/api/events", {"class": "Workout", "name": "Run", "status": "Completed", "occurred_at": "2026-01-15T07:00:00Z"})
    s, d = events()
    names = [e["name"] for e in d.get("events", [])]
    check(s == 200 and names == ["Dentist", "Coffee", "First milestone", "Run"], "timeline is newest first by when it happened", str(names))
    for query, expected, label in (("?class=Milestone", ["First milestone"], "class filter"), ("?status=Planned", ["Dentist"], "status filter"),
                                   ("?from=2026-05-30", ["Dentist", "Coffee"], "from filter (inclusive)"),
                                   ("?to=2026-05-30", ["First milestone", "Run"], "to filter (exclusive)"),
                                   ("?limit=2", ["Dentist", "Coffee"], "limit")):
        s, d = events(query)
        got = [e["name"] for e in d.get("events", [])]
        check(s == 200 and got == expected, f"timeline {label}", str(got))
    for query in ("?limit=0", "?limit=999", "?class=Banana", "?status=Done", "?from=yesterday"):
        s, d = events(query)
        check(s == 400, f"bad filter {query} is refused (400)", str(s))
    s, d = events(f"?id={id1}")
    check(s == 200 and d["event"]["name"] == "First milestone", "get one event by id")
    check(events("?id=not-a-uuid")[0] == 400 and events("?id=00000000-0000-4000-8000-000000000000")[0] == 404, "bad id is 400, unknown id is 404")

    # --- versioning: a change is a new row, the old one is kept and linked
    before = len(events("?include_superseded=1")[1]["events"])
    s, _, d, _ = post("/api/events", {"class": "Milestone", "name": "First milestone (corrected)", "status": "Completed",
                                      "occurred_at": "2026-05-29T11:00:00Z", "supersedes": id1})
    e1b = (d or {}).get("event", {})
    check(s == 201 and e1b.get("id") not in (None, id1), "a corrected version is created (201)", str(s))
    s, d = events()
    names = [e["name"] for e in d["events"]]
    check("First milestone" not in names and "First milestone (corrected)" in names, "default timeline shows the new version only", str(names))
    s, d = events("?include_superseded=1")
    check(len(d["events"]) == before + 1, "include_superseded=1 keeps the old version too (nothing was deleted)", f"{before} -> {len(d['events'])}")
    check(events(f"?id={id1}")[1]["event"]["superseded_by"] == e1b.get("id"), "the old version points to its replacement (superseded_by)")
    s, _, d, _ = post("/api/events", {"class": "Milestone", "name": "Third try", "status": "Completed", "occurred_on": "2026-05-29", "supersedes": id1})
    check(s == 409, "an already-replaced event cannot be replaced again (409)", str(s))
    s, _, d, _ = post("/api/events", {"name": "Ghost", "status": "Planned", "occurred_on": "2026-01-01", "supersedes": "00000000-0000-4000-8000-000000000000"})
    check(s == 409, "superseding a non-existent event is refused (409)", str(s))
    check(len(events("?include_superseded=1")[1]["events"]) == before + 1, "refused supersedes left no orphan row (transaction rolled back)")

    # --- database safety net behind the Rust validation
    p = pg.psql("INSERT INTO lifeui.events (iri, class_iri, name, status) VALUES ('x', 'c', 'n', 'Planned')", check_ok=False)
    check(p.returncode != 0 and "events_has_time_check" in p.stderr, "database itself refuses an event with no time (CHECK)", p.stderr.strip()[:90])



def check_browser(edge: str, stage: str) -> None:
    with tempfile.TemporaryDirectory() as tmp:
        def dump(url: str) -> str:
            out = Path(tmp) / "dom.html"
            with open(out, "wb") as f:
                subprocess.run([edge, "--headless=new", "--disable-gpu", "--no-first-run", f"--user-data-dir={tmp}\\profile",
                                "--virtual-time-budget=12000", "--dump-dom", url], stdout=f, stderr=subprocess.DEVNULL, timeout=90)
            return out.read_text(encoding="utf-8", errors="replace")

        page = dump(f"http://localhost:{PORT_WEB}/")
        if stage == "no-db":
            check("<h1" in page and "LifeUI" in page and "Cannot reach the server" in page, "browser: app renders and shows the 'cannot reach the server' screen without a database")
        elif stage == "setup":
            check("First-time setup" in page and page.count('type="password"') == 3 and 'autocomplete="new-password"' in page,
                  "browser: fresh database shows the first-time setup form (3 password fields)")
        elif stage == "login":
            check("Log in" in page and page.count('type="password"') == 1 and 'autocomplete="current-password"' in page,
                  "browser: a visitor without a cookie sees the login form")

        smoke = dump(f"http://localhost:{PORT_WEB}/__smoke")
        match = re.search(r'<pre id="out">(.*?)</pre>', smoke, re.S)
        try:
            both = json.loads(html.unescape(match.group(1)))
            h, d, e = both["health"], both["db"], both["env"]
            check(h.get("status") == 200 and h["body"].get("ok") is True, "browser JS glue -> /api/health", f"status {h.get('status')} in {h.get('ms')} ms")
            expected_db = 503 if stage == "no-db" else 200
            check(d.get("status") == expected_db, "browser JS glue -> /api/db_ping", f"status {d.get('status')}")
            check(e.get("secureContext") and e.get("serviceWorker") and e.get("webauthn"), "browser has secure context, service worker and WebAuthn", f"host {e.get('host')}")
        except Exception as ex:
            check(False, "smoke page produced results", f"{type(ex).__name__}: {str(match.group(1))[:200] if match else 'no output'}")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--local-db", action="store_true", help="start a throwaway local PostgreSQL and run the full login flow")
    args = parser.parse_args()

    env_file = load_env()
    db_url = env_file.get("DATABASE_URL", "")
    if db_url:
        secrets.append(db_url)
        password = urlparse(db_url).password
        if password:
            secrets.append(password)

    pg: LocalPostgres | None = None
    if args.local_db:
        bindir = find_pg_bin()
        if not bindir:
            record("FAIL", "--local-db needs an installed PostgreSQL", "set PG_BIN to its bin folder")
            return 1
        pg = LocalPostgres(bindir, ROOT / "db" / "migrations")
        try:
            pg.start()
        except Exception as e:
            record("FAIL", "start throwaway PostgreSQL and apply migrations", str(e))
            pg.stop()
            return 1
        record("PASS", "throwaway PostgreSQL started and migrations applied", f"port {pg.port}, {sorted(p.name for p in (ROOT / 'db' / 'migrations').glob('*.sql'))}")
        db_url = pg.url
    stage = "local" if pg else ("remote" if db_url else "no-db")

    procs: list[subprocess.Popen] = []
    try:
        if not run(["cargo", "build", "--bins", "--quiet"], ROOT, "cargo build functions"):
            return 1
        if not (DIST / "index.html").exists() and not run(["trunk", "build", "--release"], ROOT / "web", "trunk build (web/dist was missing)"):
            return 1

        base_env = {k: v for k, v in os.environ.items() if k not in ("DATABASE_URL", "SETUP_CODE")}
        for name, port in FUNCTIONS.items():
            extra = {}
            if db_url:
                extra["DATABASE_URL"] = db_url
            if pg:
                extra["SETUP_CODE"] = SETUP_CODE
            procs.append(subprocess.Popen([str(ROOT / "target" / "debug" / f"{name}{EXE}")], env={**base_env, "VERCEL_DEV_PORT": str(port), **extra},
                                          stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL))
        map_args: list[str] = []
        for name, port in FUNCTIONS.items():
            map_args += ["--map", f"{name}={port}"]
        procs.append(subprocess.Popen([sys.executable, "-I", str(ROOT / "tools" / "dev_server.py"), "--port", str(PORT_WEB), *map_args],
                                      stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL))
        if not check(all(wait_port(p) for p in [*FUNCTIONS.values(), PORT_WEB]), "functions and dev server listening"):
            return 1

        edge = next((p for p in EDGE_CANDIDATES if Path(p).exists()), None)
        check_static()
        check_request_guards()
        if stage == "no-db":
            check_api_without_database()
            record("SKIP", "database and login checks", "run with --local-db (or create .env with DATABASE_URL for a connectivity check)")
            if edge:
                check_browser(edge, "no-db")
        else:
            if stage == "remote":
                check_database_connectivity()
                screen = check_remote_auth_status()
                record("SKIP", "login flow", "it creates the owner password, so it only runs with --local-db, never against .env's database")
                if edge and screen:
                    check_browser(edge, screen)
            if pg:
                check_database_connectivity()
                if edge:
                    check_browser(edge, "setup")
                check_auth_flow(pg)
                if edge:
                    check_browser(edge, "login")
            if not edge:
                record("SKIP", "headless Edge checks", "Edge not found")
    finally:
        for p in procs:
            p.terminate()
        for p in procs:
            try:
                p.wait(timeout=5)
            except Exception:
                p.kill()
        if pg:
            pg.stop()

    failed = [r for r in results if r[0] == "FAIL"]
    passed = sum(1 for r in results if r[0] == "PASS")
    skipped = sum(1 for r in results if r[0] == "SKIP")
    print(f"\n{passed} passed, {len(failed)} failed, {skipped} skipped")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
