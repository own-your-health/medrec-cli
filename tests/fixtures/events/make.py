"""Writes tests/fixtures/events/: an invented household of two (a parent and a
newborn) whose records give each event rule something to judge. Every story,
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

# 1. An emergency visit with 4 billers and a replaced line the totals leave out.
recs.append(claim("handfac", "institutional", "Example Lakeside Hospital", "2025-02-11", "2025-02-12", [
    line(1, "99285", "Emergency room visit", "visit", {"billed": 5200, "plan_paid": 0, "you_owe": 5200}, revenue="0450", counted=False),
    line(2, "99284", "Emergency room visit", "visit", {"billed": 5200, "allowed": 640, "plan_paid": 490, "you_owe": 150}, revenue="0450"),
    line(3, "85025", "Blood cell test", "lab", {"billed": 800, "allowed": 100, "plan_paid": 100, "you_owe": 0}, revenue="0300"),
], {"billed": 6000, "allowed": 740, "plan_paid": 590, "copay": 150, "you_owe": 150}))
recs.append(claim("handdoc", "professional", "J SAMPLE", "2025-02-11", "2025-02-11", [
    line(1, "99284", "Emergency room visit", "visit", {"billed": 900, "allowed": 240, "plan_paid": 192, "you_owe": 48}),
], {"billed": 900, "allowed": 240, "plan_paid": 192, "coinsurance": 48, "you_owe": 48}, category="visit"))
recs.append(claim("handrad", "professional", "Example Imaging Partners", "2025-02-11", "2025-02-11", [
    line(1, "73130", "Hand x-ray", "imaging", {"billed": 100, "allowed": 25, "plan_paid": 25, "you_owe": 0}),
], {"billed": 100, "allowed": 25, "plan_paid": 25, "you_owe": 0}, category="imaging"))
recs.append(claim("handlab", "professional", "Sample Laboratory Group", "2025-02-12", "2025-02-12", [
    line(1, "80053", "Chemistry tests", "lab", {"billed": 60, "allowed": 12, "plan_paid": 12, "you_owe": 0}),
], {"billed": 60, "allowed": 12, "plan_paid": 12, "you_owe": 0}, category="lab"))
# Its statement, sent weeks later (D3s).
docs.append(doc("handbill", "bill", "2025-03-20", "Example Lakeside Hospital", "Statement",
                statement={"statement_date": "2025-03-20", "amount_due": usd(150)}))

# 2. A birth stay, with a physicians' group's and the anesthesiologists' claims.
recs.append(claim("stayfac", "institutional", "Example Lakeside Hospital", "2024-08-20", "2024-08-23", [
    line(1, "59400", "Room and board", "facility", {"billed": 16000, "allowed": 7200, "plan_paid": 6500, "you_owe": 700}, revenue="0120"),
], {"billed": 16000, "allowed": 7200, "plan_paid": 6500, "deductible": 700, "you_owe": 700}))
recs.append(claim("staygroup", "professional", "Sample Hospitalist Group", "2024-08-23", "2024-08-23", [
    line(1, "99238", "Day of discharge", "visit", {"billed": 350, "allowed": 130, "plan_paid": 130, "you_owe": 0}),
], {"billed": 350, "allowed": 130, "plan_paid": 130, "you_owe": 0}, category="visit"))
recs.append(claim("stayanes", "professional", "Sample Anesthesia Services", "2024-08-21", "2024-08-21", [
    line(1, "01967", "Anesthesia for labor", "anesthesia", {"billed": 2100, "allowed": 600, "plan_paid": 600, "you_owe": 0}),
], {"billed": 2100, "allowed": 600, "plan_paid": 600, "you_owe": 0}, category="anesthesia"))

# 3. A portal visit with its lab claims, results, a call after it and its summary.
res1 = rec("tsh", "result", "2025-05-13T15:00:00-04:00", "Thyroid test", {"value": {"value": 2.1, "unit": "mIU/L"}})
res2 = rec("bloodcount", "result", "2025-05-13T15:00:00-04:00", "Blood count", {"value": {"value": 14, "unit": "g/dL"}})
recs += [res1, res2]
recs.append(rec("skincheck", "encounter", "2025-05-13T14:30:00-04:00", "Office Visit", {
    "class": "AMB", "location": "Example Primary Care Group", "service_provider": "Example Primary Care Group",
    "practitioner": "Casey Sample, MD", "reason": "Skin check",
    "period": {"start": "2025-05-13T14:30:00-04:00", "end": "2025-05-13T15:10:00-04:00"}}))
recs.append(rec("followcall", "encounter", "2025-05-15T09:00:00-04:00", "Telephone", {
    "class": "Telephone", "service_provider": "Example Primary Care Group", "period": {"start": "2025-05-15T09:00:00-04:00"}}))
recs.append(claim("skindoc", "professional", "C SAMPLE", "2025-05-13", "2025-05-13", [
    line(1, "99213", "Office visit", "visit", {"billed": 220, "allowed": 120, "plan_paid": 120, "you_owe": 0}),
], {"billed": 220, "allowed": 120, "plan_paid": 120, "you_owe": 0}, category="visit"))
recs.append(claim("skinlab", "professional", "Example Labs", "2025-05-13", "2025-05-13", [
    line(1, "84443", "Thyroid test", "lab", {"billed": 80, "allowed": 14, "plan_paid": 14, "you_owe": 0}, records=[res1["id"]]),
    line(2, "85025", "Blood count", "lab", {"billed": 35, "allowed": 4, "plan_paid": 4, "you_owe": 0}, records=[res2["id"]]),
], {"billed": 115, "allowed": 18, "plan_paid": 18, "you_owe": 0}, category="lab", results=[res1["id"], res2["id"]]))
docs.append(doc("skinavs", "visit_summary", "2025-05-13", "Example Primary Care Group", "After Visit Summary", src=PORTAL))

# 4. A desk copay the claim says was not owed: the receipt is filed.
recs.append(claim("eyevisit", "professional", "Example Eye Clinic", "2025-04-08", "2025-04-08", [
    line(1, "99213", "Office visit", "visit", {"billed": 190, "allowed": 85, "plan_paid": 85, "you_owe": 0}),
], {"billed": 190, "allowed": 85, "plan_paid": 85, "you_owe": 0}, category="visit"))
docs.append(doc("eyereceipt", "receipt", "2025-04-08", "Example Eye Clinic", "Payment receipt"))
docs.append(doc("eyereceiptcopy", "receipt", "2025-04-08", "Example Eye Clinic", "Payment receipt", near=rid("d", "eyereceipt")))

# 5. A course of therapy over four weeks, and a visit inside it that stays apart.
recs.append(claim("balance", "institutional", "Example Balance Center", "2025-09-02", "2025-09-30", [
    line(1, "97112", "Balance exercises", "medicine", {"billed": 1400, "allowed": 560, "plan_paid": 448, "you_owe": 112}, revenue="0420"),
], {"billed": 1400, "allowed": 560, "plan_paid": 448, "coinsurance": 112, "you_owe": 112}))
recs.append(claim("balanceeval", "professional", "R EXAMPLE", "2025-09-16", "2025-09-16", [
    line(1, "99214", "Office visit", "visit", {"billed": 180, "allowed": 80, "plan_paid": 80, "you_owe": 0}),
], {"billed": 180, "allowed": 80, "plan_paid": 80, "you_owe": 0}, category="visit"))

# 6. Pharmacy fills on one day, a vaccine there later, and the prescription.
for tag, drug in [("fillone", "Naproxen 250 mg"), ("filltwo", "Cetirizine 10 mg")]:
    recs.append(claim(tag, "pharmacy", "Sample Pharmacy #456", "2025-10-06", "2025-10-06", [
        {"sequence": 1, "category": "pharmacy", "service": {"system": "http://hl7.org/fhir/sid/ndc", "code": "00000000000", "display": drug},
         "amounts": {"billed": usd(24), "plan_paid": usd(16), "you_owe": usd(8)}}],
        {"billed": 24, "plan_paid": 16, "copay": 8, "you_owe": 8}, category="pharmacy", display=drug))
recs.append(claim("tetanus", "pharmacy", "Sample Pharmacy #456", "2025-11-03", "2025-11-03", [
    {"sequence": 1, "category": "pharmacy", "service": {"system": "http://hl7.org/fhir/sid/ndc", "code": "00000000001", "display": "Tetanus vaccine"},
     "amounts": {"billed": usd(40), "plan_paid": usd(40), "you_owe": usd(0)}}],
    {"billed": 40, "plan_paid": 40, "you_owe": 0}, category="pharmacy", display="Tetanus vaccine"))
docs.append(doc("rxpaper", "prescription", "2025-10-05", "Sample Pharmacy", "Prescription"))

# 7. Two office visits on one day and the day's tests: not assigned (C5, unsure).
recs.append(claim("twoa", "professional", "A SAMPLE", "2025-06-24", "2025-06-24", [
    line(1, "99213", "Office visit", "visit", {"billed": 160, "allowed": 75, "plan_paid": 75, "you_owe": 0})],
    {"billed": 160, "allowed": 75, "plan_paid": 75, "you_owe": 0}, category="visit"))
recs.append(claim("twob", "professional", "B EXAMPLE", "2025-06-24", "2025-06-24", [
    line(1, "99214", "Office visit", "visit", {"billed": 230, "allowed": 105, "plan_paid": 105, "you_owe": 0})],
    {"billed": 230, "allowed": 105, "plan_paid": 105, "you_owe": 0}, category="visit"))
recs.append(claim("twolab", "professional", "Example Labs", "2025-06-24", "2025-06-24", [
    line(1, "85025", "Blood cell test", "lab", {"billed": 45, "allowed": 7, "plan_paid": 7, "you_owe": 0})],
    {"billed": 45, "allowed": 7, "plan_paid": 7, "you_owe": 0}, category="lab"))

# 8. The parent's visit that a paper filed under the child belongs to (X2).
recs.append(claim("allergy", "professional", "K EXAMPLE", "2024-06-17", "2024-06-17", [
    line(1, "99203", "Office visit", "visit", {"billed": 260, "allowed": 125, "plan_paid": 90, "you_owe": 35})],
    {"billed": 260, "allowed": 125, "plan_paid": 90, "copay": 35, "you_owe": 35}, category="visit"))

# 9. A bill from a practice no claim names, on the only visit that day, for
# the visit's share: sure (D3), and the clinician is learned to belong to it (N5).
docs.append(doc("allergybill", "bill", "2024-06-17", "Example Allergy Associates", "Statement",
                statement={"statement_date": "2024-06-17", "amount_due": usd(35), "due_date": "2024-07-15"}))

parent = {"schema": "medrec-index/v1", "person": PARENT, "rev": 1, "documents": docs, "records": recs, "studies": [], "imports": []}
texts[rid("d", "handbill")] = "Example Lakeside Hospital\nStatement\nAmount due 150.00"

# --- the child: born on the stay's second day -----------------------------------
crecs = []
for tag, code, display, d in [("nbone", "99460", "Newborn care", "2024-08-21"), ("nbtwo", "99462", "Newborn care", "2024-08-22"),
                              ("nbthree", "99238", "Day of discharge", "2024-08-23")]:
    crecs.append(claim(tag, "professional", "Sample Newborn Care Group", d, d, [
        line(1, code, display, "visit", {"billed": 280, "allowed": 110, "plan_paid": 110, "you_owe": 0})],
        {"billed": 280, "allowed": 110, "plan_paid": 110, "you_owe": 0}, category="visit"))
cdocs = [doc("allergyrx", "prescription", "2024-06-17", "Example Allergy for Kids", "Prescription")]
child = {"schema": "medrec-index/v1", "person": CHILD, "rev": 1, "documents": cdocs, "records": crecs, "studies": [], "imports": []}
ctexts = {rid("d", "allergyrx"): "Example Allergy for Kids\nKim Example, M.D.\nRx"}

household = {"schema": "medrec-household/v1", "format": FORMAT, "folder_id": "f_exampleaaaaaaaaaaaaa", "rev": 1, "people": [
    {"id": PARENT, "name": "Alex Example", "folder": "Alex Example", "birth_date": "1990-01-01", "adult": True, "created_at": "2025-01-01T00:00:00Z"},
    {"id": CHILD, "name": "Robin Example", "folder": "Robin Example", "birth_date": "2024-08-21", "guardians": [PARENT], "created_at": "2025-01-10T00:00:00Z"},
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
