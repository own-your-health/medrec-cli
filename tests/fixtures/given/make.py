#!/usr/bin/env python3
"""Writes index.json: an invented person's claim that another source gave at
a place of a private area, as the contract's "What a source gave" keeps it,
beside one that only names its place. The sources are those of
../standing/household.json."""

import json
import os

HERE = os.path.dirname(os.path.abspath(__file__))
PERSON = "p_robinaaaaaaaaaaaaaaa"
ERROR = "entered-in-error"
PLACES = {"north": "Example Health", "east": "Sample Clinic East"}
DAYS = {"north": "2024-07-02", "east": "2024-07-03"}


def oid(prefix, name):
    return (prefix + name + "a" * 22)[:22]


def provenance(place, name):
    return {
        "source": {
            "kind": "fhir",
            "cli": "fakeprov",
            "organization_id": place,
            "fhir_base": f"https://{place}.example.test/fhir",
        },
        "method": "fhir-api",
        "fetched_at": DAYS[place] + "T10:00:00Z",
        "filed_at": "2024-07-04T10:00:00Z",
        "filed_by": "medrec 0.9.9",
        "raw_path": f"Robin Example/Records/{place}/Binary/{name}.json",
    }


def words(said, name):
    """Each source's word, `said` by place."""
    return [
        {
            "raw_path": f"Robin Example/Records/{place}/Binary/{name}.json",
            "status": status,
            "read": DAYS[place] + "T10:00:00Z",
        }
        for place, status in said.items()
    ]


def record(kind, name, date, display, said, **fields):
    r = {
        "id": oid("r_", name),
        "kind": kind,
        "key": f"{kind}|{name}|{date}",
        "date": date,
        "display": display,
        "fields": fields,
        "provenance": [provenance(p, name) for p in said],
    }
    if len(said) > 1:
        r["words"] = words(said, name)
        stated = [p for p, s in said.items() if s != ERROR]
        r["fields_from"] = provenance(stated[0], name)["raw_path"]
        r["status"] = said[stated[0]]
    else:
        status = next(iter(said.values()))
        if status:
            r["status"] = status
    return r


def claim(name, date, provider, owe, said):
    def money(x):
        return {"amount": x, "currency": "USD"}

    amounts = {"billed": money("90.00"), "you_owe": money(owe)}
    return record(
        "claim",
        name,
        date,
        f"Office visit · {provider}",
        said,
        provider=provider,
        insurer="Example Health Plan",
        billable_period={"start": date, "end": date},
        type="professional",
        amounts=amounts,
        items=[
            {
                "sequence": 1,
                "category": "visit",
                "service": {
                    "system": "http://www.ama-assn.org/go/cpt",
                    "code": "99212",
                    "display": "Office visit",
                },
                "amounts": amounts,
            }
        ],
        category="visit",
    )


def paper(kind, name, date, org, title, said, due=None):
    places = list(said)
    d = {
        "id": oid("d_", name),
        "path": f"Robin Example/Claims/2024/{name}.pdf",
        "sha256": name,
        "md5": name,
        "size": 10,
        "media_type": "application/pdf",
        "doc_type": kind,
        "date": date,
        "organization": org,
        "title": title,
        "filed_at": "2024-07-04T10:00:00Z",
        "origin": {"kind": "fhir", "action": "imported"},
        "provenance": provenance(places[0], name),
    }
    if len(places) > 1:
        d["also_from"] = [provenance(p, name) for p in places[1:]]
    if ERROR in said.values():
        d["words"] = words(said, name)
        d["errors"] = [
            {"raw_path": w["raw_path"], "from": w["read"]}
            for w in d["words"]
            if w["status"] == ERROR
        ]
    if due:
        d["statement"] = {
            "statement_date": date,
            "amount_due": {"amount": due, "currency": "USD"},
        }
    return d


def write(name, value):
    with open(os.path.join(HERE, name), "w") as f:
        json.dump(value, f, indent=1, ensure_ascii=False)
        f.write("\n")


BOTH = {"north": "active", "east": ERROR}
NORTH = {"north": None}
hear = claim("hearvisit", "2024-05-07", "Example Hearing Room", "35.00", BOTH)
hear["given"] = [
    {
        "raw_path": provenance("east", "hearvisit")["raw_path"],
        "fields": {"provider": "Sample Counseling Room"},
    }
]

write(
    "index.json",
    {
        "schema": "medrec-index/v1",
        "person": PERSON,
        "rev": 1,
        "studies": [],
        "imports": [],
        "records": [hear, claim("footvisit", "2024-05-14", "Example Foot Room", "20.00", NORTH)],
        "documents": [
            paper("bill", "hearbill", "2024-06-04", "Example Hearing Room", "Hearing statement", NORTH, "35.00"),
            paper("bill", "footbill", "2024-06-11", "Example Foot Room", "Foot statement", NORTH, "20.00"),
        ],
    },
)
