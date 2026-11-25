"""Writes tests/fixtures/private/adult.{index,text}.json: an invented adult's
records, one private by each of the rules `code`, `medicine`, `test` and
`provider` (a record and a document), and some not."""
import json, os

HERE = os.path.dirname(os.path.abspath(__file__))
PERSON = "p_" + "privateadult" + "a" * 8


def pad(prefix, stem):
    s = prefix + "_" + stem + "a" * (20 - len(stem))
    assert len(s) == 22, s
    return s


def src(org):
    return [{"source": {"kind": "fhir", "cli": "fakeprov", "organization_id": org,
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
    rec("smoking", "condition", "2025-01-14", "Nicotine dependence, cigarettes", {},
        "sample-neighborhood", icd("F17.210", "Nicotine dependence, cigarettes, uncomplicated"),
        "active"),
    rec("migraine", "condition", "2024-11-12", "Migraine headache", {},
        "sample-headache", icd("G43.909", "Migraine, unspecified, not intractable"), "active"),
    rec("varenicline", "medication", "2025-01-15", "Varenicline 1 mg tablet", {},
        "sample-neighborhood", status="active"),
    rec("sumatriptan", "medication", "2024-11-13", "Sumatriptan 50 mg tablet", {},
        "sample-headache", status="active"),
    rec("quitvisit", "encounter", "2025-02-20T17:00:00Z", "Group Visit",
        {"class": "AMB", "service_provider": "Example Recovery Center",
         "period": {"start": "2025-02-20T17:00:00Z", "end": "2025-02-20T18:30:00Z"}},
        "example-recovery"),
    rec("headvisit", "encounter", "2025-03-18T15:00:00Z", "Follow-up Visit",
        {"class": "AMB", "service_provider": "Sample Headache Clinic",
         "period": {"start": "2025-03-18T15:00:00Z", "end": "2025-03-18T15:20:00Z"}},
        "sample-headache"),
    rec("magnesium", "result", "2025-03-18T15:10:00Z", "Magnesium",
        {"value": {"value": 2, "unit": "mg/dL"}, "reference_range": "1.7-2.4 mg/dL"},
        "sample-headache"),
    rec("screen", "result", "2025-03-18T15:10:00Z", "Drug screen",
        {"value_text": "None found"}, "sample-headache"),
]
documents = [
    doc("quitnotice", "2025-04-08", "letter", "Letter", "Sample Addiction Services"),
    doc("headsummary", "2025-03-18", "visit_summary", "After Visit Summary",
        "Sample Headache Clinic"),
]
texts = {
    pad("d", "quitnotice"): "Sample Addiction Services\nLetter\nThe next class is in the cedar room.",
    pad("d", "headsummary"): "Sample Headache Clinic\nAfter Visit Summary\nBring the diary in six "
                             "weeks.",
}
idx = {"schema": "medrec-index/v1", "person": PERSON, "rev": 1, "documents": documents,
       "records": records, "studies": [], "imports": []}
with open(os.path.join(HERE, "adult.index.json"), "w") as f:
    json.dump(idx, f, indent=1)
    f.write("\n")
with open(os.path.join(HERE, "adult.text.json"), "w") as f:
    json.dump({"schema": "medrec-text/v1", "person": PERSON, "texts": texts}, f, indent=1)
    f.write("\n")
