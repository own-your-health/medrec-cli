#!/usr/bin/env python3
"""Writes index.json: an invented person's results, one for each kind of
questionnaire of "Private" in docs/data-contract.md and one that is none, a
reading that only the code of one of its parts says is of one, an answer that
only the panel that lists it says is of one, panels of private results and of
private and other ones,
conditions with a code for tobacco use and one without, a report of one with its
paper, a paper named as one, a visit and claims of some
of those days, and two pharmacy claims. The sources are those of
../standing/household.json."""

import json
import os

HERE = os.path.dirname(os.path.abspath(__file__))
LOINC = "http://loinc.org"


def oid(prefix, name):
    return (prefix + name + "a" * 22)[:22]


def provenance(name):
    return {
        "source": {
            "kind": "fhir",
            "cli": "fakeprov",
            "organization_id": "north",
            "fhir_base": "https://north.example.test/fhir",
        },
        "method": "fhir-api",
        "fetched_at": "2024-07-02T10:00:00Z",
        "filed_at": "2024-07-04T10:00:00Z",
        "filed_by": "medrec 0.9.9",
        "raw_path": f"Robin Example/Records/north/Observation/{name}.json",
    }


def record(kind, name, day, display, code=None, documents=(), **fields):
    r = {
        "id": oid("r_", name),
        "kind": kind,
        "key": f"{kind}|{name}|{day}",
        "date": day,
        "display": display,
        "status": "final",
        "fields": fields,
        "provenance": [provenance(name)],
    }
    if code:
        r["code"] = {"system": LOINC, "code": code}
    if documents:
        r["documents"] = list(documents)
    return r


def condition(name, day, code):
    """A diagnosis with a name that says nothing of what its code does."""
    r = record("condition", name, day, f"Example condition, {name}")
    r["status"] = "active"
    r["code"] = {"system": "http://hl7.org/fhir/sid/icd-10-cm", "code": code}
    return r


def visit(name, day):
    r = record("encounter", name, day, "Office visit", service_provider="Example Health")
    r["status"] = "finished"
    return r


def claim(name, day, what="Office visit", kind="professional", **links):
    """A claim, with the results of its day as an import links them."""
    money = {"amount": "40.00", "currency": "USD"}
    category = "pharmacy" if kind == "pharmacy" else "visit"
    place = "Example Corner Pharmacy" if kind == "pharmacy" else "Example Hearing Room"
    line = {"sequence": 1, "category": category, "amounts": {"billed": money}}
    if "results" in links:
        line["records"] = links["results"]
    r = record(
        "claim",
        name,
        day,
        f"{what} · {place}",
        provider=place,
        insurer="Example Health Plan",
        billable_period={"start": day, "end": day},
        type=kind,
        amounts={"billed": money, "you_owe": money},
        items=[line],
        category=category,
        **links,
    )
    r["status"] = "active"
    return r


def paper(name, day, title):
    return {
        "id": oid("d_", name),
        "path": f"Robin Example/Visits/2024/{name}.pdf",
        "sha256": name,
        "md5": name,
        "size": 10,
        "media_type": "application/pdf",
        "doc_type": "other",
        "date": day,
        "organization": "Example Health",
        "title": title,
        "filed_at": "2024-07-04T10:00:00Z",
        "origin": {"kind": "fhir", "action": "imported"},
        "provenance": provenance(name),
    }


RESULTS = [
    ("lowmood", "2024-03-04", "Patient health questionnaire, total", None),
    ("coded", "2024-03-11", "Total score", "44261-6"),
    ("worry", "2024-03-18", "GAD-7 total", None),
    ("newparent", "2024-03-25", "Postnatal depression screening", None),
    ("safety", "2024-04-08", "Suicide risk", None),
    ("nerves", "2024-04-15", "Anxiety screening", None),
    ("checklist", "2024-04-22", "PTSD checklist", None),
    ("drinks", "2024-05-06", "Alcohol use", None),
    ("drugs", "2024-05-13", "DAST-10", None),
    ("smoke", "2024-05-27", "Tobacco smoking status", None),
    ("sunlight", "2024-05-20", "Example sunlight index", None),
]


def part(display, code, value):
    return {"code": {"system": LOINC, "code": code, "display": display}, "value": value}

with open(os.path.join(HERE, "index.json"), "w") as f:
    json.dump(
        {
            "schema": "medrec-index/v1",
            "person": "p_robinaaaaaaaaaaaaaaa",
            "rev": 1,
            "studies": [],
            "imports": [],
            "records": [
                *(
                    record("result", n, day, display, code, value="4", category="Survey")
                    for n, day, display, code in RESULTS
                ),
                record(
                    "result",
                    "pieces",
                    "2024-06-24",
                    "Example check-in",
                    value="First piece 6; Second piece 2",
                    components=[
                        part("First piece", "00000-9", 6),
                        part("Second piece", "44261-6", 2),
                    ],
                    category="Survey",
                ),
                {
                    **record(
                        "result", "inner", "2024-07-01", "Second answer", value="1", category="Survey"
                    ),
                    "listed_by": [{"display": "PHQ-9 panel"}],
                },
                record(
                    "result",
                    "pair",
                    "2024-07-01",
                    "Example pair",
                    category="Survey",
                    members=[oid("r_", "inner"), oid("r_", "drinks")],
                ),
                record(
                    "result",
                    "mixpanel",
                    "2024-07-02",
                    "Example mixed panel",
                    category="Survey",
                    members=[oid("r_", "sunlight"), oid("r_", "drugs")],
                ),
                condition("leaves", "2024-05-28", "Z72.0"),
                condition("formerly", "2024-05-29", "Z87.891"),
                condition("ankle", "2024-05-30", "S93.401A"),
                visit("checkup", "2024-03-04"),
                claim("hearvisit", "2024-03-18", results=[oid("r_", "worry")]),
                claim(
                    "seenvisit",
                    "2024-05-20",
                    results=[oid("r_", n) for n in ("drugs", "sunlight")],
                ),
                claim("calmfill", "2024-07-08", "Buspirone 5 mg tablet", "pharmacy"),
                claim("nosefill", "2024-07-15", "Cetirizine 10 mg tablet", "pharmacy"),
                record(
                    "report",
                    "asked",
                    "2024-06-03",
                    "Depression screening",
                    documents=[oid("d_", "answers")],
                ),
            ],
            "documents": [
                paper("answers", "2024-06-03", "Answers"),
                paper("sheet", "2024-06-10", "Beck depression inventory"),
                paper("leaflet", "2024-06-17", "Leaflet"),
            ],
        },
        f,
        indent=1,
    )
    f.write("\n")
