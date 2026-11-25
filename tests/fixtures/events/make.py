"""Writes tests/fixtures/events/: an invented household of two (a parent and a
child) whose records give each event rule of docs/data-contract.md something to
judge. Every story, name, date and amount is made up from the rule's text."""
import hashlib, json, os, sys

out = sys.argv[1]
FORMAT = int(sys.argv[2])
os.makedirs(out, exist_ok=True)
PARENT, CHILD = "p_parentaaaaaaaaaaaaaa", "p_childaaaaaaaaaaaaaaa"
PLAN = {"kind": "fhir", "cli": "fakeprov", "organization_id": "example-plan", "fhir_base": "https://plan.example.test/fhir"}
PORTAL = {"kind": "fhir", "cli": "fakeprov", "organization_id": "example-joint", "fhir_base": "https://portal.example.test/fhir"}
FILE = {"kind": "file"}
n = [0]


def rid(prefix, tag):
    tag = "".join(c for c in tag.lower() if c in "abcdefghijklmnopqrstuvwxyz")
    return f"{prefix}_{(tag + 'a' * 20)[:20]}"


def prov(src):
    return [{"source": src, "method": "fhir-api" if src is not FILE else "file",
             "filed_at": "2026-11-20T10:00:00Z", "filed_by": "medrec 0.9.0"}]


def usd(x):
    return {"amount": f"{x:.2f}", "currency": "USD"}


def line(seq, code, display, cat, amounts, revenue=None, counted=True, records=None, serviced=None):
    it = {"sequence": seq, "category": cat, "service": {"system": "http://www.ama-assn.org/go/cpt", "code": code, "display": display},
          "amounts": {k: usd(v) for k, v in amounts.items()}}
    if revenue:
        it["revenue"] = {"system": "https://www.nubc.org/CodeSystem/RevenueCodes", "code": revenue, "display": display}
    if not counted:
        it["counted"] = False
    if records:
        it["records"] = records
    if serviced:
        it["serviced"] = serviced
    return it


def claim(tag, ctype, provider, start, end, lines, totals, category=None, display=None, results=None):
    f = {"provider": provider, "insurer": "Example Health Plan", "billable_period": {"start": start, "end": end},
         "type": ctype, "amounts": {k: usd(v) for k, v in totals.items()}, "items": lines}
    if category:
        f["category"] = category
    if results:
        f["results"] = results
    names = [l["service"]["display"] for l in lines if l.get("counted", True)]
    return {"id": rid("r", tag), "kind": "claim", "key": f"claim|example|{tag}|{start}", "date": start,
            "display": display or f"{names[0] if names else 'Care'} · {provider}", "fields": f, "provenance": prov(PLAN)}


def rec(tag, kind, date, display, fields, src=PORTAL, status=None):
    r = {"id": rid("r", tag), "kind": kind, "key": f"{kind}|{tag}|{date[:10]}", "date": date, "display": display,
         "fields": fields, "provenance": prov(src)}
    if status:
        r["status"] = status
    return r


def doc(tag, doc_type, date, org, title=None, statement=None, src=FILE, near=None):
    d = {"id": rid("d", tag), "path": f"Documents/{date[:4]}/{date}_{tag}.pdf", "sha256": hashlib.sha256(tag.encode()).hexdigest(),
         "md5": hashlib.md5(tag.encode()).hexdigest(), "size": 1000, "media_type": "application/pdf", "doc_type": doc_type,
         "date": date, "organization": org, "filed_at": f"{date}T12:00:00Z", "origin": {"kind": "file", "action": "copied"},
         "provenance": prov(src)[0]}
    if title:
        d["title"] = title
    if statement:
        d["statement"] = statement
    if near:
        d["near_duplicates"] = [{"document": near, "similarity": 0.97}]
    return d


# --- the parent ---------------------------------------------------------------
recs, docs, texts = [], [], {}


def visit(tag, provider, date, code, display, totals, records=None, results=None, category="visit", more=()):
    """A professional claim of one line with the claim's own amounts."""
    ln = line(1, code, display, category, totals, records=records)
    recs.append(claim(tag, "professional", provider, date, date, [ln, *more], totals, category=category, results=results))


