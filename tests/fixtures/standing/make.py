"""Writes tests/fixtures/standing/{household,index}.json: an invented person's
conditions, allergies and medicines, one for each rule of "Standing" in
docs/data-contract.md, and a result. `medrec` writes standing.json from
them."""
import json, os

HERE = os.path.dirname(os.path.abspath(__file__))
PERSON = "p_robinaaaaaaaaaaaaaaa"
# cli, organization, name, the day it was last read
SOURCES = {
    "north": ("Example Health", "2024-05-02"),
    "east": ("Sample Clinic East", "2024-05-03"),
    "walkin": ("Example Walk-in", "2024-05-04"),
    "dropin": ("Sample Drop-in", "2024-05-05"),
}


def pad(stem):
    s = "r_" + stem + "a" * (20 - len(stem))
    assert len(s) == 22, s
    return s


def provenance(org, path):
    p = {"source": {"kind": "fhir", "cli": "fakeprov", "organization_id": org,
                    "fhir_base": f"https://{org}.example.test/fhir"},
         "method": "fhir-api", "fetched_at": SOURCES[org][1] + "T10:00:00Z",
         "filed_at": "2024-06-01T10:00:00Z", "filed_by": "medrec 0.9.9"}
    if path:
        p["raw_path"] = path
    return p


def rec(stem, kind, date, name, words, code=None, status=None, fields=None, by_hand=False,
        heard=None, wrote=0):
    """`words`: (source, what it says). `heard`: the sources whose words it
    keeps, else all. `wrote`: the place of the one whose its status is."""
    r = {"id": pad(stem), "kind": kind, "key": f"{kind}|{stem}|{date}", "date": date,
         "display": name, "fields": fields or {}, "provenance": []}
    said = []
    for n, (org, w) in enumerate(words):
        path = None if by_hand else f"Robin Example/Records/{org}/{stem}-{n}.json"
        r["provenance"].append(provenance(org, path))
        if not by_hand and (heard is None or org in heard):
            said.append({"raw_path": path, **w})
    if said and kind in (C, A, M) and fields != FILL:
        r["words"] = said
        if len(words) > 1 and wrote is not None:
            r["fields_from"] = r["provenance"][wrote]["raw_path"]
    first = status or (words[wrote or 0][1].get("status") if words else None)
    if first:
        r["status"] = first
    if code:
        r["code"] = {"system": "http://example.org/codes", "code": code, "display": name}
    return r


C, A, M = "condition", "allergy", "medication"
FILL = {"source": "dispense"}
LIST = ["problem-list-item"]
VISIT = ["encounter-diagnosis"]


def con(status=None, sure=None, cats=LIST, gone=None):
    w = {}
    if status:
        w["status"] = status
    if sure:
        w["verification"] = sure
    if cats:
        w["categories"] = cats
    if gone:
        w["abatement"] = gone
    return w


def med(status=None, end=None, of=None):
    w = {}
    if status:
        w["status"] = status
    if end:
        w["end"], w["end_of"] = end, of
    return w


