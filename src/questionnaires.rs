//! Questionnaires about mental health and substance use, and what a chart says
//! of tobacco, told by their codes and by medrec's own names for them: which
//! area their answers are of, and whether a child answers them of themself
//! (docs/data-contract.md, "Private" and "About another person").

use std::sync::LazyLock;

use regex::Regex;

use serde_json::Value;

use crate::model::{Code, Record};
use crate::normalize::{COMPONENTS, LOINC};
use crate::private::{MENTAL_HEALTH, SUBSTANCE_USE};

/// Whom an answer is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answers {
    /// The grown person, or teenager, who gives it.
    Grown,
    /// Whoever's chart it is in, at any age.
    Any,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Questionnaire {
    pub kind: &'static str,
    pub area: &'static str,
    pub answers: Answers,
}

const fn of_kind(kind: &'static str, area: &'static str, answers: Answers) -> Questionnaire {
    Questionnaire {
        kind,
        area,
        answers,
    }
}

const POSTNATAL: Questionnaire = of_kind("postnatal depression", MENTAL_HEALTH, Answers::Grown);
const DEPRESSION: Questionnaire = of_kind("depression", MENTAL_HEALTH, Answers::Grown);
const ANXIETY: Questionnaire = of_kind("anxiety", MENTAL_HEALTH, Answers::Grown);
const ANXIETY_AT_ANY_AGE: Questionnaire = of_kind("anxiety", MENTAL_HEALTH, Answers::Any);
const DEPRESSION_AT_ANY_AGE: Questionnaire = of_kind("depression", MENTAL_HEALTH, Answers::Any);
const SUICIDE: Questionnaire = of_kind("suicide risk", MENTAL_HEALTH, Answers::Any);
const ALCOHOL: Questionnaire = of_kind("alcohol use", SUBSTANCE_USE, Answers::Grown);
const DRUGS: Questionnaire = of_kind("drug use", SUBSTANCE_USE, Answers::Grown);
const TOBACCO: Questionnaire = of_kind("tobacco use", SUBSTANCE_USE, Answers::Any);
const OTHER: Questionnaire = of_kind("other mental health", MENTAL_HEALTH, Answers::Any);

/// The first that a name has the words of.
static NAMES: LazyLock<Vec<(Regex, Questionnaire)>> = LazyLock::new(|| {
    [
        (
            r"edinburgh (?:post\S*|depression|scale|score|total|screen\w*)|epds|(?:post-?natal|post-?partum|perinatal|peripartum|antenatal|maternal|paternal|parental|caregiver|mother(?:'?s)?|father(?:'?s)?|parent(?:'?s)?) (?:depress\w*|anxiety|mood|mental health)",
            POSTNATAL,
        ),
        // Those made for children, before the words they share with others.
        (
            r"ces-?dc|children'?s depression|child depression|depression self-rating scale for children",
            DEPRESSION_AT_ANY_AGE,
        ),
        (
            r"(?:child(?:ren)?|childhood|pediatric|adolescent)(?:'?s)? anxiety|preschool anxiety|rcads|spence",
            ANXIETY_AT_ANY_AGE,
        ),
        (
            r"suicid\w*|c-?ssrs|columbia[- ]suicide|self[- ](?:harm\w*|injur\w*)",
            SUICIDE,
        ),
        (
            r"gad[- ]?[27]|generali[sz]ed anxiety|beck anxiety|hamilton anxiety|ham-a|hospital anxiety|control(?:ling)? worrying|worrying too much|nervous,? anxious|trouble relaxing|afraid as if something awful|so restless|easily annoyed or irritable",
            ANXIETY,
        ),
        (
            r"phq(?:[- ]?\d{1,2}[am]?|[- ]a)?|patient health questionnaire|(?:beck|hamilton|geriatric|zung|montgomery)\W+(?:\S+ )?depression|ham-d|hdrs|madrs|qids(?:-sr)?|ces-?d(?:-?r)?|epidemiologic(?:al)? studies depression|little interest or pleasure|feeling down|feeling bad about (?:yourself|myself)|trouble falling or staying asleep|poor appetite or overeating|feeling tired or having little energy|better off dead|hurting (?:yourself|myself|themsel\w+)|trouble concentrating on things|moving or speaking so slowly|how difficult have these problems",
            DEPRESSION,
        ),
        (
            r"^depression$|depression[,:]? (?:and \w+ )?(?:screen\w*|scale|score|severity|inventory|questionnaire|assessment|risk|index|survey|interpretation|total|status|panel|symptoms?)|(?:screen\w*|scale|score|questionnaire|assessment|survey)[,:]? (?:for |of )?(?:\w+ )?depression|depressive (?:symptoms?|disorder|episode)|(?:major|clinical) depression|depressed mood",
            DEPRESSION,
        ),
        (
            r"audit[- ]?c|audit[- ](?:total|score|questionnaire|10)|alcohol use disorders? identification|cage(?:-aid|[- ]questionnaire|[- ]score)|alcohol(?:ic)? (?:screening|use|misuse|abuse|intake|consumption|dependence|risk)|drinks? containing alcohol|alcoholic (?:drinks?|beverages?)|drinks (?:on a typical|per (?:day|week|occasion))|(?:six|five|four) or more drinks|binge drink\w*",
            ALCOHOL,
        ),
        (
            r"dast(?:[- ]?(?:10|20|a))?|crafft|cudit(?:-r)?|(?:drug|substance) (?:use|misuse)|substance abuse|drug abuse (?:screening test|questionnaire)|(?:illicit|recreational|street) drugs?|(?:cannabis|marijuana|opioid) (?:use|misuse)|opioid risk tool",
            DRUGS,
        ),
        (
            r"tobacco|smok(?:e|er|ers|ing|ed|es|eless)|cigarettes?|e-?cig(?:arettes?|s)?|cigars?|vap(?:e|es|ed|ing)|nicotine|cotinine|pack[- ]years?|packs? (?:per|a|/) ?day|fagerstr[oö]m",
            TOBACCO,
        ),
        (r"anxiety|panic (?:disorder|attacks?)", ANXIETY_AT_ANY_AGE),
        (
            r"pcl-?5|(?:pc-)?ptsd|post-?traumatic stress|mood disorder|mdq|bipolar|young mania|y-?bocs|obsessive[- ]compulsive|eating disorders?|scoff|psychosis|prodromal questionnaire|adhd|attention[- ]deficit|vanderbilt|asrs|conners|pediatric symptom checklist|psc-?(?:17|35)|strengths and difficulties|mental health|behaviou?ral health|psychiatric|psychological",
            OTHER,
        ),
    ]
    .into_iter()
    .map(|(words, q)| (Regex::new(&format!(r"(?i)\b(?:{words})\b")).unwrap(), q))
    .collect()
});

