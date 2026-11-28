#!/usr/bin/env python3
"""Writes index.json, cvx.txt and cpt.txt: an invented person's vaccine record
and claims, one for each rule of the vaccines claims tell of under "Records" in
docs/data-contract.md, and invented tables in the layouts of CDC's CVX and
CPT-to-CVX files. The sources are those of ../standing/household.json.

CPT codes 00011 to 00013 and 00099 are below the first CPT code (00100), and
CVX codes 900 to 902 are none CDC assigns."""

import json
import os

HERE = os.path.dirname(os.path.abspath(__file__))
CVX = "http://hl7.org/fhir/sid/cvx"
CPT = "http://www.ama-assn.org/go/cpt"
PERSON = "p_robinaaaaaaaaaaaaaaa"


def oid(name):
    return ("r_" + name + "a" * 22)[:22]


def record(kind, name, day, display, code=None, status="active", **fields):
    out = {
        "id": oid(name),
        "kind": kind,
        "key": f"{kind}|{name}|{day}",
        "date": day,
        "display": display,
        "status": status,
        "fields": fields,
        "provenance": [
            {
                "source": {
                    "kind": "fhir",
                    "cli": "fakeprov",
                    "organization_id": "north",
                    "fhir_base": "https://north.example.test/fhir",
                },
                "method": "fhir-api",
                "fetched_at": "2025-12-02T10:00:00Z",
                "filed_at": "2025-12-04T10:00:00Z",
                "filed_by": "medrec 0.9.11",
                "raw_path": f"Robin Example/Records/north/Any/{name}.json",
            }
        ],
    }
    if code:
        system, value = code
        out["code"] = {"system": system, "code": value}
    return out


def line(code, display=None, **more):
    service = {"system": CPT, "code": code}
    if display:
        service["display"] = display
    return {"sequence": 1, "service": service, **more}


def claim(name, day, *lines, kind="professional", status="active"):
    return record(
        "claim", name, day, "Example Clinic · Example Health Plan", None, status,
        provider="Example Clinic", insurer="Example Health Plan", type=kind, category=kind,
        billable_period={"start": day, "end": day}, items=list(lines),
    )


RECORDS = [
    # The clinic's own record, and its claim of that vaccine and day.
    record("immunization", "flushot", "2024-10-01", "Sample flu vaccine", (CVX, "900"), "completed"),
    claim("clinicflu", "2024-10-01", line("00011", "Sample vaccine A")),
    # A vaccine no record has, by CDC's CPT table.
    claim("pharmacyflu", "2025-10-02", line("00011", "SAMPLE VACCINE A"), kind="pharmacy"),
    # An immune globulin, which CDC's CVX table says is no vaccine.
    claim("globulin", "2025-03-03", line("00012", "Sample globulin")),
    # A line's own CVX code, read with no table.
    claim(
        "cvxline", "2025-04-04",
        line("00099", "Sample vaccine B", codes=[
            {"system": CPT, "code": "00099"}, {"system": CVX, "code": "902"},
        ]),
    ),
    # Denied, with nothing of it paid.
    claim(
        "denied", "2025-05-05",
        line("00011", "Sample vaccine A",
             adjudication=[{"category": "Reject Reason", "amount": None, "reason": "Sample reason"}]),
    ),
    # One CPT code of two vaccines; the test marks it private.
    claim("twovaccines", "2025-06-06", line("00013", "Sample vaccines A and B")),
    # An open claim whose vaccine line is private by its payer's note: it
    # tells of no vaccine where private.json leaves the line out.
    claim(
        "mixed", "2025-08-08",
        line("00099", "Sample vaccine B", codes=[
            {"system": CPT, "code": "00099"}, {"system": CVX, "code": "902"},
        ], payer_notes=["Psychotherapy is paid apart"]),
        {**line("00099", "Sample visit"), "sequence": 2},
    ),
    claim("cancelled", "2025-07-07", line("00011"), status="cancelled"),
]

# CDC's layouts: CVX code, short text, full name, notes, status, no-vaccine
# flag, day changed; and CPT code, its text, an empty column, the CVX text and
# code, a comment, the day changed and an id. Every text is invented.
CVX_ROWS = [
    ["900", "Sample A", "Sample vaccine A", "", "Active", "False", "2025/01/02"],
    ["901", "Sample globulin", "Sample immune globulin", "", "Active", "True", "2025/01/02"],
    ["902", "Sample B", "Sample vaccine B", "", "Active", "False", "2025/02/03"],
]
CPT_ROWS = [
    ["00011", "Sample product A", "", "Sample A", "900", "", "2025/01/02", "901"],
    ["00012", "Sample globulin", "", "Sample globulin", "901", "", "2025/01/02", "902"],
    ["00013", "Sample product A and B", "", "Sample A", "900", "", "2025/01/02", "903"],
    ["00013", "Sample product A and B", "", "Sample B", "902", "", "2025/02/03", ""],
]


def table(rows, bom):
    text = "".join("|".join(r) + "\r\n" for r in rows)
    return ("﻿" if bom else "") + text


if __name__ == "__main__":
    index = {"schema": "medrec-index/v1", "person": PERSON, "rev": 1, "documents": [], "records": RECORDS}
    with open(os.path.join(HERE, "index.json"), "w") as f:
        json.dump(index, f, indent=1, ensure_ascii=False)
        f.write("\n")
    for name, rows, bom in [("cvx.txt", CVX_ROWS, True), ("cpt.txt", CPT_ROWS, False)]:
        with open(os.path.join(HERE, name), "w", encoding="utf-8", newline="") as f:
            f.write(table(rows, bom))
