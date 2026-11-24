"""Writes tests/fixtures/events/: an invented household of two (a parent and a
newborn) whose records cover the event shapes the app's brief lists. Every
name, date and amount is made up."""
import hashlib, json, os, sys

out = sys.argv[1]
FORMAT = int(sys.argv[2])
os.makedirs(out, exist_ok=True)
PARENT, CHILD = "p_parentaaaaaaaaaaaaaa", "p_childaaaaaaaaaaaaaaa"
UHC = {"kind": "fhir", "cli": "uhc", "organization_id": "example-plan", "fhir_base": "https://plan.example.test/fhir"}
PORTAL = {"kind": "fhir", "cli": "mychart", "organization_id": "example-family", "fhir_base": "https://portal.example.test/fhir"}
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
            "display": display or f"{names[0] if names else 'Care'} · {provider}", "fields": f, "provenance": prov(UHC)}


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

# 1. An ER visit with 4 billers and a re-coded line the totals leave out.
recs.append(claim("erfac", "institutional", "Example General Hospital", "2026-10-07", "2026-10-08", [
    line(1, "99285", "Emergency room visit", "visit", {"billed": 7000, "plan_paid": 0, "you_owe": 7000}, revenue="0450", counted=False),
    line(2, "99284", "Emergency room visit", "visit", {"billed": 7000, "allowed": 900, "plan_paid": 700, "you_owe": 200}, revenue="0450"),
    line(3, "85025", "Blood cell test", "lab", {"billed": 2000, "allowed": 200, "plan_paid": 200, "you_owe": 0}, revenue="0300"),
], {"billed": 9000, "allowed": 1100, "plan_paid": 900, "copay": 200, "you_owe": 200}))
recs.append(claim("erdoc", "professional", "J SAMPLE", "2026-10-07", "2026-10-07", [
    line(1, "99284", "Emergency room visit", "visit", {"billed": 1200, "allowed": 300, "plan_paid": 240, "you_owe": 60}),
], {"billed": 1200, "allowed": 300, "plan_paid": 240, "coinsurance": 60, "you_owe": 60}, category="visit"))
recs.append(claim("errad", "professional", "Example Radiology Associates", "2026-10-07", "2026-10-07", [
    line(1, "71046", "Chest x-ray", "imaging", {"billed": 120, "allowed": 30, "plan_paid": 30, "you_owe": 0}),
], {"billed": 120, "allowed": 30, "plan_paid": 30, "you_owe": 0}, category="imaging"))
recs.append(claim("erlab", "professional", "Sample Pathology Labs", "2026-10-08", "2026-10-08", [
    line(1, "80053", "Chemistry tests", "lab", {"billed": 80, "allowed": 15, "plan_paid": 15, "you_owe": 0}),
], {"billed": 80, "allowed": 15, "plan_paid": 15, "you_owe": 0}, category="lab"))
# Its statement, sent weeks later (D3s).
docs.append(doc("erbill", "bill", "2026-11-02", "Example General Hospital", "Statement",
                statement={"statement_date": "2026-11-02", "amount_due": usd(200)}))

# 2. A birth stay, with a physicians' group's and the anesthesiologists' claims.
recs.append(claim("birthfac", "institutional", "Example General Hospital", "2026-03-05", "2026-03-08", [
    line(1, "59400", "Room and board", "facility", {"billed": 20000, "allowed": 9000, "plan_paid": 8000, "you_owe": 1000}, revenue="0120"),
], {"billed": 20000, "allowed": 9000, "plan_paid": 8000, "deductible": 1000, "you_owe": 1000}))
recs.append(claim("birthrheum", "professional", "Sample Rheumatology Group", "2026-03-08", "2026-03-08", [
    line(1, "99238", "Day of discharge", "visit", {"billed": 400, "allowed": 150, "plan_paid": 150, "you_owe": 0}),
], {"billed": 400, "allowed": 150, "plan_paid": 150, "you_owe": 0}, category="visit"))
recs.append(claim("birthanes", "professional", "Example Anesthesia Partners", "2026-03-06", "2026-03-06", [
    line(1, "01967", "Anesthesia for labor", "anesthesia", {"billed": 2500, "allowed": 700, "plan_paid": 700, "you_owe": 0}),
], {"billed": 2500, "allowed": 700, "plan_paid": 700, "you_owe": 0}, category="anesthesia"))

