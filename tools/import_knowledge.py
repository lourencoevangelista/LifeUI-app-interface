#!/usr/bin/env python
"""Load the knowledge base (data-private/owner.ttl + data-private/import/*.ttl) into the database.

    python -I tools/import_knowledge.py plan               # parse, map, report; touches NO database
    python -I tools/import_knowledge.py apply --yes        # write to DATABASE_URL_MIGRATE from .env (the live database)
    python -I tools/import_knowledge.py selftest           # full import on a THROWAWAY local PostgreSQL, with checks

Mapping (see db/src/schema.ts):
  life:Source               -> lifeui.sources              insert if absent (by iri)
  life:Event + subclasses   -> lifeui.events               insert if absent (by iri); NEVER overwritten: the app owns them
  every other entity        -> lifeui.entities             PROJECTION of the Turtle files: rows with origin 'import' are refreshed
                                                           (life:CriticalInformation included: pointers in storageRef, never secrets)
  object properties         -> lifeui.entity_links         (subject, predicate, object) for entities and events
The refresh of entities/entity_links runs in ONE transaction together with the inserts, so a failure changes nothing.
The Turtle files stay the master copy: to change an imported entity, change the Turtle and re-run.
Secrets (the database password) are read from .env, passed to psql through the environment and never printed.
"""
from __future__ import annotations

import argparse
import glob
import json
import os
import subprocess
import sys
import tempfile
from collections import Counter
from decimal import Decimal
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from rdflib import RDF, RDFS, Graph, Literal, URIRef  # noqa: E402

from backup import CA_FILE, conn_from_url, load_env, mask, pg_env  # noqa: E402
from localpg import LocalPostgres, find_pg_bin, pg_tool  # noqa: E402

ROOT = Path(__file__).resolve().parents[1]
ONT = "https://lifeui.example/ontology#"
IDS = "https://lifeui.example/id/"
STATUSES = {"Planned", "InProgress", "Completed", "Cancelled", "Blocked"}
SENSITIVITIES = {"Public", "Personal", "Sensitive", "Secret"}
SOURCE_KINDS = {"UserStated", "Observed", "Imported", "Inferred"}
# properties stored in columns, so they are neither in props/payload nor links
COLUMN_PROPS = {"name", "confidence", "hasSensitivity", "hasStatus", "hasCategory", "hasSourceKind"}
FORBIDDEN_KEYS = {"password", "passwd", "passphrase", "secret", "token", "api_key", "apikey", "private_key", "pin", "cvv", "cvc"}


class ImportError_(Exception):
    pass


def local(term) -> str:
    """Name without namespace: life:relatesTo -> relatesTo, rdfs:label -> label."""
    return str(term).rsplit("#", 1)[-1].rsplit("/", 1)[-1]


def q(value) -> str:
    """SQL literal. Strings are single-quote escaped (standard_conforming_strings is on: backslashes are literal)."""
    if value is None:
        return "NULL"
    if isinstance(value, (int, float, Decimal)):
        return str(value)
    text = str(value)
    if "\x00" in text:
        raise ImportError_("a value contains a NUL character")
    return "'" + text.replace("'", "''") + "'"


def qjson(obj) -> str:
    return q(json.dumps(obj, ensure_ascii=False, sort_keys=True)) + "::jsonb"


def literal_value(lit: Literal):
    py = lit.toPython()
    if isinstance(py, Decimal):
        return float(py)
    if isinstance(py, bool | int | float):
        return py
    return str(lit)


# ------------------------------------------------------------------------------------------ plan
# Your knowledge base (data-private/, git-ignored); without one, the fictional sample in examples/data-private/.
DATA_DIR = ROOT / "data-private" if (ROOT / "data-private" / "owner.ttl").exists() else ROOT / "examples" / "data-private"


def load_graphs() -> tuple[Graph, Graph, list[str]]:
    ont = Graph()
    ont.parse(ROOT / "ontology" / "lifeui.ttl")
    data = Graph()
    files = [str(DATA_DIR / "owner.ttl")] + sorted(glob.glob(str(DATA_DIR / "import" / "*.ttl")))
    for f in files:
        data.parse(f, format="turtle")
    return ont, data, files


