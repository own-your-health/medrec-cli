#!/usr/bin/env python3
"""Writes index.json and text.json: an invented person's claims, procedures and
papers, one for each rule of "Private parts" in docs/data-contract.md and of the
services under "Private", each beside one that says nothing. The sources are
those of ../standing/household.json."""

import json
import os

HERE = os.path.dirname(os.path.abspath(__file__))
CPT = "http://www.ama-assn.org/go/cpt"
HCPCS = "https://bluebutton.cms.gov/resources/codesystem/hcpcs"
ICD10 = "http://hl7.org/fhir/sid/icd-10-cm"
PERSON = "p_robinaaaaaaaaaaaaaaa"


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
        "fetched_at": "2024-12-02T10:00:00Z",
        "filed_at": "2024-12-04T10:00:00Z",
        "filed_by": "medrec 0.9.9",
        "raw_path": f"Robin Example/Records/north/ExplanationOfBenefit/{name}.json",
    }


def record(kind, name, day, display, **fields):
    return {
        "id": oid("r_", name),
        "kind": kind,
        "key": f"{kind}|{name}|{day}",
        "date": day,
        "display": display,
        "status": "active",
        "fields": fields,
        "provenance": [provenance(name)],
    }


def money(amount):
    return {"amount": amount, "currency": "USD"}


def line(display, code=None, system=CPT, category="visit", **more):
    service = {"display": display}
    if code:
        service = {"system": system, "code": code, "display": display}
    return {
        "category": category,
        "service": service,
        "amounts": {"billed": money("20.00")},
        **more,
    }


def revenue(code, display):
    """A line a hospital bills with no service of its own."""
    return {
        "category": "facility",
        "revenue": {"code": code, "display": display},
        "amounts": {"billed": money("20.00")},
    }


def claim(name, day, title, items, provider="Example Pediatrics", category="visit", **more):
    for i, x in enumerate(items):
        x["sequence"] = i + 1
    paid = money(f"{20 * len(items)}.00")
    return record(
        "claim",
        name,
        day,
        f"{title} · {provider}",
        provider=provider,
        insurer="Example Health Plan",
        billable_period={"start": day, "end": day},
        type="professional",
        amounts={"billed": paid, "you_owe": money("10.00")},
        items=items,
        category=category,
        **more,
    )


def diagnoses(*codes):
    return [
        {"sequence": i + 1, "code": {"system": ICD10, "code": c, "display": f"Example finding {c}"}}
        for i, c in enumerate(codes)
    ]


def given(r, items):
    """What an earlier version of the claim gave."""
    r["given"] = [
        {
            "raw_path": r["provenance"][0]["raw_path"],
            "fields": {
                "items": [
                    {k: x[k] for k in ("service", "revenue", "payer_notes", "counted") if k in x}
                    for x in items
                ]
            },
        }
    ]
    return r


def of_paper(r, name):
    r["documents"] = [oid("d_", name)]
    return r


def procedure(name, day, display, code=None):
    r = record("procedure", name, day, display)
    if code:
        r["code"] = {"system": CPT, "code": code, "display": display}
    return r


def paper(name, day, kind, title, came_as=None):
    d = {
        "id": oid("d_", name),
        "path": f"Robin Example/Papers/2024/{name}.pdf",
        "sha256": name,
        "md5": name,
        "size": 10,
        "media_type": "application/pdf",
        "doc_type": kind,
        "date": day,
        "organization": "Example Pediatrics",
        "text": {"chars": 10},
        "filed_at": "2024-12-04T10:00:00Z",
        "origin": {"kind": "file", "action": "copied"},
        "provenance": provenance(name),
    }
    if title:
        d["title"] = title
    if came_as:
        d["original_name"] = came_as
    return d


OFFICE = lambda: line("Sample visit", "99213")
EARS = lambda: line("Ear wax look", "92550", category="medicine")
MOOD = lambda: line("Quick feelings form", "96127", category="medicine")