RECORDS = [
    # Conditions
    rec("plain", C, "2024-04-20", "Sample stiffness", [("north", con("active", "confirmed"))], "C-1"),
    rec("bothsay", C, "2024-04-19", "Sample ache", [("north", con("active", "confirmed")), ("east", con("resolved", "confirmed"))], "C-2"),
    rec("tworecords", C, "2024-04-18", "Sample rash", [("north", con("active"))], "C-3"),
    rec("tworecordsother", C, "2024-04-17", "Sample rash", [("east", con("resolved"))], "C-3"),
    rec("again", C, "2024-04-16", "Sample cough", [("north", con("active"))], "C-4"),
    rec("againbefore", C, "2019-02-03", "Sample cough", [("north", con("resolved", gone="2019-03-01"))], "C-4"),
    rec("visitonly", C, "2024-04-15", "Sample sprain", [("walkin", con("active", cats=VISIT))], "C-5"),
    rec("onlist", C, "2024-04-14", "Sample itch", [("north", con("active"))], "C-6"),
    rec("onlistvisit", C, "2024-04-13", "Sample itch", [("walkin", con("active", cats=VISIT))], "C-6"),
    rec("refuted", C, "2024-04-12", "Sample fever", [("north", con("active", "refuted"))], "C-7"),
    rec("refutedbyone", C, "2024-04-11", "Sample tremor", [("north", con(None, "refuted")), ("east", con("active", "confirmed"))], "C-8"),
    rec("error", C, "2024-04-10", "Sample burn", [("north", con(None, "entered-in-error"))], "C-9"),
    rec("provisional", C, "2024-04-09", "Sample spasm", [("north", con("active", "provisional"))], "C-10"),
    rec("suretwice", C, "2024-04-08", "Sample strain", [("north", con("active", "unconfirmed")), ("east", con("active", "confirmed"))], "C-11"),
    rec("abated", C, "2024-04-07", "Sample chill", [("north", con(gone="2022-05"))], "C-12"),
    rec("errorbeside", C, "2024-04-06", "Sample swelling", [("north", con(None, "entered-in-error")), ("east", con("active"))], "C-13"),
    rec("byhand", C, "2024-04-05", "Sample bruise", [("north", {})], by_hand=True),
    rec("bothended", C, "2024-04-04", "Sample knot", [("north", con("resolved")), ("east", con("inactive"))], "C-14"),
    rec("visitended", C, "2024-04-03", "Sample blister", [("walkin", con("resolved", cats=VISIT))], "C-15"),
    rec("wrote", C, "2024-04-02", "Sample lump", [("north", con("resolved")), ("east", con("resolved"))], "C-16", heard=["east"]),
    rec("mayhavelist", C, "2024-04-01", "Sample hiccup", [("dropin", con("active", cats=VISIT))], "C-17"),
    rec("mayhavelisttoo", C, "2024-03-31", "Sample graze", [("north", con("resolved")), ("dropin", con("active"))], "C-18", heard=["north"]),
    # Allergies
    rec("nut", A, "2024-03-20", "Sample nut", [("north", con("active", "confirmed", None))]),
    rec("nutagain", A, "2024-03-19", "SAMPLE NUT", [("east", con("active", "confirmed", None))]),
    rec("oldpollen", A, "2001-03-18", "Sample pollen", [("north", con("active", None, None))]),
    rec("inactive", A, "2024-03-17", "Sample dust", [("north", con("inactive", None, None))]),
    rec("refutedallergy", A, "2024-03-16", "Sample latex", [("north", con("active", "refuted", None))]),
    rec("errorallergy", A, "2024-03-15", "Sample dye", [("north", con("active", "entered-in-error", None))]),
    rec("oneheard", A, "2024-03-14", "Sample glue", [("north", con("active", None, None)), ("east", con("inactive", None, None))], heard=["east"], wrote=None),
    rec("errorunheard", A, "2024-03-13", "Sample resin", [("north", con("entered-in-error", None, None)), ("east", con("active", None, None))], heard=["north"], wrote=None),
    rec("unheardrefuted", A, "2024-03-12", "Sample moss", [("north", con("active", "refuted", None)), ("dropin", con("active", None, None))], heard=["north"], wrote=None),
    rec("besiderefuted", A, "2024-03-11", "Sample moss", [("east", con("active", None, None))]),
    rec("unheardended", A, "2024-03-10", "Sample fern", [("north", con("inactive", None, None)), ("dropin", con("active", None, None))], heard=["north"], wrote=None),
    rec("besideended", A, "2024-03-09", "Sample fern", [("east", con("inactive", None, None))]),
    # Medicines
    rec("noend", M, "2024-02-20", "Sample tablet", [("north", med("active"))], "M-1"),
    rec("noendtwice", M, "2024-02-19", "Sample tablet", [("east", med("active"))], "M-1"),
    rec("ends", M, "2024-02-18", "Sample capsule", [("north", med("active", "2024-03-31", "prescription"))], "M-2"),
    rec("endsmonth", M, "2024-02-17", "Sample drops", [("north", med("active", "2024-06", "dosage"))], "M-3"),
    rec("endsatone", M, "2024-02-16", "Sample syrup", [("north", med("active", "2024-03-31", "prescription")), ("east", med("active"))], "M-4"),
    rec("endstwice", M, "2024-02-15", "Sample cream", [("north", med("active", "2024-03-31", "prescription")), ("east", med("active", "2024-06-30", "taking"))], "M-5"),
    rec("onhold", M, "2024-02-14", "Sample patch", [("north", med("on-hold"))], "M-6"),
    rec("draft", M, "2024-02-13", "Sample gel", [("north", med("draft"))], "M-7"),
    rec("unknown", M, "2024-02-12", "Sample spray", [("north", med("unknown"))], "M-8"),
    rec("nottaken", M, "2024-02-11", "Sample lozenge", [("north", med("not-taken"))], "M-9"),
    rec("stoppedatone", M, "2024-02-10", "Sample powder", [("north", med("active")), ("east", med("stopped"))], "M-10"),
    rec("errormedicine", M, "2024-02-08", "Sample wafer", [("north", med("entered-in-error"))], "M-11"),
    rec("noneheard", M, "2024-02-07", "Sample balm", [("north", med("stopped")), ("east", med("stopped"))], "M-12", heard=[], wrote=None),
    rec("endsunheard", M, "2024-02-06", "Sample rinse", [("north", med("active", "2024-03-31", "prescription")), ("east", med("active"))], "M-13", heard=["north"]),
    rec("fillbefore", M, "2024-03-05", "Sample capsule", [("east", med("completed"))], "M-2", fields=FILL),
    rec("fillsoon", M, "2024-04-10", "Sample capsule", [("east", med("completed"))], "M-2", fields=FILL),
    rec("fillafter", M, "2024-05-02", "Sample capsule", [("east", med("completed"))], "M-2", fields=FILL),
    rec("fillreturned", M, "2024-05-20", "Sample capsule", [("east", med("cancelled"))], "M-2", fields=FILL),
    rec("fillerror", M, "2024-05-21", "Sample capsule", [("east", med("entered-in-error"))], "M-2", fields=FILL),
    # One that has no standing
    rec("resulterror", "result", "2024-05-22", "Sample level", [("east", {"status": "entered-in-error"})], "L-1"),
]
RECORDS.sort(key=lambda r: r["date"], reverse=True)

household = {"schema": "medrec-household/v1", "format": 1, "folder_id": "f_exampleaaaaaaaaaaaaa", "rev": 1,
             "created_at": "2024-01-01T00:00:00Z",
             "people": [{"id": PERSON, "name": "Robin Example", "folder": "Robin Example",
                         "created_at": "2024-01-01T00:00:00Z"}],
             "sources": [{"id": "s_" + org + "a" * (20 - len(org)), "kind": "fhir", "person": PERSON, "cli": "fakeprov",
                          "organization": org, "organization_name": name,
                          "fhir_base": f"https://{org}.example.test/fhir", "patient_id": "pat-0001"}
                         for org, (name, _) in SOURCES.items()]}
index = {"schema": "medrec-index/v1", "person": PERSON, "rev": 1, "documents": [], "records": RECORDS}
for name, v in (("household.json", household), ("index.json", index)):
    with open(os.path.join(HERE, name), "w") as f:
        f.write(json.dumps(v, indent=1) + "\n")
