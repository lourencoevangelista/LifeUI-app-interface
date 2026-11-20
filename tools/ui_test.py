#!/usr/bin/env python
"""End-to-end test of the signed-in app in a REAL browser session (headless Edge driven over the DevTools protocol).

Run:  .venv\\Scripts\\python.exe -I tools\\ui_test.py        (needs web/dist: `cd web; trunk build --release`)

Everything runs on a THROWAWAY local PostgreSQL (tools/localpg.py) with the Rust functions behind tools/dev_server.py:
it never touches the database in .env. It walks through setup, the timeline, the event form, the News tab (including
server-side validation messages), "New version", reload (the session cookie survives) and logout.
"""
from __future__ import annotations

import json
import os
from collections import Counter
import subprocess
import sys
import tempfile
import time
import urllib.parse
import urllib.request
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import smoke_test as S  # noqa: E402  (constants and helpers only; its main() is not run)
import import_knowledge as IK  # noqa: E402
from backup import conn_from_url  # noqa: E402
from localpg import LocalPostgres, find_pg_bin, free_port, EXE  # noqa: E402
import websocket  # noqa: E402

ROOT = S.ROOT
results: list[tuple[bool, str]] = []
PASSWORD = "correct horse battery staple 99"


def check(ok: bool, name: str, detail: str = "") -> bool:
    results.append((ok, name))
    print(f"[{'PASS' if ok else 'FAIL'}] {name}" + (f": {detail}" if detail and not ok else ""))
    return ok