# C1, C2, D3s: two claims of one facility on one day are one emergency visit, with
# its doctor's. A statement for one claim's share.
recs.append(claim("burnroom", "institutional", "Sample Valley Hospital", "2024-10-08", "2024-10-08", [
    line(1, "99283", "Emergency room visit", "visit", {"billed": 1800, "allowed": 700, "plan_paid": 600, "you_owe": 100}, revenue="0450"),
], {"billed": 1800, "allowed": 700, "plan_paid": 600, "copay": 100, "you_owe": 100}))
recs.append(claim("burncare", "institutional", "Sample Valley Hospital", "2024-10-08", "2024-10-08", [
    line(1, "16020", "Burn dressing", "procedure", {"billed": 320, "allowed": 125, "plan_paid": 100, "you_owe": 25}, revenue="0459"),
], {"billed": 320, "allowed": 125, "plan_paid": 100, "coinsurance": 25, "you_owe": 25}))
visit("burndoctor", "T PLACEHOLDER", "2024-10-08", "99283", "Emergency room visit",
      {"billed": 510, "allowed": 180, "plan_paid": 144, "coinsurance": 36, "you_owe": 36})
docs.append(doc("burnbill", "bill", "2024-11-19", "Sample Valley Hospital", "Statement",
                statement={"statement_date": "2024-11-19", "amount_due": usd(100)}))

# C2, X1: a long stay, with one doctors' claim and one laboratory's inside it.
recs.append(claim("restroom", "institutional", "Example Harbor Hospital", "2022-03-01", "2022-03-12", [
    line(1, "00000", "Room and board", "facility", {"billed": 40000, "allowed": 21000, "plan_paid": 20000, "you_owe": 1000}, revenue="0120"),
], {"billed": 40000, "allowed": 21000, "plan_paid": 20000, "deductible": 1000, "you_owe": 1000}))
visit("restdoctor", "Example Maternal Medicine", "2022-03-05", "99232", "Hospital visit",
      {"billed": 240, "allowed": 110, "plan_paid": 110, "you_owe": 0})
visit("restlab", "Sample Blood Bank", "2022-03-06", "86900", "Blood type",
      {"billed": 44, "allowed": 9, "plan_paid": 9, "you_owe": 0}, category="lab")

# C3: an outpatient facility claim with a line its totals leave out, and the
# surgeon's that day.
recs.append(claim("kneeroom", "institutional", "Example Day Surgery", "2024-05-14", "2024-05-14", [
    line(1, "29881", "Knee procedure", "procedure", {"billed": 6400, "allowed": 3000, "plan_paid": 2700, "you_owe": 300}, revenue="0360"),
    line(2, "29880", "Knee procedure, other part", "procedure", {"billed": 2100, "plan_paid": 0, "you_owe": 2100}, revenue="0360", counted=False),
], {"billed": 6400, "allowed": 3000, "plan_paid": 2700, "coinsurance": 300, "you_owe": 300}))
visit("kneesurgeon", "Sample Surgeons", "2024-05-14", "29881", "Knee procedure",
      {"billed": 1900, "allowed": 900, "plan_paid": 810, "coinsurance": 90, "you_owe": 90}, category="procedure")

# C4: one provider's hospital claims on touching days, with no facility's.
for tag, date in [("watchone", "2023-10-03"), ("watchtwo", "2023-10-04")]:
    visit(tag, "Example Heart Doctors", date, "99221", "Hospital visit",
          {"billed": 400, "allowed": 190, "plan_paid": 190, "you_owe": 0})

# C5, E1, E3, E4, D4: a portal visit, its doctor's claim and another provider's
# test, the test's report, a message after it, and the portal's note of it.
report = rec("hipreport", "result", "2023-04-18T11:00:00-04:00", "Hip angle", {"value": {"value": 41, "unit": "deg"}})
recs.append(report)
recs.append(rec("hipcheck", "encounter", "2023-04-18T09:15:00-04:00", "Office Visit", {
    "class": "AMB", "location": "Sample Joint Clinic", "service_provider": "Sample Joint Clinic",
    "practitioner": "Quinn Placeholder, DO", "reason": "Hip pain",
    "period": {"start": "2023-04-18T09:15:00-04:00", "end": "2023-04-18T09:50:00-04:00"}}))
