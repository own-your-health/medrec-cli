#!/usr/bin/env python3
"""Writes index.json: an invented person's claims that print no more of a
clinician than a surname and an initial, papers that name two of them, one by its
organization and one by its title, a bill of a third's visit, from the practice
they are of, a day two clinicians share with such a bill, and a visit, a
prescription and a procedure that print the first. The sources are those of
../standing/household.json."""

import json
import os

HERE = os.path.dirname(os.path.abspath(__file__))
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
        "filed_by": "medrec 0.9.10",
        "raw_path": f"Robin Example/Records/north/ExplanationOfBenefit/{name}.json",
    }


def claim(name, day, provider):
    amounts = {
        "billed": {"amount": "40.00", "currency": "USD"},
        "you_owe": {"amount": "15.00", "currency": "USD"},
    }
    return {
        "id": oid("r_", name),
        "kind": "claim",
        "key": f"claim|{name}|{day}",
        "date": day,
        "display": f"Sample visit \u00b7 {provider}",
        "status": "active",
        "fields": {
            "provider": provider,
            "insurer": "Example Health Plan",
            "billable_period": {"start": day, "end": day},
            "type": "professional",
            "category": "visit",
            "amounts": amounts,
            "items": [
                {
                    "category": "visit",
                    "service": {"display": "Sample visit"},
                    "amounts": amounts,
                    "sequence": 1,
                }
            ],
        },
        "provenance": [provenance(name)],
    }


def printing(name, kind, day, display, field, who):
    return {
        "id": oid("r_", name),
        "kind": kind,
        "key": f"{kind}|{name}|{day}",
        "date": day,
        "display": display,
        "status": "completed" if kind != "medication" else "active",
        "fields": {field: who},
        "provenance": [provenance(name)],
    }


def paper(name, day, kind, organization, title):
    return {
        "id": oid("d_", name),
        "path": f"Robin Example/Papers/2024/{name}.pdf",
        "sha256": name,
        "md5": name,
        "size": 10,
        "media_type": "application/pdf",
        "doc_type": kind,
        "date": day,
        "organization": organization,
        "title": title,
        "filed_at": "2024-12-04T10:00:00Z",
        "origin": {"kind": "file", "action": "copied"},
        "provenance": provenance(name),
    }


INDEX = {
    "schema": "medrec-index/v1",
    "person": PERSON,
    "rev": 1,
    "records": [
        claim("byorg", "2024-09-03", "K PLACEHOLDER"),
        claim("bytitle", "2024-09-10", "EXAMPLETON, SAM"),
        claim("another", "2024-09-17", "J PLACEHOLDER"),
        claim("billed", "2024-10-01", "E SAMPLETON"),
        claim("later", "2024-10-29", "SAMPLETON, ED"),
        claim("shareda", "2024-10-15", "R SPECIMEN"),
        claim("sharedb", "2024-10-15", "T FILLER"),
        claim("specimen", "2024-11-26", "SPECIMEN, R"),
        printing("visit", "encounter", "2024-09-24", "Office visit", "practitioner", "Kim Placeholder"),
        printing("refill", "medication", "2024-09-24", "Sample tablet", "prescriber", "PLACEHOLDER, KIM"),
        printing("procedure", "procedure", "2024-09-24", "Sample procedure", "performer", "K Placeholder"),
    ],
    "studies": [],
    "imports": [],
    "documents": [
        paper("bill", "2024-10-01", "bill", "Example Counseling", "Statement"),
        paper("shared", "2024-10-15", "receipt", "Example Counseling", "Receipt"),
        paper("statement", "2024-11-05", "letter", "Kim Placeholder, LCSW", "Account letter"),
        paper("reminder", "2024-11-12", "letter", "Example Counseling", "Visit with Sam Exampleton"),
        paper("notice", "2024-11-19", "letter", "Example Pediatrics", "Visit with J Placeholder"),
    ],
}

with open(os.path.join(HERE, "index.json"), "w") as f:
    json.dump(INDEX, f, indent=1)
    f.write("\n")