/// LOINC codes of the questionnaires and of what they ask.
const CODES: [(Questionnaire, &[&str]); 6] = [
    (
        POSTNATAL,
        &[
            "71354-5", "71348-7", "71349-5", "71350-3", "71351-1", "71352-9", "71353-7",
        ],
    ),
    (
        DEPRESSION,
        &[
            "44249-1", "44250-9", "44251-7", "44252-5", "44253-3", "44254-1", "44255-8", "44258-2",
            "44259-0", "44260-8", "44261-6", "55757-9", "55758-7", "69724-3", "89204-2",
        ],
    ),
    (SUICIDE, &["93373-9"]),
    (
        ANXIETY,
        &[
            "69737-5", "70274-6", "69725-0", "68509-9", "69733-4", "69734-2", "69735-9", "69689-8",
            "69736-7",
        ],
    ),
    (
        ALCOHOL,
        &[
            "72109-2", "75626-2", "72110-0", "75624-7", "68518-0", "68519-8", "68520-6",
        ],
    ),
    (TOBACCO, &["72166-2", "11367-0"]),
];

/// The questionnaire a result, a report or a paper is of, by its code, else by
/// its name or the code's.
pub fn of(name: &str, code: Option<&Code>) -> Option<Questionnaire> {
    let loinc = code.filter(|c| c.system.as_deref() == Some(LOINC));
    let coded = loinc.and_then(|c| c.code.as_deref()).and_then(|c| {
        let mut kinds = CODES.iter();
        kinds.find(|(_, codes)| codes.contains(&c)).map(|(q, _)| *q)
    });
    let names = std::iter::once(name).chain(code.and_then(|c| c.display.as_deref()));
    coded.or_else(|| {
        names
            .filter_map(|n| NAMES.iter().find(|(re, _)| re.is_match(n)))
            .map(|(_, q)| *q)
            .next()
    })
}

/// What a result or a report is told by: its name and code, then those of each
/// of its parts.
pub fn told_by(r: &Record) -> Vec<(String, Option<Code>)> {
    let parts = r.fields.get(COMPONENTS).and_then(Value::as_array);
    let code = |p: &Value| serde_json::from_value::<Code>(p.get("code")?.clone()).ok();
    let parts = parts.into_iter().flatten().filter_map(code);
    let parts = parts.map(|c| (c.display.clone().unwrap_or_default(), Some(c)));
    std::iter::once((r.display.clone(), r.code.clone()))
        .chain(parts)
        .collect()
}

