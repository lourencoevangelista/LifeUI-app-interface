#!/usr/bin/env python
"""Validate the LifeUI ontology, shapes and data.

Checks
  1. Turtle parses (ontology, shapes, data).
  2. Lint: classes/properties have labels; properties have domain and range
     (unless an inverse supplies them); every life: term used in shapes or
     data is declared in the ontology.
  3. Positive SHACL run: private data + a minimal synthetic fixture conform.
  4. Negative SHACL run: deliberately bad snippets must each produce the
     expected violation (guards against shapes that silently match nothing).

Usage:  python tools/validate_ontology.py
Exit code is non-zero on any failure.
"""
from __future__ import annotations

import sys
from pathlib import Path

import owlrl
from pyshacl import validate
from rdflib import OWL, RDF, RDFS, Graph, Namespace, URIRef
from rdflib.namespace import SH

ROOT = Path(__file__).resolve().parents[1]
ONTOLOGY = ROOT / "ontology" / "lifeui.ttl"
SHAPES = ROOT / "ontology" / "lifeui.shapes.ttl"
# Your knowledge base (data-private/, git-ignored); without one, the fictional sample in examples/data-private/.
DATA_DIR = ROOT / "data-private" if (ROOT / "data-private" / "owner.ttl").exists() else ROOT / "examples" / "data-private"
OWNER_FILE = DATA_DIR / "owner.ttl"
IMPORT_DIR = DATA_DIR / "import"  # one .ttl per document-import slice

LIFE = Namespace("https://lifeui.example/ontology#")
LIFE_NS = str(LIFE)

HEADER = """\
@prefix life: <https://lifeui.example/ontology#> .
@prefix id:   <https://lifeui.example/id/> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
@prefix xsd:  <http://www.w3.org/2001/XMLSchema#> .
id:src a life:Source ; rdfs:label "test source" ; life:hasSourceKind life:UserStated .
"""

# Common well-formed provenance block for fixtures.
PROV = "life:hasSource id:src ; life:hasSensitivity life:Personal ; life:confidence 1.0"

VALID = HEADER + f"""
id:acct a life:Account ; life:name "Checking" ; life:currency "EUR" ; life:ownedBy id:p ; {PROV} .
id:p a life:Person ; life:name "Test Person" ; {PROV} .
id:ev a life:Transaction ; life:name "Coffee" ; life:startedAt "2026-01-01T08:00:00Z"^^xsd:dateTime ;
    life:endedAt "2026-01-01T08:05:00Z"^^xsd:dateTime ; life:hasStatus life:Completed ;
    life:amount -3.5 ; life:currency "EUR" ; life:inAccount id:acct ; {PROV} .
id:ci a life:CriticalInformation ; life:name "Passport" ; life:hasCategory life:Identity ;
    life:hasSensitivity life:Secret ; life:storageRef "vault://passport"^^xsd:anyURI ;
    life:hasSource id:src ; life:confidence 1.0 .
id:emp a life:Employment ; life:name "Job" ; life:holder id:p ; life:withOrganization id:org ;
    life:startDate "2020-01-01"^^xsd:date ; life:endDate "2021-01-01"^^xsd:date ; {PROV} .
id:org a life:Organization ; life:name "Acme" ; {PROV} .
"""