class Plan:
    def __init__(self):
        self.sources: list[dict] = []
        self.events: list[dict] = []
        self.entities: list[dict] = []
        self.links: list[tuple[str, str, str]] = []
        self.dangling: list[tuple[str, str, str]] = []
        self.problems: list[str] = []

    def counts(self) -> dict:
        return {"sources": len(self.sources), "events": len(self.events),
                "entities": len(self.entities), "links": len(self.links), "dangling_links": len(self.dangling)}


def build_plan(ont: Graph, data: Graph) -> Plan:
    plan = Plan()

    def ancestors(cls) -> set:
        seen, stack = set(), [cls]
        while stack:
            for parent in ont.objects(stack.pop(), RDFS.subClassOf):
                if isinstance(parent, URIRef) and parent not in seen:
                    seen.add(parent)
                    stack.append(parent)
        return seen

    subjects = sorted({s for s in data.subjects(RDF.type, None) if isinstance(s, URIRef) and str(s).startswith(IDS)}, key=str)
    kind_of: dict[str, str] = {}
    types_of: dict[str, list] = {}
    for s in subjects:
        types = sorted(data.objects(s, RDF.type), key=lambda t: (-len(ancestors(t)), str(t)))
        types_of[str(s)] = types
        full = set(types)
        for t in types:
            full |= ancestors(t)
        if URIRef(ONT + "Source") in full:
            kind_of[str(s)] = "source"
        elif URIRef(ONT + "Event") in full:
            kind_of[str(s)] = "event"
        else:
            kind_of[str(s)] = "entity"
    known = set(kind_of)

    for s in subjects:
        iri, kind = str(s), kind_of[str(s)]
        cls = types_of[iri][0] if types_of[iri] else None
        props = {}
        for p, o in data.predicate_objects(s):
            if p == RDF.type:
                continue
            name = local(p)
            if isinstance(o, Literal):
                if name in COLUMN_PROPS:
                    continue
                value = literal_value(o)
                if name.lower() in FORBIDDEN_KEYS:
                    plan.problems.append(f"{iri}: property '{name}' looks like a secret value; refused")
                    continue
                if name in props:
                    props[name] = (props[name] if isinstance(props[name], list) else [props[name]]) + [value]
                else:
                    props[name] = value
            elif isinstance(o, URIRef):
                if name in COLUMN_PROPS:
                    continue
                if str(o) in known:
                    plan.links.append((iri, name, str(o)))
                elif str(o).startswith(ONT):  # a controlled-vocabulary individual: keep as its name
                    props[name] = (props[name] if isinstance(props[name], list) else [props[name]]) + [local(o)] if name in props else local(o)
                elif str(o).startswith(IDS):
                    plan.links.append((iri, name, str(o)))
                    plan.dangling.append((iri, name, str(o)))
                else:  # an external IRI: keep as text
                    props[name] = str(o)
        for key, value in list(props.items()):
            if isinstance(value, list):
                props[key] = sorted(value, key=str)

        def one(prop, allowed=None, default=None):
            values = sorted(local(o) for o in data.objects(s, URIRef(ONT + prop)))
            value = values[0] if values else default
            if value is not None and allowed is not None and value not in allowed:
                plan.problems.append(f"{iri}: {prop} '{value}' is not one of {sorted(allowed)}")
            return value

        def text_of(prop):
            values = [str(o) for o in data.objects(s, URIRef(ONT + prop)) if isinstance(o, Literal)]
            return values[0] if values else None

        sources = sorted(str(o) for o in data.objects(s, URIRef(ONT + "hasSource")))
        confidence = text_of("confidence")
        name = text_of("name")
        base = {"iri": iri, "name": name, "confidence": confidence, "sensitivity": one("hasSensitivity", SENSITIVITIES, "Personal"),
                "source_iri": sources[0] if sources else None}
        for extra in sources[1:]:
            plan.links.append((iri, "hasSource", extra))
        if kind == "source":
            label = next((str(o) for o in data.objects(s, RDFS.label)), None) or name
            skind = one("hasSourceKind", SOURCE_KINDS)
            if not label or not skind:
                plan.problems.append(f"{iri}: a Source needs a label and a hasSourceKind")
                continue
            plan.sources.append({"iri": iri, "label": label, "kind": skind})
            continue
        if not name:
            plan.problems.append(f"{iri}: no life:name")
            continue
        props.pop("description", None) if kind == "event" else None
        description = text_of("description")
        if kind == "event":
            event = dict(base, class_iri=str(cls), description=description, status=one("hasStatus", STATUSES))
            for column, prop in (("occurred_at", "occurredAt"), ("occurred_on", "occurredOn"), ("started_at", "startedAt"), ("ended_at", "endedAt")):
                event[column] = text_of(prop)
                props.pop(prop, None)
            if not event["status"]:
                plan.problems.append(f"{iri}: an event needs a status")
            if not (event["occurred_at"] or event["occurred_on"] or event["started_at"]):
                plan.problems.append(f"{iri}: an event needs occurredAt, occurredOn or startedAt")
            event["payload"] = props
            plan.events.append(event)
        else:
            if cls == URIRef(ONT + "CriticalInformation") and base["sensitivity"] == "Secret" and "storageRef" not in props:
                plan.problems.append(f"{iri}: a Secret needs a storageRef pointer")
            plan.entities.append(dict(base, class_iri=str(cls), status=one("hasStatus", STATUSES), props=props))
    plan.links = sorted(set(plan.links))
    return plan


