#!/usr/bin/env python3
"""Writes index.json: an invented person's records, one for each of the rules by
a record's own words and codes under "Private" in docs/data-contract.md, each
beside one that says nothing. The sources are those of
../standing/household.json."""

import json
import os

HERE = os.path.dirname(os.path.abspath(__file__))
ICD10 = "http://hl7.org/fhir/sid/icd-10-cm"
SNOMED = "http://snomed.info/sct"
PERSON = "p_robinaaaaaaaaaaaaaaa"


def oid(prefix, name):
    return (prefix + name + "a" * 22)[:22]


def record(kind, name, day, display, code=None, **fields):
    out = {
        "id": oid("r_", name),
        "kind": kind,
        "key": f"{kind}|{name}|{day}",
        "date": day,
        "display": display,
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
                "fetched_at": "2024-12-02T10:00:00Z",
                "filed_at": "2024-12-04T10:00:00Z",
                "filed_by": "medrec 0.9.10",
                "raw_path": f"Robin Example/Records/north/Any/{name}.json",
            }
        ],
    }
    if code:
        out["code"] = code
    if kind in ("condition", "medication"):
        out["status"] = "active"
    return out


def fill(name, day, medicine):
    amounts = {"billed": {"amount": "9.00", "currency": "USD"}}
    return record(
        "claim",
        name,
        day,
        f"{medicine} \u00b7 Example Pharmacy",
        provider="Example Pharmacy",
        insurer="Example Health Plan",
        billable_period={"start": day, "end": day},
        type="pharmacy",
        category="pharmacy",
        amounts=amounts,
    )


RECORDS = [
    record("encounter", "reasoned", "2024-03-05", "Sample visit", reason="Anxiety follow up"),
    record("encounter", "namedvisit", "2024-03-12", "Behavioral health hour"),
    record("encounter", "earvisit", "2024-03-19", "Sample visit", reason="Ear pain"),
    record("condition", "uncoded", "2024-04-02", "Generalized anxiety"),
    record(
        "condition",
        "othercode",
        "2024-04-03",
        "Sample habit",
        {"system": SNOMED, "code": "1", "display": "Tobacco use"},
    ),
    record(
        "condition",
        "family",
        "2024-04-04",
        "Sample history",
        {"system": ICD10, "code": "Z81.1", "display": "Sample history"},
    ),
    record("condition", "heart", "2024-04-05", "ST segment depression"),
    record("medication", "brand", "2024-05-06", "Zoloft 50 MG"),
    record("medication", "nose", "2024-05-07", "Loratadine 10 MG"),
    fill("brandfill", "2024-05-08", "Lexapro 10 MG"),
    fill("nosefill", "2024-05-09", "Loratadine 10 MG"),
    record("report", "told", "2024-06-03", "Example report", conclusion="HIV antibody not seen"),
    record("report", "eartold", "2024-06-04", "Example ear report", conclusion="Clear"),
    record("result", "textvalue", "2024-06-05", "Example smear", value="Chlamydia not seen"),
    record("result", "earvalue", "2024-06-06", "Example ear swab", value="Not seen"),
]

INDEX = {
    "schema": "medrec-index/v1",
    "person": PERSON,
    "rev": 1,
    "records": RECORDS,
    "studies": [],
    "imports": [],
    "documents": [],
}

with open(os.path.join(HERE, "index.json"), "w") as f:
    json.dump(INDEX, f, indent=1)
    f.write("\n")