# name -> (turtle body, expected sourceConstraintComponent local name, expected result path local name or None)
INVALID = {
    "confidence above 1": (
        f'id:t a life:Task ; life:name "x" ; life:hasStatus life:Planned ; life:hasSource id:src ; '
        f'life:hasSensitivity life:Personal ; life:confidence 1.5 .',
        "MaxInclusiveConstraintComponent", "confidence"),
    "event ends before it starts": (
        f'id:e a life:Meeting ; life:name "x" ; life:hasStatus life:Planned ; '
        f'life:startedAt "2026-01-02T10:00:00Z"^^xsd:dateTime ; life:endedAt "2026-01-01T10:00:00Z"^^xsd:dateTime ; {PROV} .',
        "LessThanOrEqualsConstraintComponent", "startedAt"),
    "event without any time": (
        f'id:e a life:Appointment ; life:name "x" ; life:hasStatus life:Planned ; {PROV} .',
        "OrConstraintComponent", None),
    "currency not ISO 4217": (
        f'id:a a life:Account ; life:name "A" ; life:currency "EUR" ; life:ownedBy id:p ; {PROV} .\n'
        f'id:p a life:Person ; life:name "P" ; {PROV} .\n'
        f'id:e a life:Transaction ; life:name "x" ; life:occurredOn "2026-01-01"^^xsd:date ; life:hasStatus life:Completed ; '
        f'life:amount 1 ; life:currency "euro" ; life:inAccount id:a ; {PROV} .',
        "PatternConstraintComponent", "currency"),
    "task priority out of range": (
        f'id:t a life:Task ; life:name "x" ; life:hasStatus life:Planned ; life:priority 9 ; {PROV} .',
        "MaxInclusiveConstraintComponent", "priority"),
    "entity without a name": (
        f'id:t a life:Task ; life:hasStatus life:Planned ; {PROV} .',
        "MinCountConstraintComponent", "name"),
    "period ends before it starts": (
        f'id:p a life:Person ; life:name "P" ; {PROV} .\n'
        f'id:r a life:Residency ; life:name "x" ; life:holder id:p ; life:atPlace id:pl ; '
        f'life:startDate "2021-01-01"^^xsd:date ; life:endDate "2020-01-01"^^xsd:date ; {PROV} .\n'
        f'id:pl a life:Home ; life:name "H" ; {PROV} .',
        "LessThanOrEqualsConstraintComponent", "startDate"),
    "secret critical info without storageRef": (
        f'id:c a life:CriticalInformation ; life:name "x" ; life:hasCategory life:Access ; '
        f'life:hasSensitivity life:Secret ; life:hasSource id:src .',
        "SPARQLConstraintComponent", None),
    "application without addressee": (
        f'id:p a life:Person ; life:name "P" ; {PROV} .\n'
        f'id:a a life:Application ; life:name "x" ; life:applicant id:p ; life:hasStatus life:Planned ; {PROV} .',
        "MinCountConstraintComponent", "appliesTo"),
    "specification without unit": (
        f'id:s a life:Specification ; life:name "Mass" ; life:value 480 ; {PROV} .',
        "MinCountConstraintComponent", "unit"),
    "invalid timezone": (
        f'id:p a life:Person ; life:name "P" ; life:timezone "Paris" ; {PROV} .',
        "PatternConstraintComponent", "timezone"),
    "sensitivity outside the vocabulary": (
        f'id:t a life:Task ; life:name "x" ; life:hasStatus life:Planned ; life:hasSource id:src ; '
        f'life:hasSensitivity id:made-up-level ; life:confidence 1.0 .',
        "ClassConstraintComponent", "hasSensitivity"),
    "source that is not a Source": (
        f'id:t a life:Task ; life:name "x" ; life:hasStatus life:Planned ; life:hasSource id:not-a-source ; '
        f'life:hasSensitivity life:Personal ; life:confidence 1.0 .',
        "ClassConstraintComponent", "hasSource"),
    "status not from vocabulary": (
        f'id:t a life:Task ; life:name "x" ; life:hasStatus id:made-up ; {PROV} .',
        "ClassConstraintComponent", "hasStatus"),
}


def load(*sources: Path | str, fmt: str = "turtle") -> Graph:
    g = Graph()
    for s in sources:
        if isinstance(s, Path):
            g.parse(s, format=fmt)
        else:
            g.parse(data=s, format=fmt)
    return g


def local(term) -> str:
    return str(term).rsplit("#", 1)[-1].rsplit("/", 1)[-1]