recs.append(rec("hipmessage", "encounter", "2023-04-20T16:00:00-04:00", "Patient Message", {
    "class": "VR", "service_provider": "Sample Joint Clinic", "period": {"start": "2023-04-20T16:00:00-04:00"}}))
visit("hipdoctor", "Q PLACEHOLDER", "2023-04-18", "99214", "Office visit",
      {"billed": 260, "allowed": 140, "plan_paid": 140, "you_owe": 0})
visit("hipimage", "Example Imaging Readers", "2023-04-18", "73502", "Hip x-ray",
      {"billed": 95, "allowed": 31, "plan_paid": 31, "you_owe": 0}, records=[report["id"]], results=[report["id"]], category="imaging")
docs.append(doc("hipnote", "visit_summary", "2023-04-18", "Sample Joint Clinic", "After Visit Summary", src=PORTAL))

# C6, D2, D6: a dental visit, its receipt, and a second scan of that.
recs.append(claim("cleaning", "oral", "Sample Dental Studio", "2023-08-22", "2023-08-22", [
    line(1, "D1110", "Teeth cleaning", "dental", {"billed": 130, "allowed": 90, "plan_paid": 45, "you_owe": 45}),
], {"billed": 130, "allowed": 90, "plan_paid": 45, "coinsurance": 45, "you_owe": 45}, category="dental"))
docs.append(doc("dentreceipt", "receipt", "2023-08-22", "Sample Dental Studio", "Payment receipt"))
docs.append(doc("dentrescan", "receipt", "2023-08-22", "Sample Dental Studio", "Payment receipt", near=rid("d", "dentreceipt")))

# C1, C5: a facility claim over several weeks takes no other claims.
recs.append(claim("wound", "institutional", "Example Wound Center", "2023-01-09", "2023-03-06", [
    line(1, "97597", "Wound care", "procedure", {"billed": 2600, "allowed": 1100, "plan_paid": 990, "you_owe": 110}, revenue="0519"),
], {"billed": 2600, "allowed": 1100, "plan_paid": 990, "coinsurance": 110, "you_owe": 110}))
visit("cough", "R SPECIMEN", "2023-02-13", "99212", "Office visit",
      {"billed": 120, "allowed": 60, "plan_paid": 60, "you_owe": 0})

# C6, D5: a pharmacy's fills of one day, a vaccine at another that day, and the
# paper of the day before.
for tag, drug in [("fillone", "Amlodipine 5 mg"), ("filltwo", "Folic acid 1 mg"), ("fillthree", "Loratadine 10 mg")]:
    recs.append(claim(tag, "pharmacy", "Sample Corner Pharmacy", "2023-11-14", "2023-11-14", [
        {"sequence": 1, "category": "pharmacy", "service": {"system": "http://hl7.org/fhir/sid/ndc", "code": "00000000000", "display": drug},
         "amounts": {"billed": usd(18), "plan_paid": usd(12), "you_owe": usd(6)}}],
        {"billed": 18, "plan_paid": 12, "copay": 6, "you_owe": 6}, category="pharmacy", display=drug))
recs.append(claim("shot", "pharmacy", "Example Mail Pharmacy", "2023-11-14", "2023-11-14", [
    {"sequence": 1, "category": "pharmacy", "service": {"system": "http://hl7.org/fhir/sid/ndc", "code": "00000000001", "display": "Pneumonia vaccine"},
     "amounts": {"billed": usd(150), "plan_paid": usd(150), "you_owe": usd(0)}}],
    {"billed": 150, "plan_paid": 150, "you_owe": 0}, category="pharmacy", display="Pneumonia vaccine"))
docs.append(doc("fillpaper", "prescription", "2023-11-13", "Sample Corner Pharmacy", "Prescription"))