# 3. A portal visit with its lab claims, results, a call after it and its summary.
res1 = rec("lipid", "result", "2026-06-10T15:00:00-04:00", "Cholesterol", {"value": {"value": 180, "unit": "mg/dL"}})
res2 = rec("glucose", "result", "2026-06-10T15:00:00-04:00", "Glucose", {"value": {"value": 90, "unit": "mg/dL"}})
recs += [res1, res2]
recs.append(rec("checkup", "encounter", "2026-06-10T14:30:00-04:00", "Office Visit", {
    "class": "AMB", "location": "Example Family Practice", "service_provider": "Example Family Practice",
    "practitioner": "Casey Sample, MD", "reason": "Annual checkup",
    "period": {"start": "2026-06-10T14:30:00-04:00", "end": "2026-06-10T15:10:00-04:00"}}))
recs.append(rec("callback", "encounter", "2026-06-12T09:00:00-04:00", "Telephone", {
    "class": "Telephone", "service_provider": "Example Family Practice", "period": {"start": "2026-06-12T09:00:00-04:00"}}))
recs.append(claim("checkupdoc", "professional", "C SAMPLE", "2026-06-10", "2026-06-10", [
    line(1, "99396", "Preventive visit", "visit", {"billed": 300, "allowed": 180, "plan_paid": 180, "you_owe": 0}),
], {"billed": 300, "allowed": 180, "plan_paid": 180, "you_owe": 0}, category="visit"))
recs.append(claim("checkuplab", "professional", "Example Labs", "2026-06-10", "2026-06-10", [
    line(1, "80061", "Cholesterol tests", "lab", {"billed": 90, "allowed": 12, "plan_paid": 12, "you_owe": 0}, records=[res1["id"]]),
    line(2, "82947", "Glucose test", "lab", {"billed": 40, "allowed": 5, "plan_paid": 5, "you_owe": 0}, records=[res2["id"]]),
], {"billed": 130, "allowed": 17, "plan_paid": 17, "you_owe": 0}, category="lab", results=[res1["id"], res2["id"]]))
docs.append(doc("checkupavs", "visit_summary", "2026-06-10", "Example Family Practice", "After Visit Summary", src=PORTAL))

# 4. A desk copay the claim says was not owed: the receipt is filed.
recs.append(claim("obvisit", "professional", "Example Womens Care", "2026-05-14", "2026-05-14", [
    line(1, "99213", "Office visit", "visit", {"billed": 250, "allowed": 110, "plan_paid": 110, "you_owe": 0}),
], {"billed": 250, "allowed": 110, "plan_paid": 110, "you_owe": 0}, category="visit"))
docs.append(doc("obreceipt", "receipt", "2026-05-14", "Example Womens Care", "Payment receipt"))
docs.append(doc("obreceiptcopy", "receipt", "2026-05-14", "Example Womens Care", "Payment receipt", near=rid("d", "obreceipt")))

# 5. A course of therapy over four weeks, and a visit inside it that stays apart.
recs.append(claim("therapy", "institutional", "Example Therapy Center", "2026-08-01", "2026-08-29", [
    line(1, "97110", "Guided stretching", "medicine", {"billed": 1600, "allowed": 640, "plan_paid": 512, "you_owe": 128}, revenue="0420"),
], {"billed": 1600, "allowed": 640, "plan_paid": 512, "coinsurance": 128, "you_owe": 128}))
recs.append(claim("therapyeval", "professional", "R EXAMPLE", "2026-08-12", "2026-08-12", [
    line(1, "99214", "Office visit", "visit", {"billed": 200, "allowed": 95, "plan_paid": 95, "you_owe": 0}),
], {"billed": 200, "allowed": 95, "plan_paid": 95, "you_owe": 0}, category="visit"))

# 6. Pharmacy fills on one day, a vaccine there later, and the prescription.
for tag, drug in [("fillone", "Amoxicillin 500 mg"), ("filltwo", "Ibuprofen 600 mg")]:
    recs.append(claim(tag, "pharmacy", "Sample Pharmacy #123", "2026-09-02", "2026-09-02", [
        {"sequence": 1, "category": "pharmacy", "service": {"system": "http://hl7.org/fhir/sid/ndc", "code": "00000000000", "display": drug},
         "amounts": {"billed": usd(30), "plan_paid": usd(20), "you_owe": usd(10)}}],
        {"billed": 30, "plan_paid": 20, "copay": 10, "you_owe": 10}, category="pharmacy", display=drug))