# ----------------------------------------------------------------------------- lint
def declared_terms(ont: Graph) -> set[URIRef]:
    return {s for s in ont.subjects(RDF.type, None) if isinstance(s, URIRef) and str(s).startswith(LIFE_NS)}


def lint_ontology(ont: Graph) -> list[str]:
    errors = []
    for cls in ont.subjects(RDF.type, OWL.Class):
        if isinstance(cls, URIRef) and not ont.value(cls, RDFS.label):
            errors.append(f"class {local(cls)} has no rdfs:label")
    for kind in (OWL.ObjectProperty, OWL.DatatypeProperty):
        for p in ont.subjects(RDF.type, kind):
            name = local(p)
            if not ont.value(p, RDFS.label):
                errors.append(f"property {name} has no rdfs:label")
            has_inverse = ont.value(p, OWL.inverseOf) or ont.value(None, OWL.inverseOf, p)
            if has_inverse:
                continue
            if not ont.value(p, RDFS.domain):
                errors.append(f"property {name} has no rdfs:domain")
            if not ont.value(p, RDFS.range):
                errors.append(f"property {name} has no rdfs:range")
    return errors


def undeclared_life_terms(g: Graph, declared: set[URIRef], own_nodes: set[URIRef] = frozenset()) -> list[str]:
    used = {t for triple in g for t in triple if isinstance(t, URIRef) and str(t).startswith(LIFE_NS)}
    return sorted(local(t) for t in used - declared - own_nodes)


# ----------------------------------------------------------------------------- shacl
# ----------------------------------------------------------------------------- owl consistency
ERR = Namespace("http://www.daml.org/2002/03/agents/agent-ont#")

# Data that the OWL axioms (disjoint classes) must flag. OWL-RL is not a full DL reasoner (no Java here), but it
# does detect these contradictions, including through subclasses (an Owner is a Person, so also not an Organization).
OWL_INVALID = {
    "Person that is also an Organization": "id:x a life:Person , life:Organization .",
    "Owner that is also an Organization (through subclass)": "id:w a life:Owner , life:Organization .",
    "Task that is also a Goal": "id:z a life:Task , life:Goal .",
    "Event that is also a Place": "id:y a life:Event , life:Place .",
}


def owl_inconsistencies(data: Graph, ont: Graph) -> list[str]:
    """OWL 2 RL closure of ontology + data; contradictions come back as err:ErrorMessage nodes in the graph."""
    merged = Graph()
    merged += ont
    merged += data
    owlrl.DeductiveClosure(owlrl.OWLRL_Semantics, axiomatic_triples=False, datatype_axioms=False).expand(merged)
    return [str(o) for m in merged.subjects(RDF.type, ERR.ErrorMessage) for o in merged.objects(m, ERR.error)]


def run_shacl(data: Graph, shapes: Graph, ont: Graph):
    # Merge manually: pyshacl 0.40.2 does NOT mix ont_graph into the data graph
    # when inference="none", so vocabulary individuals (life:Completed a life:Status)
    # and subclass axioms would be invisible to sh:class / sh:targetClass.
    merged = Graph()
    merged += ont
    merged += data
    conforms, report_graph, report_text = validate(
        merged, shacl_graph=shapes, inference="none",
        allow_warnings=True, allow_infos=True, abort_on_first=False, meta_shacl=False,
    )
    return conforms, report_graph, report_text


def results_of(report: Graph):
    for r in report.subjects(RDF.type, SH.ValidationResult):
        yield (
            local(report.value(r, SH.sourceConstraintComponent)),
            local(report.value(r, SH.resultPath)) if report.value(r, SH.resultPath) else None,
            local(report.value(r, SH.resultSeverity)),
            str(report.value(r, SH.resultMessage) or ""),
        )