# ------------------------------------------------------------------------------------------ SQL
def source_sql(iri):
    return "NULL" if not iri else f"(SELECT id FROM lifeui.sources WHERE iri = {q(iri)})"


def build_sql(plan: Plan) -> str:
    out = ["-- generated by tools/import_knowledge.py", "SET client_min_messages = warning;"]
    for s in plan.sources:
        out.append(f"INSERT INTO lifeui.sources (iri, label, kind) VALUES ({q(s['iri'])}, {q(s['label'])}, {q(s['kind'])}) ON CONFLICT (iri) DO NOTHING;")
    for e in plan.events:
        out.append(
            "INSERT INTO lifeui.events (iri, class_iri, name, description, occurred_at, occurred_on, started_at, ended_at, status, sensitivity, source_id, confidence, payload) VALUES ("
            f"{q(e['iri'])}, {q(e['class_iri'])}, {q(e['name'])}, {q(e['description'])}, {q(e['occurred_at'])}, {q(e['occurred_on'])}, {q(e['started_at'])}, "
            f"{q(e['ended_at'])}, {q(e['status'])}, {q(e['sensitivity'])}, {source_sql(e['source_iri'])}, {q(e['confidence'])}, {qjson(e['payload'])}) ON CONFLICT (iri) DO NOTHING;")
    out.append("DELETE FROM lifeui.entity_links WHERE origin = 'import';")
    out.append("DELETE FROM lifeui.entities WHERE origin = 'import';")
    for e in plan.entities:
        out.append(
            "INSERT INTO lifeui.entities (iri, class_iri, name, status, sensitivity, source_id, confidence, props, origin) VALUES ("
            f"{q(e['iri'])}, {q(e['class_iri'])}, {q(e['name'])}, {q(e['status'])}, {q(e['sensitivity'])}, {source_sql(e['source_iri'])}, "
            f"{q(e['confidence'])}, {qjson(e['props'])}, 'import');")
    for subject, predicate, obj in plan.links:
        out.append(f"INSERT INTO lifeui.entity_links (subject_iri, predicate, object_iri, origin) VALUES ({q(subject)}, {q(predicate)}, {q(obj)}, 'import');")
    return "\n".join(out) + "\n"


def run_psql(bindir: Path, conn: dict, sql_file: Path | None = None, command: str | None = None, single_tx: bool = False) -> subprocess.CompletedProcess:
    args = [str(pg_tool(bindir, "psql")), "-h", conn["host"], "-p", conn["port"], "-U", conn["user"], "-d", conn["db"], "-v", "ON_ERROR_STOP=1", "-q"]
    if single_tx:
        args.append("--single-transaction")
    args += ["-f", str(sql_file)] if sql_file else ["-tAc", command]
    return subprocess.run(args, capture_output=True, text=True, encoding="utf-8", errors="replace", timeout=600, env={**os.environ, **pg_env(conn)})


def scalar(bindir: Path, conn: dict, sql: str) -> str:
    p = run_psql(bindir, conn, command=sql)
    if p.returncode != 0:
        raise ImportError_(f"query failed: {mask(p.stderr.strip(), conn)[:300]}")
    return p.stdout.strip()


