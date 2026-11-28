#!/usr/bin/env python3
"""Writes index.json and text.json: an invented person's papers whose texts
label follow-ups and drugs, as filed by a medrec that did not read them, one
for each rule of "Fields of a paper" in docs/data-contract.md, and a medicine
of theirs one drug prints the NDC of. The sources are those of
../standing/household.json."""

import json
import os

HERE = os.path.dirname(os.path.abspath(__file__))
NDC = "http://hl7.org/fhir/sid/ndc"
PERSON = "p_robinaaaaaaaaaaaaaaa"


def oid(prefix, name):
    return (prefix + name + "a" * 22)[:22]


def provenance(name, kind="DocumentReference"):
    return {
        "source": {
            "kind": "fhir",
            "cli": "fakeprov",
            "organization_id": "north",
            "fhir_base": "https://north.example.test/fhir",
        },
        "method": "fhir-api",
        "fetched_at": "2025-06-02T10:00:00Z",
        "filed_at": "2025-06-04T10:00:00Z",
        "filed_by": "medrec 0.9.11",
        "raw_path": f"Robin Example/Records/north/{kind}/{name}.json",
    }


def paper(name, day, kind, title=None, organization="Example Family Clinic"):
    d = {
        "id": oid("d_", name),
        "path": f"Robin Example/Papers/2025/{name}.pdf",
        "sha256": name,
        "md5": name,
        "size": 10,
        "media_type": "application/pdf",
        "doc_type": kind,
        "date": day,
        "organization": organization,
        "text": {"chars": 10},
        "filed_at": "2025-06-04T10:00:00Z",
        "origin": {"kind": "file", "action": "copied"},
        "provenance": provenance(name),
    }
    if title:
        d["title"] = title
    return d


PAPERS = [
    # A follow-up after the paper's date with a clinician, and a drug with
    # its directions and the NDC of the person's medicine.
    (
        paper("visit", "2025-03-10", "visit_summary"),
        "Visit summary\nExample Family Clinic\nFollow up in 2 weeks with Dr. Pat Sample.\n"
        "Rx: Exampleprofen 200 mg tablet\nSig: 1 tablet by mouth twice daily\nNDC 00000-0001-01\n",
    ),
    # A day with no year, before a condition; and words with no day.
    (
        paper("discharge", "2025-04-02", "discharge_summary"),
        "Discharge instructions\nReturn on Apr 20, sooner if fever.\nFollow up as needed.\n",
    ),
    # An open paper: a follow-up at a place of a private area, one for a
    # diagnosis code of one, and a drug of one by its brand, each private by
    # itself; and a follow-up that says none.
    (
        paper("counsel", "2025-05-05", "visit_summary"),
        "Visit summary\nFollow up at Placeholder Counseling Center in 1 month\n"
        "Follow up for F41.1 in 6 weeks\nRx: Camila 0.35 mg tablet\nSig: 1 tablet daily\n"
        "Follow up in 3 months\n",
    ),
    # A private paper: its follow-up is private by it.
    (
        paper("private", "2025-05-12", "visit_summary", title="Psychiatry visit"),
        "Psychiatry visit\nFollow up in 2 weeks\n",
    ),
    # A line that says an area is never read.
    (
        paper("held", "2025-05-20", "visit_summary"),
        "Visit summary\nFollow up in 2 weeks for HIV test results\n",
    ),
    # A bill is not read.
    (
        paper("bill", "2025-05-25", "bill"),
        "Statement\nReturn this portion with your payment\nFollow up in 2 weeks\n",
    ),
]

RECORDS = [
    {
        "id": oid("r_", "exampleprofen"),
        "kind": "medication",
        "key": "medication|exampleprofen",
        "date": "2025-03-10",
        "display": "Exampleprofen 200 mg tablet",
        "status": "active",
        "code": {"system": NDC, "code": "00000-0001-01", "display": "Exampleprofen 200 mg tablet"},
        "fields": {},
        "provenance": [provenance("exampleprofen", "MedicationRequest")],
    }
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