/// The questionnaires that a result or a report is of: by what it is told by,
/// and those that list it.
pub fn asked_by(r: &Record) -> Vec<Questionnaire> {
    let told = told_by(r);
    let told = told
        .iter()
        .map(|(name, code)| (name.as_str(), code.as_ref()));
    let all = told.chain(crate::panels::asked_in(r));
    all.filter_map(|(name, code)| of(name, code)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kind(name: &str) -> Option<(&'static str, &'static str, Answers)> {
        of(name, None).map(|q| (q.kind, q.area, q.answers))
    }

    #[test]
    fn a_questionnaire_is_told_by_its_name() {
        use Answers::*;
        for (kind_is, area, answers, names) in [
            (
                "postnatal depression",
                MENTAL_HEALTH,
                Grown,
                &[
                    "Edinburgh Depression Scale",
                    "EPDS total",
                    "Postpartum depression screening",
                    "Maternal Depression Screen",
                    "Caregiver depression",
                ][..],
            ),
            (
                "depression",
                MENTAL_HEALTH,
                Grown,
                &[
                    "PHQ-9 total score",
                    "PHQ2",
                    "PHQ-9M Score",
                    "Patient Health Questionnaire",
                    "Beck Depression Inventory",
                    "Geriatric depression scale",
                    "Depression screening",
                    "Depression",
                    "Screening for depression",
                    "Little interest or pleasure in doing things",
                    "Thoughts of hurting yourself",
                ],
            ),
            (
                "depression",
                MENTAL_HEALTH,
                Any,
                &["Children's Depression Inventory", "CES-DC"],
            ),
            (
                "anxiety",
                MENTAL_HEALTH,
                Grown,
                &["GAD-7", "GAD2 score", "Generalized anxiety disorder 7 item"],
            ),
            (
                "anxiety",
                MENTAL_HEALTH,
                Any,
                &["Anxiety screening", "Spence score", "Child anxiety scale"],
            ),
            (
                "suicide risk",
                MENTAL_HEALTH,
                Any,
                &[
                    "Suicide risk",
                    "C-SSRS",
                    "Self-harm risk",
                    "Suicidal ideation",
                ],
            ),
            (
                "alcohol use",
                SUBSTANCE_USE,
                Grown,
                &[
                    "AUDIT-C total",
                    "Alcohol use",
                    "CAGE-AID",
                    "How often do you have a drink containing alcohol",
                    "Drinks per week",
                ],
            ),
            (
                "drug use",
                SUBSTANCE_USE,
                Grown,
                &[
                    "DAST-10",
                    "CRAFFT score",
                    "Illicit drug use",
                    "Substance use",
                ],
            ),
            (
                "tobacco use",
                SUBSTANCE_USE,
                Any,
                &[
                    "Tobacco smoking status",
                    "Smoker",
                    "Cigarettes per day",
                    "Vaping",
                    "Pack years",
                    "Cotinine, urine",
                ],
            ),
            (
                "other mental health",
                MENTAL_HEALTH,
                Any,
                &["PTSD checklist", "PCL-5", "ADHD rating scale", "PSC-17"],
            ),
        ] {
            for n in names {
                assert_eq!(kind(n), Some((kind_is, area, answers)), "{n}");
            }
        }
        for n in [
            "ST depression",
            "Respiratory depression",
            "Rib cage, two views",
            "GAD-65 antibody",
            "Mast cell count",
            "Drug screen, urine",
            "Alcohol, ethyl",
            "Vapor, inhaled",
            "Ages and stages, 24 months",
            "Autism screening at 18 months",
            "Hearing screening",
            "Hemoglobin",
            "Audit of the chart",
        ] {
            assert_eq!(kind(n), None, "{n}");
        }
    }

    #[test]
    fn a_questionnaire_is_told_by_its_code_first() {
        let code = |system: &str, code: &str, display: Option<&str>| Code {
            system: Some(system.into()),
            code: Some(code.into()),
            display: display.map(str::to_string),
        };
        let total = code(LOINC, "44261-6", None);
        assert_eq!(of("Total score", Some(&total)), Some(DEPRESSION));
        let postnatal = code(LOINC, "71354-5", Some("PHQ"));
        assert_eq!(of("Anxiety", Some(&postnatal)), Some(POSTNATAL));
        // The same digits in another system are another code.
        let local = code("urn:example:local", "44261-6", None);
        assert_eq!(of("Total score", Some(&local)), None);
        let named = code("urn:example:local", "1", Some("GAD-7 total"));
        assert_eq!(of("Total score", Some(&named)), Some(ANXIETY));
    }
}
