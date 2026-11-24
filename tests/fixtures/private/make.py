"""Writes tests/fixtures/private/adult.{index,text}.json: an invented adult's
records, some of them private by the rules (a mental health code, a medicine,
a behavioral health practice's visit and letter) and some not."""
import json, os

HERE = os.path.dirname(os.path.abspath(__file__))
PERSON = "p_" + "privateadult" + "a" * 8


def pad(prefix, stem):
    s = prefix + "_" + stem + "a" * (20 - len(stem))
    assert len(s) == 22, s
    return s


def src(org):
    return [{"source": {"kind": "fhir", "cli": "mychart", "organization_id": org,
                        "fhir_base": "https://portal.example.test/fhir"},
             "method": "fhir-api", "filed_at": "2026-11-20T10:00:00Z", "filed_by": "medrec 0.9.0"}]


def rec(stem, kind, date, display, fields, org, code=None, status=None):
    r = {"id": pad("r", stem), "kind": kind, "key": f"{kind}|{stem}|{date[:10]}", "date": date,
         "display": display, "fields": fields, "provenance": src(org)}
    if code:
        r["code"] = code
    if status:
        r["status"] = status
    return r


def icd(code, display):
    return {"system": "http://hl7.org/fhir/sid/icd-10-cm", "code": code, "display": display}


def doc(stem, date, doc_type, title, org):
    name = f"Documents/{date[:4]}/{date}_{stem}.pdf"
    return {"id": pad("d", stem), "path": name, "sha256": "0" * 64, "md5": "0" * 32,
            "size": 1000, "media_type": "application/pdf", "doc_type": doc_type, "date": date,
            "organization": org, "filed_at": date + "T12:00:00Z",
            "origin": {"kind": "file", "action": "copied"},
            "provenance": {"source": {"kind": "file"}, "method": "file",
                           "filed_at": "2026-11-20T10:00:00Z", "filed_by": "medrec 0.9.0"},
            "title": title}


records = [
    rec("depression", "condition", "2026-08-01", "Major depressive disorder, single episode, mild",
        {}, "example-family", icd("F32.0", "Major depressive disorder, single episode, mild"),
        "active"),
    rec("hypertension", "condition", "2026-05-01", "Essential hypertension", {},
        "example-family", icd("I10", "Essential (primary) hypertension"), "active"),
    rec("sertraline", "medication", "2026-08-02", "Sertraline 50 mg tablet", {},
        "example-family", status="active"),
    rec("lisinopril", "medication", "2026-05-02", "Lisinopril 10 mg tablet", {},
        "example-family", status="active"),
    rec("therapyvisit", "encounter", "2026-09-01T14:00:00Z", "Office Visit",
        {"class": "AMB", "service_provider": "Example Behavioral Health",
         "period": {"start": "2026-09-01T14:00:00Z", "end": "2026-09-01T15:00:00Z"}},
        "example-behavioral"),
    rec("checkup", "encounter", "2026-06-01T13:00:00Z", "Office Visit",
        {"class": "AMB", "service_provider": "Example Family Practice",
         "period": {"start": "2026-06-01T13:00:00Z", "end": "2026-06-01T13:30:00Z"}},
        "example-family"),
    rec("glucose", "result", "2026-06-01T13:20:00Z", "Glucose, fasting",
        {"value": {"value": 92, "unit": "mg/dL"}, "reference_range": "70-99 mg/dL"},
        "example-family"),
]
documents = [
    doc("therapyletter", "2026-09-05", "letter", "Letter", "Example Counseling Center"),
    doc("checkupavs", "2026-06-01", "visit_summary", "After Visit Summary",
        "Example Family Practice"),
]
texts = {
    pad("d", "therapyletter"): "Example Counseling Center\nLetter\nNext session in four weeks.",
    pad("d", "checkupavs"): "Example Family Practice\nAfter Visit Summary\nBlood pressure recheck in "
                            "three months.",
}
idx = {"schema": "medrec-index/v1", "person": PERSON, "rev": 1, "documents": documents,
       "records": records, "studies": [], "imports": []}
with open(os.path.join(HERE, "adult.index.json"), "w") as f:
    json.dump(idx, f, indent=1)
    f.write("\n")
with open(os.path.join(HERE, "adult.text.json"), "w") as f:
    json.dump({"schema": "medrec-text/v1", "person": PERSON, "texts": texts}, f, indent=1)
    f.write("\n")