recs.append(claim("flushot", "pharmacy", "Sample Pharmacy #123", "2026-10-15", "2026-10-15", [
    {"sequence": 1, "category": "pharmacy", "service": {"system": "http://hl7.org/fhir/sid/ndc", "code": "00000000001", "display": "Influenza vaccine"},
     "amounts": {"billed": usd(40), "plan_paid": usd(40), "you_owe": usd(0)}}],
    {"billed": 40, "plan_paid": 40, "you_owe": 0}, category="pharmacy", display="Influenza vaccine"))
docs.append(doc("rxpaper", "prescription", "2026-09-01", "Sample Pharmacy", "Prescription"))

# 7. Two office visits on one day and the day's tests: not assigned (C5, unsure).
recs.append(claim("twoa", "professional", "A SAMPLE", "2026-07-28", "2026-07-28", [
    line(1, "99213", "Office visit", "visit", {"billed": 200, "allowed": 90, "plan_paid": 90, "you_owe": 0})],
    {"billed": 200, "allowed": 90, "plan_paid": 90, "you_owe": 0}, category="visit"))
recs.append(claim("twob", "professional", "B EXAMPLE", "2026-07-28", "2026-07-28", [
    line(1, "99214", "Office visit", "visit", {"billed": 260, "allowed": 120, "plan_paid": 120, "you_owe": 0})],
    {"billed": 260, "allowed": 120, "plan_paid": 120, "you_owe": 0}, category="visit"))
recs.append(claim("twolab", "professional", "Example Labs", "2026-07-28", "2026-07-28", [
    line(1, "85025", "Blood cell test", "lab", {"billed": 50, "allowed": 8, "plan_paid": 8, "you_owe": 0})],
    {"billed": 50, "allowed": 8, "plan_paid": 8, "you_owe": 0}, category="lab"))

# 8. The parent's visit that a paper filed under the child belongs to (X2).
recs.append(claim("podiatry", "professional", "K EXAMPLE", "2026-03-02", "2026-03-02", [
    line(1, "99203", "Office visit", "visit", {"billed": 300, "allowed": 140, "plan_paid": 100, "you_owe": 40})],
    {"billed": 300, "allowed": 140, "plan_paid": 100, "copay": 40, "you_owe": 40}, category="visit"))

# 9. A bill from a practice no claim names, on the only visit that day, for
# the visit's share: sure (D3), and the clinician is learned to belong to it (N5).
docs.append(doc("podiatrybill", "bill", "2026-03-02", "Example Podiatry Associates", "Statement",
                statement={"statement_date": "2026-03-02", "amount_due": usd(40), "due_date": "2026-03-30"}))

parent = {"schema": "medrec-index/v1", "person": PARENT, "rev": 1, "documents": docs, "records": recs, "studies": [], "imports": []}
texts[rid("d", "erbill")] = "Example General Hospital\nStatement\nAmount due 200.00"

# --- the child: born on the stay's second day -----------------------------------
crecs = []
for tag, code, display, d in [("nbone", "99460", "Newborn care", "2026-03-06"), ("nbtwo", "99462", "Newborn care", "2026-03-07"),
                              ("nbthree", "99238", "Day of discharge", "2026-03-08")]:
    crecs.append(claim(tag, "professional", "Sample Neonatal Physicians", d, d, [
        line(1, code, display, "visit", {"billed": 300, "allowed": 120, "plan_paid": 120, "you_owe": 0})],
        {"billed": 300, "allowed": 120, "plan_paid": 120, "you_owe": 0}, category="visit"))
cdocs = [doc("podiatryrx", "prescription", "2026-03-02", "Example Podiatry for Kids", "Prescription")]
child = {"schema": "medrec-index/v1", "person": CHILD, "rev": 1, "documents": cdocs, "records": crecs, "studies": [], "imports": []}
ctexts = {rid("d", "podiatryrx"): "Example Podiatry for Kids\nKim Example, M.D.\nRx"}

household = {"schema": "medrec-household/v1", "format": FORMAT, "folder_id": "f_exampleaaaaaaaaaaaaa", "rev": 1, "people": [
    {"id": PARENT, "name": "Alex Example", "folder": "Alex Example", "birth_date": "1990-01-01", "adult": True, "created_at": "2026-01-01T00:00:00Z"},
    {"id": CHILD, "name": "Robin Example", "folder": "Robin Example", "birth_date": "2026-03-06", "guardians": [PARENT], "created_at": "2026-03-10T00:00:00Z"},
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
print(len(recs), len(docs), len(crecs), len(cdocs))