def apply_plan(plan: Plan, conn: dict) -> dict:
    bindir = find_pg_bin()
    if not bindir:
        raise ImportError_("no installed PostgreSQL found (psql is needed)")
    before = int(scalar(bindir, conn, "SELECT count(*) FROM lifeui.events"))
    with tempfile.TemporaryDirectory() as tmp:
        sql_file = Path(tmp) / "import.sql"
        sql_file.write_bytes(build_sql(plan).encode("utf-8"))
        p = run_psql(bindir, conn, sql_file=sql_file, single_tx=True)
    if p.returncode != 0:
        raise ImportError_(f"import failed and was rolled back: {mask(p.stderr.strip(), conn)[:500]}")
    after = int(scalar(bindir, conn, "SELECT count(*) FROM lifeui.events"))
    return {"events_added": after - before, "events_already_there": len(plan.events) - (after - before)}


# ------------------------------------------------------------------------------------------ commands
def show_plan(plan: Plan, files: list[str]) -> None:
    print(f"read {len(files)} Turtle files")
    print("plan:", json.dumps(plan.counts()))
    kinds = Counter(local(e["class_iri"]) for e in plan.entities)
    print("entity classes:", dict(kinds.most_common(12)), "...")
    for problem in plan.problems:
        print("PROBLEM:", problem)
    for d in plan.dangling[:10]:
        print("DANGLING LINK:", d)


def cmd_plan(_args) -> int:
    ont, data, files = load_graphs()
    plan = build_plan(ont, data)
    show_plan(plan, files)
    return 1 if plan.problems else 0


def cmd_apply(args) -> int:
    if not args.yes:
        print("apply writes to the database named in DATABASE_URL_MIGRATE (the LIVE Supabase): add --yes to confirm", file=sys.stderr)
        return 2
    ont, data, files = load_graphs()
    plan = build_plan(ont, data)
    show_plan(plan, files)
    if plan.problems:
        print("refusing to import while there are problems", file=sys.stderr)
        return 1
    if not args.skip_validate:
        print("running the SHACL + OWL-RL validator first ...")
        v = subprocess.run([sys.executable, "-I", str(ROOT / "tools" / "validate_ontology.py")], capture_output=True, text=True, timeout=900)
        if v.returncode != 0 or "all checks passed" not in v.stdout:
            print("validator failed; nothing was imported", file=sys.stderr)
            return 1
    url = load_env().get("DATABASE_URL_MIGRATE")
    if not url:
        print("DATABASE_URL_MIGRATE is missing in .env", file=sys.stderr)
        return 2
    result = apply_plan(plan, conn_from_url(url))
    print("imported:", json.dumps(result), "| entities and links refreshed:", plan.counts()["entities"], plan.counts()["links"])
    return 0


