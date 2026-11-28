#!/usr/bin/env python3
"""Writes index.json: an invented person's medicines, each ended by its date,
the pharmacy's fills of some, and pharmacy claims, one for each of the rules of
`filled` under "Standing" in docs/data-contract.md. The sources are those of
../standing/household.json."""

import json
import os

HERE = os.path.dirname(os.path.abspath(__file__))
RXNORM = "http://www.nlm.nih.gov/research/umls/rxnorm"
NDC = "http://hl7.org/fhir/sid/ndc"
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
                "fetched_at": "2024-12-02T10:00:00Z",
                "filed_at": "2024-12-04T10:00:00Z",
                "filed_by": "medrec 0.9.11",
                "raw_path": f"Robin Example/Records/north/Any/{name}.json",
            }
        ],
    }
    if code:
        system, value = code
        out["code"] = {"system": system, "code": value}
    return out


def medicine(name, display, code=None):
    """A prescription that ends on 2024-03-31."""
    return record(
        "medication", name, "2024-02-01", display, code,
        source="request", end="2024-03-31", end_of="prescription",
    )


def fill(name, day, display, code, **fields):
    return record("medication", name, day, display, code, "completed", source="dispense", **fields)


def owed(amount):
    return {"you_owe": {"amount": amount, "currency": "USD"}}


def claim(
    name, day, drug, *services, status="active", kind="pharmacy", uncounted=(), denied=(), owe=None
):
    items = [
        {"sequence": n, "service": {"system": system, "code": value}}
        for n, (system, value) in enumerate(services, 1)
    ]
    for n in uncounted:
        items[n]["counted"] = False
    for n, why in denied:
        items[n]["adjudication"] = [{"category": why, "amount": None, "reason": "Sample reason"}]
    if owe:
        items[0]["amounts"] = owed(owe)
    return record(
        "claim", name, day, f"{drug} · Example Pharmacy", None, status,
        provider="Example Pharmacy", insurer="Example Health Plan", type=kind, category=kind,
        billable_period={"start": day, "end": day}, items=items,
    )


RECORDS = [
    medicine("capsule", "Sample capsule 10 mg", (RXNORM, "RX-1")),
    fill("capsulefill", "2024-04-05", "Sample capsule 10 mg", (RXNORM, "RX-1"), ndc="0000-0001-01"),
    # By an equal RxNorm code, whatever it is called.
    claim("coded", "2024-05-06", "SMPL CAP 10MG", (RXNORM, "RX-1")),
    # By the NDC of the pharmacy's own fill, in another form.
    claim("package", "2024-06-07", "SMPL CAP 10MG", (NDC, "00000000101")),
    # The pharmacy's own record has it.
    claim("dispensed", "2024-04-05", "SMPL CAP 10MG", (RXNORM, "RX-1")),
    # While it was current.
    claim("early", "2024-03-04", "SMPL CAP 10MG", (RXNORM, "RX-1")),
    claim("cancelled", "2024-07-08", "SMPL CAP 10MG", (RXNORM, "RX-1"), status="cancelled"),
    claim("mistaken", "2024-07-09", "SMPL CAP 10MG", (RXNORM, "RX-1"), status="entered-in-error"),
    # No pharmacy's.
    claim("clinic", "2024-07-10", "SMPL CAP 10MG", (RXNORM, "RX-1"), kind="professional"),
    # A line the claim's totals leave out, of one that is another drug's.
    claim(
        "leftout", "2024-07-11", "Example cream 1 %", (RXNORM, "RX-1"), (RXNORM, "RX-9"),
        uncounted=(0,),
    ),
    # By its name, where no code of it is equal.
    medicine("tablet", "Sample tablet, 5 mg", (RXNORM, "RX-2")),
    claim("named", "2024-05-07", "SAMPLE TABLET 5 MG", (NDC, "00000-0002-01")),
    claim("othername", "2024-05-08", "SAMPLE TABLET 50 MG", (NDC, "00000-0002-02")),
    # By its code, and not by the name of another.
    medicine("drops", "Sample drops", (RXNORM, "RX-3")),
    medicine("syrup", "Sample syrup"),
    claim("codefirst", "2024-05-09", "Sample syrup", (RXNORM, "RX-3")),
    claim("uncoded", "2024-05-10", "Sample  Syrup"),
    # A drug of which there is no medicine.
    claim("nomedicine", "2024-05-11", "Example ointment", (RXNORM, "RX-8")),
    # The plan denied it, and the household paid nothing of it; paid some of it;
    # and denied one line of two.
    medicine("lotion", "Sample lotion", (RXNORM, "RX-4")),
    claim("denied", "2024-05-15", "SMPL LOTION", (RXNORM, "RX-4"), denied=[(0, "Reject Reason")]),
    claim(
        "partly", "2024-05-14", "SMPL LOTION", (RXNORM, "RX-4"), (RXNORM, "RX-4"),
        denied=[(0, "denialreason")],
    ),
    claim(
        "deniedpaid", "2024-05-13", "SMPL LOTION", (RXNORM, "RX-4"),
        denied=[(0, "Denial Reason")], owe="12.00",
    ),
    # A reversal (returned to stock) is a cancelled claim, as above.
]
RECORDS.sort(key=lambda r: r["date"], reverse=True)

index = {
    "schema": "medrec-index/v1", "person": PERSON, "rev": 1, "documents": [], "records": RECORDS,
}
with open(os.path.join(HERE, "index.json"), "w") as f:
    f.write(json.dumps(index, indent=1, ensure_ascii=False) + "\n")
