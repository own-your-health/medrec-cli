#!/usr/bin/env python3
"""Writes the antigen files and the release notes' document.xml of an
invented CDSi supporting data archive, in the shape of CDC's published XML:
the antigens, their series and the CVX codes (951 to 954, as in
ScheduleSupportingData.xml) are no real ones. Each series and dose stands for
one rule of `src/schedule.rs`. `medrec::testdata::cdsi_archive` zips them under
"Version 0.1 - 508/"."""

import os

HERE = os.path.dirname(os.path.abspath(__file__))


def el(name, value=""):
    return f"<{name}>{value}</{name}>"


def age(earliest="", cessation=""):
    return (
        "<age>"
        + el("absMinAge")
        + el("minAge")
        + el("earliestRecAge", earliest)
        + el("latestRecAge")
        + el("maxAge")
        + el("effectiveDate")
        + el("cessationDate", cessation)
        + "</age>"
    )


def vaccine(cvx, begin="6 weeks"):
    return (
        "<preferableVaccine>"
        + el("vaccineType", "Sample vaccine")
        + el("cvx", cvx)
        + el("beginAge", begin)
        + el("endAge")
        + el("tradeName")
        + el("mvx")
        + el("volume", "0.5")
        + el("forecastVaccineType", "N")
        + "</preferableVaccine>"
    )


def condition(kind, **v):
    keys = ["startDate", "endDate", "beginAge", "endAge", "interval", "doseCount",
            "doseType", "doseCountLogic", "vaccineTypes", "seriesGroups"]
    return (
        "<condition>"
        + el("conditionID", "1")
        + el("conditionType", kind)
        + "".join(el(k, v.get(k, "")) for k in keys)
        + "</condition>"
    )


def skip(context, conditions, logic="AND", effective="", cessation=""):
    return (
        "<conditionalSkip>"
        + el("context", context)
        + el("setLogic", "n/a")
        + "<set>"
        + el("setID", "1")
        + el("setDescription", "Invented")
        + el("effectiveDate", effective)
        + el("cessationDate", cessation)
        + el("conditionLogic", logic)
        + "".join(conditions)
        + "</set>"
        + "</conditionalSkip>"
    )


def dose(n, earliest, cvx, skips=(), recurring="No", season=("", ""), ages=None):
    return (
        "<seriesDose>"
        + el("doseNumber", f"Dose {n}")
        + (ages if ages is not None else age(earliest))
        + vaccine(cvx)
        + "".join(skips)
        + el("recurringDose", recurring)
        + "<seasonalRecommendation>"
        + el("startDate", season[0])
        + el("endDate", season[1])
        + "</seasonalRecommendation>"
        + "</seriesDose>"
    )


def series(name, antigen, group, doses, kind="Standard", default="Yes"):
    return (
        "<series>"
        + el("seriesName", name)
        + el("targetDisease", antigen)
        + el("vaccineGroup", group)
        + el("seriesAdminGuidance")
        + el("seriesType", kind)
        + "<selectSeries>"
        + el("defaultSeries", default)
        + el("productPath", "No")
        + el("seriesGroupName", "Standard")
        + el("seriesGroup", "1")
        + el("seriesPriority", "A")
        + el("seriesPreference", "1")
        + el("minAgeToStart")
        + el("maxAgeToStart")
        + "</selectSeries>"
        + "".join(doses)
        + "</series>"
    )


def antigen_file(antigen, *all_series):
    body = "".join(all_series).replace("><", ">\n<")
    return (
        '<?xml version="1.0" encoding="utf-8"?>\n'
        "<!-- Invented, in the shape of a CDC CDSi antigen file. -->\n"
        f"<antigenSupportingData>\n<immunity></immunity>\n<contraindications></contraindications>\n{body}\n"
        "</antigenSupportingData>\n"
    )


EXAMPLITIS = antigen_file(
    "Examplitis",
    # The routine path: a skip of evaluation only and one of a condition
    # medrec does not read keep their doses; a count over two skips dose 4;
    # a set out of effect skips nothing; a dose of no age of its own; a
    # recurring dose; an age that ceased beside the one in effect.
    series(
        "Examplitis 4-dose series",
        "Examplitis",
        "Examplitis",
        [
            dose(1, "2 months", "951"),
            dose(2, "4 months", "951",
                 skips=[skip("Forecast", [condition("Completed Series", seriesGroups="1")])]),
            dose(3, "12 months - 4 days", "951",
                 skips=[skip("Evaluation", [condition("Age", beginAge="0 days")])]),
            dose(4, "15 months", "951",
                 skips=[skip("Forecast", [condition("Vaccine Count by Age", beginAge="6 weeks",
                                                    doseCount="2", doseType="Total",
                                                    doseCountLogic="greater than",
                                                    vaccineTypes="951")])]),
            dose(5, "4 years", "951",
                 skips=[skip("Both", [condition("Age", beginAge="0 days")],
                             effective="20990101")],
                 ages=age("5 years", cessation="20200101") + age("4 years")),
            dose(6, "", "951"),
            dose(7, "11 years", "951", recurring="Yes"),
        ],
    ),
    # Neither a series that is not the default nor one of a risk is routine.
    series("Examplitis 2-dose series", "Examplitis", "Examplitis",
           [dose(1, "3 months", "951")], default="No"),
    series("Examplitis risk series", "Examplitis", "Examplitis",
           [dose(1, "1 month", "951")], kind="Risk"),
)

# Two antigens with the same doses, one line; a third with others.
SAMPLEPOX_A = antigen_file(
    "Samplepox A",
    series("Samplepox A 2-dose series", "Samplepox A", "Samplepox",
           [dose(1, "12 months", "952"), dose(2, "4 years", "952")]),
)
SAMPLEPOX_B = antigen_file(
    "Samplepox B",
    series("Samplepox B 2-dose series", "Samplepox B", "Samplepox",
           [dose(1, "12 months", "952"), dose(2, "4 years", "952")]),
)
SAMPLEPOX_C = antigen_file(
    "Samplepox C",
    series("Samplepox C seasonal series", "Samplepox C", "Samplepox",
           [dose(1, "6 months", "953", recurring="Yes", season=("20270801", "20280630"))]),
)

# The release notes' document.xml: its first date is the table's.
NOTES = (
    '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>\n'
    '<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">'
    "<w:body><w:p><w:r><w:t>Invented release notes</w:t></w:r></w:p>"
    "<w:p><w:r><w:t>Released </w:t></w:r><w:r><w:t>September 8</w:t></w:r>"
    "<w:r><w:t>, 2026</w:t></w:r></w:p></w:body></w:document>\n"
)

for name, text in [
    ("AntigenSupportingData- Examplitis.xml", EXAMPLITIS),
    ("AntigenSupportingData- Samplepox A.xml", SAMPLEPOX_A),
    ("AntigenSupportingData- Samplepox B.xml", SAMPLEPOX_B),
    ("AntigenSupportingData- Samplepox C.xml", SAMPLEPOX_C),
    ("document.xml", NOTES),
]:
    with open(os.path.join(HERE, name), "w") as f:
        f.write(text)