def main() -> int:
    failures: list[str] = []

    ont = load(ONTOLOGY)
    shapes = load(SHAPES)
    print(f"parsed ontology ({len(ont)} triples) and shapes ({len(shapes)} triples)")

    # 1. lint ontology
    failures += [f"ontology lint: {e}" for e in lint_ontology(ont)]

    declared = declared_terms(ont)
    shape_nodes = {s for s in shapes.subjects(RDF.type, SH.NodeShape)}
    missing = undeclared_life_terms(shapes, declared, shape_nodes)
    failures += [f"shapes use undeclared life:{m}" for m in missing]

    # 2. positive runs
    # Usage: validate_ontology.py                 -> owner.ttl + every data-private/import/*.ttl merged
    #        validate_ontology.py --only FILE...  -> owner.ttl (as context) + each FILE, checked one by one
    # Merging matters: import files reference ids defined in owner.ttl (id:owner, your organizations, ...).
    argv = sys.argv[1:]
    positives = []
    if argv[:1] == ["--only"]:
        for name in argv[1:]:
            path = Path(name).resolve()
            positives.append((f"{path.name} (+ owner.ttl)", load(OWNER_FILE, path)))
    else:
        data_files = [p for p in [OWNER_FILE, *sorted(IMPORT_DIR.glob("*.ttl"))] if p.exists()]
        if data_files:
            positives.append((f"{DATA_DIR.relative_to(ROOT).as_posix()} ({len(data_files)} file(s) merged)", load(*data_files)))
        else:
            print("note: no private data files found, skipped")
    positives.append(("synthetic valid fixture", load(VALID)))
    for name, g in positives:
        failures += [f"{name}: undeclared life:{m}" for m in undeclared_life_terms(g, declared)]
        conforms, report, text = run_shacl(g, shapes, ont)
        soft = [r for r in results_of(report) if r[2] in ("Warning", "Info")]
        status = "OK" if conforms else "FAIL"
        n_warn = sum(1 for r in soft if r[2] == "Warning")
        print(f"[{status}] {name} ({n_warn} warning(s), {len(soft) - n_warn} info)")
        counts: dict[tuple[str, str], int] = {}
        for w in soft:
            key = (w[2].lower(), w[3] or w[0])
            counts[key] = counts.get(key, 0) + 1
        for (sev, msg), n in sorted(counts.items(), key=lambda kv: (kv[0][0] != "warning", -kv[1])):
            print(f"      {sev} x{n}: {msg}")
        if not conforms:
            failures.append(f"{name} does not conform:\n{text}")
        contradictions = owl_inconsistencies(g, ont)
        print(f"[{'OK' if not contradictions else 'FAIL'}] {name}: OWL-RL consistency ({len(contradictions)} contradiction(s))")
        failures += [f"{name}: OWL contradiction: {m[:160]}" for m in contradictions]

    # OWL negative runs: the axioms must catch real contradictions
    for name, body in OWL_INVALID.items():
        found = owl_inconsistencies(load(HEADER + body), ont)
        ok = any("Disjoint classes" in m for m in found)
        print(f"[{'OK' if ok else 'FAIL'}] OWL-RL detects: {name}")
        if not ok:
            failures.append(f"OWL negative case '{name}': no contradiction reported (got {found})")

    # 3. negative runs
    for name, (body, component, path) in INVALID.items():
        g = load(HEADER + body)
        conforms, report, _ = run_shacl(g, shapes, ont)
        hits = [r for r in results_of(report)
                if r[2] == "Violation" and r[0] == component and (path is None or r[1] == path)]
        ok = (not conforms) and bool(hits)
        print(f"[{'OK' if ok else 'FAIL'}] rejects: {name}")
        if not ok:
            seen = sorted({(r[0], r[1], r[2]) for r in results_of(report)})
            failures.append(f"negative case '{name}': expected {component}/{path}, got {seen}")

    print()
    if failures:
        print(f"{len(failures)} problem(s):")
        for f in failures:
            print(" -", f)
        return 1
    print("all checks passed")
    return 0


if __name__ == "__main__":
    sys.exit(main())