# C5: two office visits and two providers' tests on one day: the tests are not
# assigned.
visit("earvisit", "Q SPECIMEN", "2024-02-27", "99213", "Office visit",
      {"billed": 150, "allowed": 72, "plan_paid": 72, "you_owe": 0})
visit("footvisit", "W TESTPERSON", "2024-02-27", "99212", "Office visit",
      {"billed": 110, "allowed": 55, "plan_paid": 55, "you_owe": 0})
visit("daytest", "Sample Blood Bank", "2024-02-27", "80061", "Cholesterol test",
      {"billed": 60, "allowed": 12, "plan_paid": 12, "you_owe": 0}, category="lab")
visit("dayimage", "Example Imaging Readers", "2024-02-27", "73630", "Foot x-ray",
      {"billed": 85, "allowed": 28, "plan_paid": 28, "you_owe": 0}, category="imaging")

# D3, N5: a bill from a practice no claim names, for the only visit that day and
# its share.
visit("hearing", "N TESTPERSON", "2023-06-20", "99204", "Office visit",
      {"billed": 310, "allowed": 150, "plan_paid": 100, "deductible": 50, "you_owe": 50})
docs.append(doc("hearingbill", "bill", "2023-06-20", "Sample Hearing Center", "Statement",
                statement={"statement_date": "2023-06-20", "amount_due": usd(50), "due_date": "2023-07-18"}))

# X2: the parent's only visit on the day of a paper filed under the child.
visit("shoulder", "Y EXAMPLE", "2021-11-16", "99202", "Office visit",
      {"billed": 180, "allowed": 88, "plan_paid": 68, "copay": 20, "you_owe": 20})

parent = {"schema": "medrec-index/v1", "person": PARENT, "rev": 1, "documents": docs, "records": recs, "studies": [], "imports": []}
texts[rid("d", "burnbill")] = "Sample Valley Hospital\nStatement\nAmount due 100.00"

# --- the child ------------------------------------------------------------------
precs, recs = recs, []
# C4, X1: intensive care during the parent's stay, and a visit after it.
for tag, code, date in [("intakeone", "99468", "2022-03-09"), ("intaketwo", "99469", "2022-03-10")]:
    visit(tag, "Example Infant Intensive Care", date, code, "Intensive care day",
          {"billed": 1500, "allowed": 800, "plan_paid": 800, "you_owe": 0})
visit("laterchild", "B SPECIMEN", "2022-06-14", "99391", "Checkup",
      {"billed": 190, "allowed": 95, "plan_paid": 95, "you_owe": 0})
crecs = recs
cdocs = [doc("misfiled", "report", "2021-11-16", "Example Sports Clinic", "Report")]
child = {"schema": "medrec-index/v1", "person": CHILD, "rev": 1, "documents": cdocs, "records": crecs, "studies": [], "imports": []}
ctexts = {rid("d", "misfiled"): "Example Sports Clinic\nReport\nShoulder range of motion"}

household = {"schema": "medrec-household/v1", "format": FORMAT, "folder_id": "f_exampleaaaaaaaaaaaaa", "rev": 1, "people": [
    {"id": PARENT, "name": "Alex Example", "folder": "Alex Example", "birth_date": "1990-01-01", "adult": True, "created_at": "2025-01-01T00:00:00Z"},
    {"id": CHILD, "name": "Robin Example", "folder": "Robin Example", "birth_date": "2022-03-09", "guardians": [PARENT], "created_at": "2025-01-10T00:00:00Z"},
], "created_at": "2026-01-01T00:00:00Z"}

def dump(name, v):
    with open(os.path.join(out, name), "w") as f:
        json.dump(v, f, indent=1)
        f.write("\n")

dump("household.json", household)
dump("parent.index.json", parent)
dump("parent.text.json", {"schema": "medrec-text/v1", "person": PARENT, "texts": texts})
dump("child.index.json", child)
dump("child.text.json", {"schema": "medrec-text/v1", "person": CHILD, "texts": ctexts})
print(len(precs), len(docs), len(crecs), len(cdocs))