class Browser:
    def __init__(self, edge: str, port: int):
        self.profile = tempfile.mkdtemp(prefix="lifeui-edge-")
        self.proc = subprocess.Popen(
            [edge, "--headless=new", f"--remote-debugging-port={port}", f"--user-data-dir={self.profile}", "--no-first-run",
             "--disable-gpu", "--no-default-browser-check", "about:blank"],
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        deadline = time.time() + 30
        ws_url = None
        while time.time() < deadline and not ws_url:
            try:
                pages = json.load(urllib.request.urlopen(f"http://127.0.0.1:{port}/json/list", timeout=2))
                ws_url = next((p["webSocketDebuggerUrl"] for p in pages if p.get("type") == "page"), None)
            except Exception:
                time.sleep(0.5)
        if not ws_url:
            raise RuntimeError("Edge did not start with a debugging port")
        self.ws = websocket.create_connection(ws_url, timeout=30, suppress_origin=True)
        self.next_id = 0
        self.errors: list[str] = []
        self.send("Runtime.enable")
        self.send("Log.enable")
        self.send("Page.enable")

    def send(self, method: str, **params):
        self.next_id += 1
        mid = self.next_id
        self.ws.send(json.dumps({"id": mid, "method": method, "params": params}))
        while True:
            message = json.loads(self.ws.recv())
            if message.get("method") == "Runtime.exceptionThrown":
                self.errors.append(json.dumps(message["params"]["exceptionDetails"].get("exception", {}).get("description", "exception"))[:300])
            if message.get("method") == "Log.entryAdded" and message["params"]["entry"].get("level") == "error":
                entry = message["params"]["entry"]
                # the browser logs every non-2xx fetch: the deliberate 400s and 404 (validation and "not an entity" checks) and 401s are expected
                if "favicon" not in entry.get("url", "") and not any(code in entry.get("text", "") for code in ("status of 400", "status of 401", "status of 404")):
                    self.errors.append(entry.get("text", "")[:300])
            if message.get("id") == mid:
                if "error" in message:
                    raise RuntimeError(message["error"])
                return message.get("result", {})

    def js(self, expression: str):
        result = self.send("Runtime.evaluate", expression=expression, awaitPromise=True, returnByValue=True)
        if "exceptionDetails" in result:
            raise RuntimeError(result["exceptionDetails"].get("text", "js error"))
        return result["result"].get("value")

    def goto(self, url: str):
        self.send("Page.navigate", url=url)
        time.sleep(1.0)

    def wait(self, condition: str, timeout: float = 15.0) -> bool:
        end = time.time() + timeout
        while time.time() < end:
            try:
                if self.js(f"Boolean({condition})"):
                    return True
            except RuntimeError:
                pass
            time.sleep(0.25)
        return False

    def close(self):
        try:
            self.ws.close()
        except Exception:
            pass
        self.proc.terminate()
        try:
            self.proc.wait(timeout=5)
        except Exception:
            self.proc.kill()


HELPERS = """
window.$t = (sel, txt) => [...document.querySelectorAll(sel)].find(e => !txt || e.textContent.trim() === txt);
window.$fill = (el, v) => { el.value = v; el.dispatchEvent(new Event('input', {bubbles: true})); el.dispatchEvent(new Event('change', {bubbles: true})); };
window.$label = (txt) => [...document.querySelectorAll('label')].find(l => l.firstChild && l.firstChild.textContent.trim().startsWith(txt));
window.$field = (txt) => $label(txt).querySelector('input,select,textarea');
window.$submit = () => document.querySelector('form button[type=submit]').click();
"""


def main() -> int:
    edge = next((p for p in S.EDGE_CANDIDATES if Path(p).exists()), None)
    bindir = find_pg_bin()
    if not edge or not bindir or not (S.DIST / "index.html").exists():
        print("needs Edge, an installed PostgreSQL and web/dist (cd web; trunk build --release)")
        return 2
    pg = LocalPostgres(bindir, ROOT / "db" / "migrations")
    procs: list[subprocess.Popen] = []
    browser = None
    try:
        pg.start()
        subprocess.run(["cargo", "build", "--bins", "--quiet"], cwd=ROOT, check=True)
        base_env = {k: v for k, v in os.environ.items() if k not in ("DATABASE_URL", "SETUP_CODE")}
        for name, port in S.FUNCTIONS.items():
            procs.append(subprocess.Popen([str(ROOT / "target" / "debug" / f"{name}{EXE}")],
                                          env={**base_env, "VERCEL_DEV_PORT": str(port), "DATABASE_URL": pg.url, "SETUP_CODE": S.SETUP_CODE},
                                          stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL))
        map_args: list[str] = []
        for name, port in S.FUNCTIONS.items():
            map_args += ["--map", f"{name}={port}"]
        procs.append(subprocess.Popen([sys.executable, "-I", str(ROOT / "tools" / "dev_server.py"), "--port", str(S.PORT_WEB), *map_args],
                                      stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL))
        if not check(all(S.wait_port(p) for p in [*S.FUNCTIONS.values(), S.PORT_WEB]), "functions and dev server listening"):
            return 1

        base = f"http://localhost:{S.PORT_WEB}/"
        browser = Browser(edge, free_port())
        b = browser
        b.goto(base)
        b.js(HELPERS)
        check(b.wait("document.querySelectorAll('input[type=password]').length === 3"), "fresh database shows the setup form")

        # --- setup, then the app opens on the timeline
        b.js(HELPERS)
        inputs = "[...document.querySelectorAll('input[type=password]')]"
        b.js(f"(() => {{ const i = {inputs}; $fill(i[0], {json.dumps(S.SETUP_CODE)}); $fill(i[1], {json.dumps(PASSWORD)}); $fill(i[2], {json.dumps(PASSWORD)}); $submit(); }})()")
        check(b.wait("document.querySelector('.tabs')"), "setup logs in and shows the app tabs")
        b.js(HELPERS)
        check(b.wait("document.querySelector('button.tab.on') && document.querySelector('button.tab.on').textContent.trim() === 'Home'"), "the Home tab is the active one at start (aria-current)")
        check(b.js("document.querySelector('button.tab.on').getAttribute('aria-current')") == "page", "the active tab is marked aria-current=page")
        check(b.wait("document.querySelectorAll('.cal-day').length >= 28 && document.querySelector('.cal-day.today')"), "Home shows a month calendar with today marked")
        check(b.wait("document.body.textContent.includes('No tasks yet') && document.body.textContent.includes('No Focus topics found yet')"), "Home to-do list and Focus topics are empty on a fresh database")
        b.js("$t('button.tab', 'Timeline').click()")
        check(b.wait("document.body.textContent.includes('Nothing here yet')"), "empty timeline says so")

        # --- add an event
        b.js("$t('button.fab').click()")
        check(b.wait("document.querySelector('form h2') && document.querySelector('form h2').textContent === 'Add an event'"), "the add button on the Timeline opens the event form")
        b.js("(() => { $fill($field('What happened'), 'Graduation party ✓'); $fill($field('Notes'), 'with friends'); $submit(); })()")
        check(b.wait("document.querySelector('.card strong') && document.querySelector('.card strong').textContent === 'Graduation party ✓'"), "saved event appears on the timeline (unicode intact)")

        check(b.wait("customElements.get('number-flow')"), "the vendored NumberFlow element is registered (animated counters)")
        check(b.wait("document.querySelector('#count-timeline number-flow') && document.querySelector('#count-timeline number-flow').value === 1"),
              "the Timeline counter is an animated number and shows 1 event")

        # --- server-side validation message for a Transaction without amount
        b.js("$t('button.fab').click()")
        b.wait("document.querySelector('form h2')")
        b.js("(() => { $fill($field('Kind'), 'Transaction'); $fill($field('What happened'), 'Coffee'); $submit(); })()")
        check(b.wait("document.querySelector('.problems li') && document.body.textContent.includes('amount')"), "server validation message (Transaction needs amount) is shown")
        b.js("(() => { $fill($field('Amount'), '-2,50'); $fill($field('Currency'), 'eur'); $submit(); })()")
        check(b.wait("[...document.querySelectorAll('.card strong')].some(e => e.textContent === 'Coffee')"), "Transaction with amount and currency is accepted")

        # --- filter
        b.js("(() => { const s = document.querySelector('section select'); $fill(s, 'Transaction'); })()")
        check(b.wait("document.querySelectorAll('.card').length === 1 && document.querySelector('.card strong').textContent === 'Coffee'"), "kind filter shows only transactions")

        # --- News: empty state, then a briefing as the home server publishes it (inserted directly), filter, save
        b.js("$t('button.tab', 'News').click()")
        check(b.wait("document.body.textContent.includes('No briefing yet')"), "News shows its empty state before the first briefing")
        check(b.wait("document.querySelector('.stay-awake .switch')"), "the stay-awake switch is there even without a briefing")
        pg.psql("""INSERT INTO lifeui.news_briefings (date, intro, weather, server_status, generated_by_model, generated_at)
                   VALUES ('2026-11-19', E'3 things that matter today\\n- Gemini 4 Neon', '{"summary": "Faro: 13–18 °C, no rain.", "tmin": 12.8, "tmax": 18.4, "hourly": [{"time": "09:00", "temp": 15.9, "precip_prob": 0}], "warnings": [], "fetched_at": "2026-11-19T09:02:00+00:00"}'::jsonb,
                           '{"failed_steps": [], "alerts": [], "gpu_temp_c": 60}'::jsonb, 'ollama qwen3:8b', now());
                   INSERT INTO lifeui.news_items (briefing_id, position, url, title, source, tags, importance, focus_relevance, summary)
                   SELECT id, 0, 'https://example.com/gemini', 'Gemini 4 Neon', 'Google DeepMind', '["release"]'::jsonb, 9, 3, 'A new model.' FROM lifeui.news_briefings;
                   INSERT INTO lifeui.news_items (briefing_id, position, url, title, source, tags, importance, focus_relevance, summary)
                   SELECT id, 1, 'https://example.com/agentkit', 'agentkit v1.4.0', 'Example Org', '["release", "focus_relevant"]'::jsonb, 5, 9, 'A release.' FROM lifeui.news_briefings;""")
        b.js("$t('button.tab', 'Timeline').click()")
        b.js("$t('button.tab', 'News').click()")
        check(b.wait("document.querySelectorAll('.news-item').length === 2 && document.body.textContent.includes('Faro: 13')"), "the briefing shows the weather and both items")
        b.js("$t('button.seg', 'Focus').click()")
        check(b.wait("document.querySelectorAll('.news-item').length === 1 && document.querySelector('.news-title').textContent === 'agentkit v1.4.0'"), "the Focus filter keeps only focus items")
        b.js("$t('button.seg', 'All').click()")
        b.js("$t('button', 'Save').click()")
        check(b.wait("document.body.textContent.includes('Saved ✓')"), "saving an item shows it as saved")
        check(pg.psql("SELECT count(*) FROM lifeui.news_items WHERE saved_at IS NOT NULL").stdout.strip() == "1", "the save is stored")
        check(b.js("[...document.querySelectorAll('.news-title')].every(a => a.target === '_blank' && a.rel.includes('noopener'))") is True, "item links open in a new tab with rel=noopener")

        # --- the bottom sheet: modal dialog, Esc, backdrop tap, and the drag gesture (phone-sized viewport)
        def sheet_state():
            return json.loads(b.js("JSON.stringify({open: document.getElementById('sheet').open, state: document.getElementById('sheet').dataset.state || '', form: !!document.querySelector('#sheet form')})"))

        def mouse(kind, x, y):
            b.send("Input.dispatchMouseEvent", type=kind, x=x, y=y, button="left", buttons=0 if kind == "mouseReleased" else 1, clickCount=1)

        def grab_point():
            r = json.loads(b.js("JSON.stringify(document.querySelector('.sheet-grab').getBoundingClientRect())"))
            return r["left"] + r["width"] / 2, r["top"] + r["height"] / 2

        def open_sheet():
            b.js("$t('button.fab').click()")
            ok = b.wait("document.getElementById('sheet').open && document.getElementById('sheet').dataset.state === 'open'")
            time.sleep(0.5)  # the slide-in takes 320 ms: measure and press only once it has settled
            return ok

        def sheet_closed():
            return b.wait("!document.getElementById('sheet').open", 5)

        b.js("$t('button.tab', 'Timeline').click()")
        b.send("Emulation.setDeviceMetricsOverride", width=390, height=844, deviceScaleFactor=2, mobile=True)
        check(open_sheet(), "the sheet opens as a modal dialog")
        check(b.js("document.getElementById('sheet').matches(':modal')") is True, "the sheet is truly modal (browser focus trap and inert background)")
        b.send("Input.dispatchKeyEvent", type="keyDown", key="Escape", code="Escape", windowsVirtualKeyCode=27)
        b.send("Input.dispatchKeyEvent", type="keyUp", key="Escape", code="Escape", windowsVirtualKeyCode=27)
        check(sheet_closed(), "Escape closes the sheet (with its closing animation)")
        check(b.wait("!document.querySelector('#sheet form')", 5), "the form is removed after the sheet has closed")

        open_sheet()
        mouse("mousePressed", 195, 40)
        mouse("mouseReleased", 195, 40)
        check(sheet_closed(), "tapping the dimmed backdrop closes the sheet")

        open_sheet()
        x, y = grab_point()
        mouse("mousePressed", x, y)
        for step in range(1, 5):  # a short, slow pull: well under 40% of the height and slower than a flick
            mouse("mouseMoved", x, y + step * 8)
            time.sleep(0.12)
        moved = b.js("document.getElementById('sheet').style.transform")
        mouse("mouseReleased", x, y + 32)
        time.sleep(0.5)
        st = sheet_state()
        check("translateY" in moved, "while dragging, the sheet follows the finger (inline transform)", moved)
        check(st["open"] and st["state"] == "open" and b.js("document.getElementById('sheet').style.transform") == "", "a short slow drag snaps back and the sheet stays open", str(st))

        mouse("mousePressed", x, y)
        height = b.js("document.getElementById('sheet').offsetHeight")
        for step in range(1, 9):  # a long pull past 40% of the height, slowly
            mouse("mouseMoved", x, y + step * height * 0.06)
            time.sleep(0.12)
        mouse("mouseReleased", x, y + height * 0.5)
        check(sheet_closed(), "dragging the sheet down past 40% of its height dismisses it")

        open_sheet()
        x, y = grab_point()
        mouse("mousePressed", x, y)
        height = b.js("document.getElementById('sheet').offsetHeight")
        step_px = min(60, height * 0.12)  # 3 steps stay well under the 40% distance threshold, so only velocity can dismiss
        for step in range(1, 4):  # a quick flick: short distance, high velocity (even if each event takes ~50 ms to arrive)
            mouse("mouseMoved", x, y + step * step_px)
        mouse("mouseReleased", x, y + 3 * step_px)
        check(sheet_closed(), "a quick flick dismisses the sheet even over a short distance")
        b.send("Emulation.clearDeviceMetricsOverride")

        # --- Your knowledge base (data-private/owner.ttl + data-private/import/*.ttl), if you have one: import it into the
        # throwaway database, then explore it in the app
        if not (IK.DATA_DIR / "owner.ttl").exists():
            print("SKIP knowledge base checks: no knowledge base")
        else:
            ont, graph, _files = IK.load_graphs()
            plan = IK.build_plan(ont, graph)
            IK.apply_plan(plan, conn_from_url(pg.url))
            by_class = Counter(IK.local(e["class_iri"]) for e in plan.entities)

            # --- Home: to-dos and focus topics come from the imported knowledge base; ticks persist on the device
            b.js("$t('button.tab', 'Home').click()")
            check(b.wait(f"document.querySelectorAll('.todo').length === {by_class['Task']}"), "Home lists the imported tasks as to-dos")
            check(b.wait("document.body.textContent.includes('To look into: Focus')"), "Home shows the focus card (web/src/config.rs)")
            first = b.js("document.querySelector('.todo .check').getAttribute('aria-label')")
            b.js("document.querySelector('.todo .check').click()")
            check(b.wait("[...document.querySelectorAll('.todo.finished .check')].some(e => e.getAttribute('aria-label') === " + json.dumps(first) + ")"), "ticking a to-do moves it to the finished ones")
            b.goto(base)
            b.js(HELPERS)
            b.wait("document.querySelector('.tabs')")
            b.js("$t('button.tab', 'Home').click()")
            b.wait("document.querySelector('.todo')")
            check(b.wait("[...document.querySelectorAll('.todo.finished .check')].some(e => e.getAttribute('aria-label') === " + json.dumps(first) + ")"), "the tick survives a page reload (stored on the device)")
            # --- Agent tab: give the agent a task, mark it done, read the feed and the run log
            b.js("$t('button.tab', 'Agent').click()")
            check(b.wait("document.body.textContent.includes('Your inbox is empty')"), "Agent tab opens on an empty inbox")
            check(b.js("document.querySelector('button.fab') === null"), "the add button is hidden on the Agent tab (the form is inline)")
            b.js("(() => { $fill($field('Tell the agent'), 'Add dentist on Friday at 15:00'); $submit(); })()")
            check(b.wait("[...document.querySelectorAll('.agent .card .msg')].some(e => e.textContent === 'Add dentist on Friday at 15:00')"), "a task sent to the agent appears in the inbox")
            check(b.wait("document.querySelector('.agent textarea').value === ''"), "the compose box is cleared after sending")
            b.js("$t('button', 'Mark done').click()")
            check(b.wait("document.querySelector('.agent .card.finished') && $t('button', 'Reopen')"), "marking a task done moves it to finished (and offers Reopen)")
            b.js("$t('button.seg', 'Feed').click()")
            check(b.wait("document.body.textContent.includes('Nothing from the agent yet')"), "the feed starts empty")
            b.js("(() => { $fill($field('Title'), 'Morning briefing'); $fill($field('Message'), 'Today: 2 events'); $t('.testbox button', 'Add to feed').click(); })()")
            check(b.wait("[...document.querySelectorAll('.agent .card strong')].some(e => e.textContent === 'Morning briefing')"), "a test message shows up in the feed")
            b.js("$t('button.seg', 'Runs').click()")
            check(b.wait("document.body.textContent.includes('No scheduled runs yet')"), "the run log starts empty")
            b.js("$t('button.seg', 'Settings').click()")
            check(b.wait("document.body.textContent.includes('No runner tokens yet')"), "Settings starts without runner tokens")
            check(b.js("document.querySelector('.switch').getAttribute('aria-checked')") == "false", "the agent kill switch starts OFF")
            b.js("document.querySelector('.switch').click()")
            check(b.wait("document.querySelector('.switch').getAttribute('aria-checked') === 'true' && document.body.textContent.includes('ON: the scheduled runs may work')"), "the switch turns the agent ON")
            b.js("(() => { $fill($field('Name for the new token'), 'my pc'); $submit(); })()")
            check(b.wait("document.querySelector('.token-once code') && document.querySelector('.token-once code').textContent.startsWith('lua_')"), "a new runner token is shown once")
            check(b.wait("[...document.querySelectorAll('.agent .card strong')].some(e => e.textContent === 'my pc')"), "the token is listed by its name")
            b.js("$t('button', 'Revoke').click()")
            check(b.wait("[...document.querySelectorAll('.badge')].some(e => e.textContent === 'revoked') && !$t('button', 'Revoke')"), "revoking marks the token revoked and removes the button")
            b.js("$t('button.seg', 'Feed').click()")
            b.wait("document.querySelector('.agent')")
            check(b.js("document.querySelector('.token-once') === null"), "the token is gone from the screen after leaving Settings")
            b.js("document.querySelector('.switch') && document.querySelector('.switch').click()")
            b.js("$t('button.seg', 'Inbox').click()")
            check(b.wait("document.querySelector('.agent .card.finished')"), "switching back to the inbox shows the saved task")

            b.js("$t('button.tab', 'Explore').click()")
            check(b.wait(f"document.querySelectorAll('.card.pick').length === {min(100, len(plan.entities))}"), "Explore lists the first 100 imported entities")
            check(b.wait(f"[...document.querySelectorAll('option')].some(o => o.textContent === 'Document ({by_class['Document']})')"), "kind filter shows class counts from the database")
            b.js("(() => { const s = document.querySelector('section select'); $fill(s, 'Course'); })()")
            check(b.wait(f"document.querySelectorAll('.card.pick').length === {by_class['Course']}"), f"filtering by Course shows exactly the {by_class['Course']} courses")
            term = next(e["name"] for e in plan.entities if len(e["name"]) >= 4)[:4]
            b.js(f"(() => {{ const s = document.querySelector('section select'); $fill(s, ''); $fill(document.querySelector('input[type=search]'), {json.dumps(term.upper())}); }})()")
            check(b.wait(f"document.querySelectorAll('.card.pick').length > 0 && [...document.querySelectorAll('.card.pick strong')].every(e => e.textContent.toLowerCase().includes({json.dumps(term.lower())}))"),
                  "search matches names case-insensitively")
            # open the entity with the most links to OTHER entities (found from the plan), by searching its exact name
            entity_iris = {e["iri"] for e in plan.entities}
            names = Counter(e["name"] for e in plan.entities)
            linked = Counter(s for s, _p, o in plan.links if s in entity_iris and o in entity_iris)
            richest = max((e for e in plan.entities if names[e["name"]] == 1 and linked[e["iri"]] > 0), key=lambda e: linked[e["iri"]])
            b.js(f"$fill(document.querySelector('input[type=search]'), {json.dumps(richest['name'])})")
            check(b.wait(f"document.querySelectorAll('.card.pick').length >= 1 && [...document.querySelectorAll('.card.pick strong')].some(e => e.textContent === {json.dumps(richest['name'])})"), "searching a full name finds that entity")
            b.js(f"[...document.querySelectorAll('.card.pick')].find(c => c.querySelector('strong').textContent === {json.dumps(richest['name'])}).click()")
            check(b.wait("document.querySelector('.card.detail dl') && document.querySelector('.card.detail dl').children.length > 0"), "an entity opens with its properties")
            before_name = b.js("document.querySelector('.card.detail strong').textContent")
            check(b.wait("document.querySelector('.links a')"), "the entity shows relations to other entities")
            b.js("document.querySelector('.links a').click()")
            check(b.wait(f"document.querySelector('.card.detail strong') && document.querySelector('.card.detail strong').textContent !== {json.dumps(before_name)}"), "clicking a relation opens the related entity")
            b.js("$t('button', 'Back to the list').click()")
            check(b.wait("document.querySelector('input[type=search]')"), "back returns to the list")

            # API input handling (through the same fetch glue the app uses)
            def api_status(path):
                return json.loads(b.js(f"lifeui.api_get({json.dumps(path)})"))["status"]
            def api_count(path):
                return json.loads(b.js(f"lifeui.api_get({json.dumps(path)})"))["data"].get("count")
            check(api_status("/api/entities?class=a%3Bb") == 400, "a malformed class name is refused (400)")
            check(api_status("/api/entities?limit=0") == 400, "an out-of-range limit is refused (400)")
            check(api_status("/api/entities?iri=https://evil.example/x") == 400, "an iri outside this knowledge base is refused (400)")
            if plan.events:
                check(api_status("/api/entities?iri=" + urllib.parse.quote(plan.events[0]["iri"], safe=":/")) == 404, "an event is not an entity (404)")
            check(api_count("/api/entities?q=%25%25") == 0, "'%%' matches nothing: wildcards in the search are taken literally")
            check(api_count("/api/entities?q=%25") == sum(1 for e in plan.entities if "%" in e["name"]), "a single '%' finds exactly the names that contain one")
            check(api_count("/api/entities?q=%27%3B%20DROP%20TABLE%20lifeui.entities%3B--") == 0 and api_count("/api/entities?limit=1") == 1, "an SQL-looking search is just text")

            # imported events show up on the timeline; critical information is in the knowledge base
            b.js("$t('button.tab', 'Timeline').click()")
            if plan.events:
                check(b.wait(f"document.body.textContent.includes({json.dumps(plan.events[0]['name'])})"), "imported events appear on the timeline")
            critical = [e for e in plan.entities if IK.local(e["class_iri"]) == "CriticalInformation"]
            if critical:
                check(api_count("/api/entities?q=" + urllib.parse.quote(critical[0]["name"])) >= 1, "imported critical information is in the knowledge base (Explore)")

        # --- optional screenshots at iPhone size (set UI_SCREENSHOT_DIR) for a visual check
        shots = os.environ.get("UI_SCREENSHOT_DIR")
        if shots:
            import base64
            Path(shots).mkdir(parents=True, exist_ok=True)

            def snap(name):
                time.sleep(0.9)  # let entrance animations finish
                (Path(shots) / f"{name}.png").write_bytes(base64.b64decode(b.send("Page.captureScreenshot", format="png")["data"]))

            def escape():
                b.send("Input.dispatchKeyEvent", type="keyDown", key="Escape", code="Escape", windowsVirtualKeyCode=27)
                b.send("Input.dispatchKeyEvent", type="keyUp", key="Escape", code="Escape", windowsVirtualKeyCode=27)
                time.sleep(0.5)

            for scheme in ("dark", "light"):
                b.send("Emulation.setEmulatedMedia", features=[{"name": "prefers-color-scheme", "value": scheme}])
                for device, (w, h, mobile) in {"phone": (390, 844, True), "desktop": (1100, 760, False)}.items():
                    b.send("Emulation.setDeviceMetricsOverride", width=w, height=h, deviceScaleFactor=2 if mobile else 1, mobile=mobile)
                    prefix = f"{scheme}-{device}"
                    for tab, label in (("Home", "home"), ("Agent", "agent"), ("Timeline", "timeline"), ("Explore", "explore"), ("News", "news")):
                        b.js(f"$t('button.tab', {json.dumps(tab)}).click()")
                        snap(f"{prefix}-{label}")
                    b.js("$t('button.tab', 'Explore').click()")
                    b.wait("document.querySelector('.card.pick')")
                    b.js("document.querySelector('.card.pick').click()")
                    b.wait("document.querySelector('.card.detail')")
                    snap(f"{prefix}-entity")
                    b.js("$t('button', 'Back to the list').click()")
                    b.js("$t('button.tab', 'Timeline').click()")
                    b.wait("document.querySelector('.card .row')")
                    b.js("document.querySelector('.card .row').click()")  # expanded card
                    snap(f"{prefix}-timeline-open")
                    b.js("$t('button.fab').click()")
                    snap(f"{prefix}-event-sheet")
                    escape()
            b.send("Emulation.setEmulatedMedia", features=[])
            b.send("Emulation.clearDeviceMetricsOverride")

        # --- service worker must not pin an old deployment: change the served page and expect the browser to see it
        b.goto(base)
        b.js(HELPERS)
        check(b.wait("navigator.serviceWorker && navigator.serviceWorker.controller", 20), "the service worker is controlling the page")
        index = S.DIST / "index.html"
        original = index.read_bytes()
        marker = f"<!-- deployment-marker-{int(time.time())} -->"
        try:
            index.write_bytes(original + marker.encode())
            seen = b.js(f"fetch('/', {{credentials: 'same-origin'}}).then(r => r.text()).then(t => t.includes({json.dumps(marker)}))")
            check(seen is True, "a new deployment of the page is visible without clearing the cache (network-first)")
            index.write_bytes(original)
            hashed = b.js("[...document.querySelectorAll('script[type=module], link[rel=modulepreload]')].map(e => e.src || e.href).concat(performance.getEntriesByType('resource').map(r => r.name)).filter(u => /web-[0-9a-f]+/.test(u))[0] || ''")
            check(bool(hashed), "the hashed app bundle was loaded", hashed)
        finally:
            index.write_bytes(original)

        # --- reload keeps the session; diagnostics (in Agent settings); logout
        b.goto(base)
        b.js(HELPERS)
        check(b.wait("document.querySelector('.tabs')"), "after a page reload the session cookie keeps you signed in")
        b.js("$t('button.tab', 'Agent').click()")
        b.js("$t('button.seg', 'Settings').click()")
        b.wait("document.querySelector('details.diagnostics')")
        b.js("document.querySelector('details.diagnostics').open = true")
        check(b.wait("document.body.textContent.includes('Ping database')"), "Agent settings keep the Phase 1 diagnostics")
        b.js("document.querySelector('.logout').click()")
        check(b.wait("document.querySelectorAll('input[type=password]').length === 1"), "logout returns to the login form")
        b.js(HELPERS)
        b.js(f"(() => {{ $fill(document.querySelector('input[type=password]'), {json.dumps(PASSWORD)}); $submit(); }})()")
        check(b.wait("document.querySelector('.tabs')"), "logging in again works")
        check(not b.errors, "no JavaScript errors or panics in the browser console", "; ".join(b.errors)[:400])
    except Exception as e:  # noqa: BLE001
        check(False, "ui test ran to the end", repr(e)[:300])
    finally:
        if browser:
            browser.close()
        for p in procs:
            p.terminate()
        for p in procs:
            try:
                p.wait(timeout=5)
            except Exception:
                p.kill()
        pg.stop()
    failed = [n for ok, n in results if not ok]
    print(f"\n{len(results) - len(failed)} passed, {len(failed)} failed")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