RECORDS = [
    # One private line among others, by its number; by its name; by one a
    # payer notes; by its revenue code; one the totals leave out.
    claim(
        "mixed",
        "2024-08-05",
        "Sample visit, Quick feelings form and 1 more",
        [OFFICE(), MOOD(), EARS()],
        category="mixed",
    ),
    claim(
        "checks",
        "2024-08-12",
        "Ear wax look and Depression screening",
        [EARS(), line("Depression screening", category="medicine")],
        category="medicine",
    ),
    claim(
        "noted",
        "2024-08-19",
        "Sample visit and Form",
        [OFFICE(), line("Form", "00000", payer_notes=["Psychotherapy is paid apart"])],
    ),
    claim(
        "rooms",
        "2024-08-26",
        "Facility",
        [revenue("0300", "Bench work"), revenue("0900", "Group hour")],
        category="facility",
    ),
    claim(
        "uncounted",
        "2024-09-02",
        "Sample visit",
        [OFFICE(), line("Tobacco counseling", counted=False)],
    ),
    # Another diagnosis than the first, and a note.
    claim(
        "second",
        "2024-09-09",
        "Sample visit",
        [OFFICE()],
        diagnoses=diagnoses("J02.9", "F41.1"),
        payer_notes=["Paid in full", "Behavioral health is paid apart"],
    ),
    # A line that says nothing now, and did in an earlier version.
    given(
        claim("before", "2024-09-16", "Sample visit and Form", [OFFICE(), line("Form")]),
        [OFFICE(), line("Alcohol use screening")],
    ),
    # Of one line all, so is the claim; by what it gave; and with none, by name.
    claim(
        "every",
        "2024-09-23",
        "Quick feelings form and Yearly form",
        [MOOD(), line("Yearly form", "G0444", HCPCS)],
        category="medicine",
    ),
    given(
        claim("wasevery", "2024-09-30", "Form", [line("Form")]),
        [line("Anxiety screening")],
    ),
    of_paper(claim("nolines", "2024-10-07", "Depression screening", []), "letter"),
    # A private test, alone and beside another.
    claim(
        "labbed",
        "2024-10-08",
        "Sample visit and Drug screen",
        [OFFICE(), line("Drug screen", category="lab")],
        category="lab",
    ),
    claim(
        "labboth",
        "2024-10-09",
        "Example blood count and Drug screen",
        [line("Example blood count", category="lab"), line("Drug screen", category="lab")],
        category="lab",
    ),
    # One with none of it, and a clinician whose practice a private paper tells.
    claim("plain", "2024-10-14", "Sample visit", [OFFICE()]),
    claim(
        "doctor",
        "2024-10-21",
        "Sample visit",
        [OFFICE()],
        provider="PAT PLACEHOLDER MD",
    ),
    procedure("talk", "2024-10-28", "Sample hour", "90837"),
    procedure("implant", "2024-11-04", "Contraceptive implant"),
    procedure("stitches", "2024-11-11", "Sample stitches", "12001"),
]

PAPERS = [
    (
        paper("account", "2024-09-01", "bill", "Account summary"),
        "Example Pediatrics\nSample visit 20.00\nDepression screening 20.00\n"
        "PHQ-9\nEar wax look 20.00\nTotal 60.00\fQuestions? Call us.\n"
        "Fagerstrom form\nThank you",
    ),
    (
        paper("explained", "2024-09-02", "eob", "How it was paid"),
        "Example Health Plan\n99213 Sample visit\n96127 Form\nBehavioral\n"
        "health hour\nThis is not a bill",
    ),
    (
        paper("paidslip", "2024-09-03", "receipt", "Paid"),
        "Example Pediatrics\nPaid 10.00\nThank you",
    ),
    (
        paper("told", "2024-10-22", "letter", "Psychotherapy letter"),
        "Pat Placeholder MD, a division of Sample Riverside Group.\n",
    ),
    (
        paper("letter", "2024-10-07", "letter", "Letter"),
        "Sample words\n",
    ),
    (
        paper("afterward", "2024-10-15", "visit_summary", "After your visit"),
        "Sample visit\nDepression screening done\n",
    ),
    (
        paper("unnamed", "2024-10-16", "other", None, "Psychotherapy_session-notes.pdf"),
        "Sample words\n",
    ),
]


def write(name, v):
    with open(os.path.join(HERE, name), "w") as f:
        json.dump(v, f, indent=1)
        f.write("\n")


write(
    "index.json",
    {
        "schema": "medrec-index/v1",
        "person": PERSON,
        "rev": 1,
        "studies": [],
        "imports": [],
        "records": RECORDS,
        "documents": [d for d, _ in PAPERS],
    },
)
write(
    "text.json",
    {
        "schema": "medrec-text/v1",
        "person": PERSON,
        "texts": {d["id"]: text for d, text in PAPERS},
    },
)