def cmd_selftest(_args) -> int:
    results: list[tuple[bool, str]] = []

    def check(ok: bool, name: str, detail: str = "") -> None:
        results.append((ok, name))
        print(f"[{'PASS' if ok else 'FAIL'}] {name}" + (f": {detail}" if detail and not ok else ""))

    ont, data, files = load_graphs()
    plan = build_plan(ont, data)
    counts = plan.counts()
    id_subjects = {s for s in data.subjects(RDF.type, None) if isinstance(s, URIRef) and str(s).startswith(IDS)}
    check(not plan.problems, "mapping finds no problems in the real data", "; ".join(plan.problems[:3]))
    check(counts["sources"] + counts["events"] + counts["entities"] == len(id_subjects),
          "every entity in the Turtle files lands in exactly one table", f"{counts} vs {len(id_subjects)}")
    check(counts["dangling_links"] == 0, "every link points at a known entity", str(plan.dangling[:3]))

    bindir = find_pg_bin()
    if not bindir:
        check(False, "needs an installed PostgreSQL")
        return 1
    pg = LocalPostgres(bindir, ROOT / "db" / "migrations")
    try:
        pg.start()
        conn = conn_from_url(pg.url)
        first = apply_plan(plan, conn)
        db = lambda sql: scalar(bindir, conn, sql)  # noqa: E731
        check(int(db("SELECT count(*) FROM lifeui.sources")) == counts["sources"], "sources loaded")
        check(int(db("SELECT count(*) FROM lifeui.events")) == counts["events"] and first["events_added"] == counts["events"], "events loaded", str(first))
        check(int(db("SELECT count(*) FROM lifeui.entities")) == counts["entities"], "entities loaded")
        check(int(db("SELECT count(*) FROM lifeui.entity_links")) == counts["links"], "links loaded")
        check(db("SELECT count(*) FROM lifeui.entities WHERE source_id IS NULL") == "0", "every entity is linked to its source row")
        check(db("SELECT count(*) FROM lifeui.entity_links l WHERE NOT EXISTS (SELECT 1 FROM lifeui.entities e WHERE e.iri = l.subject_iri) "
                 "AND NOT EXISTS (SELECT 1 FROM lifeui.events e WHERE e.iri = l.subject_iri)") == "0", "no link starts at an unknown subject")

        # unicode survives byte for byte
        accented = next((e for e in plan.entities if any(ord(ch) > 127 for ch in e["name"])), None)
        if accented:
            check(db(f"SELECT name FROM lifeui.entities WHERE iri = {q(accented['iri'])}") == accented["name"], "accented names survive byte for byte", accented["name"])

        # types: decimals and dates come back as the right JSON types
        check(db("SELECT count(*) FROM lifeui.entities WHERE props ? 'value' AND jsonb_typeof(props->'value') <> 'number'") == "0", "numeric values are stored as JSON numbers")

        # no secrets: pointers only
        check(db("SELECT count(*) FROM lifeui.entities WHERE class_iri LIKE '%#CriticalInformation' AND props ? 'storageRef' "
                 "AND props->>'storageRef' !~ '^[a-z][a-z0-9+.-]*://'") == "0", "critical information entities hold pointers, not values")
        check(int(db("SELECT count(*) FROM lifeui.entities WHERE class_iri LIKE '%#CriticalInformation'")) ==
              sum(1 for e in plan.entities if e["class_iri"].endswith("#CriticalInformation")), "critical information lands in the knowledge base")
        check(db("SELECT count(*) FROM lifeui.entities WHERE props::text ~* '(sk-[A-Za-z0-9]{20}|password|passphrase|api[_-]?key)'") == "0", "no password or key text inside entity properties")

        # idempotency, and the rule that events are never overwritten
        db("UPDATE lifeui.events SET name = 'EDITED IN THE APP' WHERE iri = " + q(plan.events[0]["iri"]))
        db("DELETE FROM lifeui.entities WHERE iri = " + q(plan.entities[0]["iri"]))
        second = apply_plan(plan, conn)
        check(second["events_added"] == 0 and second["events_already_there"] == counts["events"], "a second import adds no events", str(second))
        check(db("SELECT name FROM lifeui.events WHERE iri = " + q(plan.events[0]["iri"])) == "EDITED IN THE APP", "an event changed in the app is NOT overwritten by the import")
        check(int(db("SELECT count(*) FROM lifeui.entities")) == counts["entities"] and int(db("SELECT count(*) FROM lifeui.entity_links")) == counts["links"],
              "entities and links are refreshed to exactly the Turtle content (deleted row restored, no duplicates)")

        # rollback: a bad row must leave the database as it was
        broken = build_plan(ont, data)
        broken.entities[0]["sensitivity"] = "NotAValue"
        entities_before = db("SELECT count(*) FROM lifeui.entities")
        try:
            apply_plan(broken, conn)
            check(False, "a bad row aborts the import")
        except ImportError_:
            check(db("SELECT count(*) FROM lifeui.entities") == entities_before, "a failing import rolls back completely (entities untouched)")

        # lock-down
        check(db("SELECT count(*) FROM pg_tables WHERE schemaname = 'lifeui' AND tablename IN ('entities', 'entity_links') AND rowsecurity") == "2", "row-level security is on for the new tables")
        check(db("SELECT has_table_privilege('anon', 'lifeui.entities', 'SELECT') OR has_table_privilege('anon', 'lifeui.entity_links', 'SELECT')") == "f", "the Supabase anon role cannot read the new tables")
    except Exception as error:  # noqa: BLE001
        check(False, "selftest ran to the end", repr(error)[:300])
    finally:
        pg.stop()
    failed = [n for ok, n in results if not ok]
    print(f"\n{len(results) - len(failed)} passed, {len(failed)} failed")
    return 1 if failed else 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = parser.add_subparsers(dest="command", required=True)
    sub.add_parser("plan")
    a = sub.add_parser("apply")
    a.add_argument("--yes", action="store_true")
    a.add_argument("--skip-validate", action="store_true")
    sub.add_parser("selftest")
    args = parser.parse_args()
    try:
        return {"plan": cmd_plan, "apply": cmd_apply, "selftest": cmd_selftest}[args.command](args)
    except ImportError_ as error:
        print(f"ERROR: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    sys.exit(main())
