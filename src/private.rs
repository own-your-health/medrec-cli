//! Private items (`views/<pid>/private.json`, `medrec-private/v1`): a
//! person's records, documents, events and imaging studies about mental health,
//! reproductive and fertility care, sexual health or substance use, and
//! the medicines tied to them. They come from codes (ICD-10), the names of
//! the places and clinicians, the portal they came from, the medicines and
//! tests themselves, the words of a visit, a condition with no ICD-10 code, a
//! report, a result and a bill, and the household's own marks in `marks.json`.
//!
//! It is discretion, not access control: a reader keeps such an item off
//! shared screens and household-wide answers, and shows it on the person's
//! own pages. Anyone who can read the folder can read every item.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::LazyLock;

use pk_cli_core::CliError;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::clinicians;
use crate::events::Built;
use crate::given;
use crate::marks::{failed, Failed, Marks, LABELS, READ, READ_DECISIONS, RECORDS, WRITE};
use crate::model::{Document, Household, Person, PersonIndex, PersonText, Record};
use crate::normalize::COMPONENTS;
use crate::panels;
use crate::parts::{self, Parts};
use crate::questionnaires;
use crate::services;
use crate::standing;

pub const SCHEMA: &str = "medrec-private/v1";

pub const MENTAL_HEALTH: &str = "mental_health";
pub const REPRODUCTIVE: &str = "reproductive";
pub const SEXUAL_HEALTH: &str = "sexual_health";
pub const SUBSTANCE_USE: &str = "substance_use";
/// An item the household marked private that no rule does.
pub const HOUSEHOLD: &str = "household";

/// What the household marked on one item, over the rules.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Mark {
    pub id: String,
    /// A record (`r_…`), document (`d_…`) or event (`e_…`) of the person's.
    pub item: String,
    pub private: bool,
    pub at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request: Option<String>,
    /// An event's own members when it was marked: when the event's id
    /// changes (its anchor did), the mark follows them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub members: Vec<String>,
}

/// An event's own members, by record or document id: those without
/// `person` (another person's paper).
pub(crate) fn own_members(e: &Value) -> Vec<String> {
    e["members"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|m| m.get("person").is_none())
        .filter_map(|m| m["record"].as_str().or(m["document"].as_str()))
        .map(str::to_string)
        .collect()
}

/// Whether what the household said of an event (`item`, with the own members
/// it had then) is of the event `eid` with these: it names it, or it names an
/// event no longer there and shares a member with it.
pub(crate) fn is_of_event(
    item: &str,
    had: &[String],
    eid: &str,
    members: &[String],
    events: &BTreeSet<&str>,
) -> bool {
    item == eid
        || (item.starts_with("e_")
            && !events.contains(item)
            && had.iter().any(|x| members.contains(x)))
}

fn marks_event(m: &Mark, eid: &str, members: &[String], events: &BTreeSet<&str>) -> bool {
    is_of_event(&m.item, &m.members, eid, members, events)
}

/// Why an item is private: its area, and what said so.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Why {
    area: &'static str,
    from: &'static str,
    /// Only by what a source gave and no item shows ([`given`]).
    given: bool,
    /// By its clinician: what ties them to the area.
    learned: Option<clinicians::Learned>,
}

impl Why {
    fn of(area: &'static str, from: &'static str) -> Why {
        Why {
            area,
            from,
            given: false,
            learned: None,
        }
    }

    fn json(self) -> Value {
        let mut v = json!({"area": self.area, "from": self.from});
        if self.given {
            v["given"] = Value::Bool(true);
        }
        if let Some(word) = self.learned.and_then(clinicians::Learned::word) {
            v["learned"] = word.into();
        }
        v
    }
}

// ---------------------------------------------------------------- the rules

/// The area of an ICD-10 code.
pub(crate) fn icd10_area(code: &str) -> Option<&'static str> {
    let c: String = code
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .collect::<String>()
        .to_ascii_uppercase();
    let n: u32 = c.get(1..3)?.parse().ok()?;
    let sub = c.get(3..4);
    match c.as_bytes()[0] {
        b'F' if (10..=19).contains(&n) => Some(SUBSTANCE_USE),
        b'F' => Some(MENTAL_HEALTH),
        b'A' if (50..=64).contains(&n) => Some(SEXUAL_HEALTH),
        b'B' if n == 20 => Some(SEXUAL_HEALTH),
        b'O' if n <= 8 => Some(REPRODUCTIVE),
        b'O' if n == 99 && c.get(3..4) == Some("3") => match c.get(4..5) {
            Some("1" | "2" | "3") => Some(SUBSTANCE_USE),
            Some("4") => Some(MENTAL_HEALTH),
            _ => None,
        },
        b'N' if n == 46 || (96..=98).contains(&n) => Some(REPRODUCTIVE),
        b'R' if n == 45 && c.get(3..6) == Some("851") => Some(MENTAL_HEALTH),
        b'T' if n == 14 && c.get(3..5) == Some("91") => Some(MENTAL_HEALTH),
        b'Z' => match (n, sub) {
            (13, Some("3")) | (86, Some("5")) | (91, Some("5")) => Some(MENTAL_HEALTH),
            (81, Some("1" | "2" | "3" | "4")) => Some(SUBSTANCE_USE),
            (81, _) => Some(MENTAL_HEALTH),
            (21, _) => Some(SEXUAL_HEALTH),
            (11, Some("3" | "4")) | (20, Some("2" | "6")) | (72, Some("5")) => Some(SEXUAL_HEALTH),
            (29, Some("8")) if c.get(4..5) == Some("1") => Some(SEXUAL_HEALTH),
            (30..=32, _) | (33, Some("2")) => Some(REPRODUCTIVE),
            (79, Some("3")) | (97, Some("5")) => Some(REPRODUCTIVE),
            (71, Some("4" | "5" | "6")) | (72, Some("0")) => Some(SUBSTANCE_USE),
            (77, Some("2")) if c.get(4..5) == Some("2") => Some(SUBSTANCE_USE),
            (87, Some("8")) if c.get(4..6) == Some("91") => Some(SUBSTANCE_USE),
            _ => None,
        },
        _ => None,
    }
}

fn is_icd10(system: Option<&str>) -> bool {
    system.is_some_and(|s| {
        let s = s.to_ascii_lowercase();
        s.contains("icd-10") || s.contains("icd10")
    })
}

/// The words of a place's or clinician's name, credentials included, that
/// say what they treat. Substance use first: a name can say both.
static PLACES: LazyLock<Vec<(Regex, &'static str)>> = LazyLock::new(|| {
    [
        (
            r"addiction|substance (?:use|abuse)|detox\w*|chemical dependency|opioid treatment|methadone clinic|recovery center|lcadc|ladc|cadc|casac",
            SUBSTANCE_USE,
        ),
        (
            r"psychiatr\w*|psycholog\w*|behaviou?ral health|mental health|counsel(?:l)?ing|psychotherap\w*|pmhnp|psyd|lcsw|licsw|lmhc|lpcc?|lmft",
            MENTAL_HEALTH,
        ),
        (
            r"fertility|reproductive|ivf|family planning|andrology",
            REPRODUCTIVE,
        ),
        (r"sexual health|std clinic|sti clinic|hiv", SEXUAL_HEALTH),
    ]
    .into_iter()
    .map(|(words, area)| (Regex::new(&format!(r"(?i)\b(?:{words})\b")).unwrap(), area))
    .collect()
});

/// Medicines used for little else, by ingredient.
static MEDICINES: LazyLock<Vec<(Regex, &'static str)>> = LazyLock::new(|| {
    [
        (
            "buprenorphine|naltrexone|methadone|disulfiram|acamprosate|varenicline|nicotine",
            SUBSTANCE_USE,
        ),
        (
            "sertraline|fluoxetine|paroxetine|citalopram|escitalopram|fluvoxamine|venlafaxine|desvenlafaxine|duloxetine|bupropion|mirtazapine|trazodone|vortioxetine|vilazodone|nortriptyline|lithium|quetiapine|olanzapine|risperidone|aripiprazole|ziprasidone|lurasidone|clozapine|haloperidol|paliperidone|cariprazine|brexpiprazole|buspirone|alprazolam|lorazepam|methylphenidate|amphetamine|dextroamphetamine|lisdexamfetamine|atomoxetine|viloxazine|esketamine",
            MENTAL_HEALTH,
        ),
        (
            "clomiphene|follitropin|menotropins|urofollitropin|ganirelix|cetrorelix|choriogonadotropin|chorionic gonadotropin|mifepristone|ulipristal|levonorgestrel|etonogestrel|norgestimate|drospirenone|desogestrel|norethindrone",
            REPRODUCTIVE,
        ),
        (
            "emtricitabine|bictegravir|dolutegravir|cabotegravir|rilpivirine|darunavir|efavirenz|raltegravir|lenacapavir|sildenafil|tadalafil|vardenafil|avanafil",
            SEXUAL_HEALTH,
        ),
    ]
    .into_iter()
    .map(|(words, area)| (Regex::new(&format!(r"(?i)\b(?:{words})")).unwrap(), area))
    .collect()
});

/// The brands of those medicines.
static BRANDS: LazyLock<Vec<(Regex, &'static str)>> = LazyLock::new(|| {
    [
        (
            "suboxone|subutex|zubsolv|sublocade|brixadi|bunavail|probuphine|vivitrol|revia|methadose|dolophine|antabuse|campral|chantix|zyban|nicorette|nicoderm|nicotrol|habitrol",
            SUBSTANCE_USE,
        ),
        (
            "zoloft|prozac|sarafem|paxil|pexeva|celexa|lexapro|luvox|effexor|pristiq|khedezla|cymbalta|drizalma|irenka|wellbutrin|aplenzin|forfivo|budeprion|auvelity|remeron|desyrel|oleptro|trintellix|brintellix|viibryd|pamelor|aventyl|eskalith|lithobid|lithotabs|seroquel|zyprexa|symbyax|lybalvi|risperdal|perseris|uzedy|rykindo|abilify|aristada|geodon|latuda|clozaril|fazaclo|versacloz|haldol|invega|vraylar|rexulti|buspar|xanax|niravam|ativan|loreev|ritalin|concerta|metadate|methylin|daytrana|quillivant|quillichew|aptensio|cotempla|jornay|relexxii|adhansia|adderall|mydayis|evekeo|adzenys|dyanavel|dexedrine|zenzedi|procentra|xelstrym|vyvanse|strattera|qelbree|spravato",
            MENTAL_HEALTH,
        ),
        (
            "clomid|serophene|milophene|gonal-?f|follistim|menopur|bravelle|fyremadel|cetrotide|ovidrel|novarel|pregnyl|mifeprex|mirena|liletta|nexplanon|implanon|nuvaring|eluryng|haloette|safyral|slynd|ocella|gianvi|loryna|sprintec|tri-?sprintec|ortho tri-?cyclen|ortho-?cyclen|ortho-?novum|trinessa|mononessa|estarylla|previfem|desogen|reclipsen|mircette|cyclessa|seasonale|seasonique|loseasonique|ortho micronor|loestrin|junel|microgestin|taytulla",
            REPRODUCTIVE,
        ),
        (
            "truvada|descovy|biktarvy|tivicay|triumeq|juluca|dovato|apretude|cabenuva|vocabria|edurant|odefsey|complera|genvoya|stribild|symtuza|prezista|prezcobix|atripla|symfi|sustiva|isentress|sunlenca|yeztugo|viagra|cialis|levitra|staxyn|stendra",
            SEXUAL_HEALTH,
        ),
    ]
    .into_iter()
    .map(|(words, area)| (Regex::new(&format!(r"(?i)\b(?:{words})\b")).unwrap(), area))
    .collect()
});

/// The brands that are people's names too, or words: of what is a medicine, and
/// of no other text.
static NAMED_ALIKE: LazyLock<Vec<(Regex, &'static str)>> = LazyLock::new(|| {
    let words = "yasmin|yaz|beyaz|zarah|syeda|larin|amethyst|camila|errin|heather|jolivette|nora-be|nikki|portia|skyla|kyleena|ella|lutera|levora|apri|aviane|alesse|lessina|kariva|juleber|enskyce|vienva|plan b(?: one-?step)?|my way|next choice";
    let words = Regex::new(&format!(r"(?i)\b(?:{words})\b")).unwrap();
    vec![(words, REPRODUCTIVE)]
});

/// The area of a medicine, by its ingredient or its brand.
fn medicine(name: &str) -> Option<&'static str> {
    let lists = [&*MEDICINES, &*BRANDS, &*NAMED_ALIKE];
    lists.iter().find_map(|list| first_area(list, name))
}

/// What a condition is called that the areas' codes are of.
static CONDITIONS: LazyLock<Vec<(Regex, &'static str)>> = LazyLock::new(|| {
    let used = "alcohol|opioid|opiate|fentanyl|heroin|cannabis|cannabinoid|marijuana|cocaine|stimulant|psychostimulant|amphetamine|methamphetamine|sedative|hypnotic|anxiolytic|benzodiazepine|barbiturate|hallucinogen|inhalant|ketamine|kratom|polysubstance|substance";
    let of_use = [
        &format!(r"(?:{used}|drug)s?(?:[- ](?:type|use|related|induced))?[- ](?:use[rs]?|abus\w*|misus\w*|dependen\w*|disorders?|addiction|withdrawal|intoxication|problems?)"),
        &format!(r"(?:{used})s?[- ](?:related|induced)(?: \w+){{0,3}} disorders?"),
        &format!(r"(?:abuse|misuse|dependenc[ey]|withdrawal|intoxication|harmful (?:pattern of )?use) (?:of|on|from|by|with|due to) (?:\w+ )?(?:{used}|drug)s?"),
        r"alcoholi\w*|(?:problem|heavy|binge|excessive|hazardous) (?:alcohol )?drink(?:ers?|ing)|delirium tremens|korsakoff\w*",
        r"addict\w*|nicotine|tobacco|smok(?:es|ed|ers?|ing)|(?:second[- ]?hand|passive|environmental) smoke|cigarettes?|e-?cig\w*|vap(?:es?|ing)|snuff",
    ]
    .join("|");
    let sexual = [
        r"syphilis|syphilitic|gonorrh\w*|gonococc\w*|chlamydia\w*|trichomon\w*|mycoplasma genitalium|chancroid|lymphogranuloma|granuloma inguinale|donovanosis|condyloma\w*",
        r"(?:ano)?genital (?:\w+ )?(?:herpes\w*|hsv|warts?)|herpes\w* (?:\w+ ){0,3}(?:ano)?genital\w*|hsv-?2|herpes simplex (?:virus )?(?:type )?2",
        r"hiv|human immunodeficiency|acquired immun(?:e|o)?[- ]?deficien\w*|(?-i:AIDS)",
        r"sexually transmi\w*|sexual (?:mode of )?transmission|st[di]s?|venereal|high[- ]risk (?:hetero|homo|bi)?sexual|un(?:safe|protected) (?:sex|sexual|intercourse)",
        r"erectile|impotence|premature ejaculation|sexual (?:dysfunction|desire|aversion|arousal)",
    ]
    .join("|");
    let reproductive = [
        r"infertil\w*|subfertil\w*|fertility|sterility|anovulat\w*|azoosperm\w*|oligo\w*sperm\w*|sperm counts?|semen",
        r"(?:ectopic|tubal|molar|ovarian|abdominal|cervical|cornual|interstitial|heterotopic|anembryonic) pregnanc(?:y|ies)|hydatidiform|blighted ovum",
        r"miscarriages?|abort(?:ions?|us|ers?)|pregnancy loss(?:es)?|loss of pregnancy|terminations? of (?:\w+ )?pregnanc(?:y|ies)|pregnancy terminations?|retained products of conception",
        r"contracept\w*|birth control|intrauterine device|iud|sterili[sz]ation|vasectomy|tubal ligation|family planning|pregnancy (?:examination or )?tests?",
        r"procreat\w*|preconception\w*|in vitro|ivf|(?:artificial )?insemination|artificial fertili[sz]ation|assisted reproduct\w*|ovarian hyperstimulation|gestational carrier|surroga\w*",
    ]
    .join("|");
    let mental = [
        r"depressi(?:ons?|ve)|depressed mood|feeling depressed|^depressed$|dysphori\w*|(?:baby|maternity|postpartum) blues|bipolar|mania|manic|hypomani\w*|dysthymi\w*|cyclothymi\w*|mood(?: \[?\w+\]?){0,2} disorders?|affective (?:\w+ )?(?:disorders?|psychos[ei]s)",
        r"anxiety|anxious|panic|phobi\w*|agoraphobi\w*|obsessive|compulsive|ocd|hoarding|neuros[ei]s|neurotic|ptsd|post[- ]?traumatic stress|acute stress (?:disorders?|reactions?)|reactions? to severe stress|adjustment (?:disorders?|reactions?)|prolonged grief",
        r"psycho(?:s[ei]s|tic|logic\w*|log(?:y|ist)|genic|somatic|sexual|path\w*)|psychiatr\w*|schizo\w*|delusion\w*|paranoi\w*|catatoni\w*|dissociative|depersonali[sz]ation|pseudobulbar affect",
        r"mental(?:ly)?(?: and behaviou?ral)? (?:health|ill\w*|disorders?|retardation)|behaviou?r(?:al)? (?:health|disorders?|problems?)|emotional (?:disorders?|disturbances?)|personality disorders?|personality and behaviou?ral|conduct disorders?|oppositional|explosive disorders?|impulse(?:[- ]control)? disorders?|gambling|kleptomania|pyromania|trichotillomania|(?:excoriation|skin[- ]picking) disorders?|paraphili\w*|factitious|munchausen\w*",
        r"somatoform|somati[sz]ation|somatic symptom|conversion disorders?|hypochondri\w*|dysmorphi\w*|eating disorders?|anorexia nervosa|bulimi\w*|binge[- ]eating|avoidant[ /-]+restrictive|pica|suicid\w*|self[- ](?:harm\w*|injur\w*|inflicted)|(?:intentional|deliberate) (?:self[- ])?(?:\w+ )?(?:overdose|poisoning)",
        r"adhd|attention[- ]deficit|hyperactivity|hyperkinetic (?:disorders?|syndrome)|autis\w*|asperger\w*|rett'?s?|intellectual disab\w*|(?:neuro)?developmental (?:\w+ ){0,2}disorders?|global developmental delay|(?:speech|language)(?: and language)? (?:delay|disorders?|impairment)|stutter\w*|stammer\w*|fluency disorders?|learning (?:disorders?|disabilit\w*|difficult\w*)|dyslexia|dyscalculia|dyspraxia|tourette\w*|tic disorders?|(?:motor|vocal) tics?|encopresis|mutism|attachment disorders?",
        r"gender (?:dysphoria|identity|incongruen\w*)|transgender|transsexual\w*|sleep ?walking|somnambulism|(?:night|sleep) terrors?|nightmare disorders?|(?:primary|psychophysiologic\w*) insomnia|dementia|delirium|amnestic|post[- ]?concuss\w*",
    ]
    .join("|");
    [
        (of_use, SUBSTANCE_USE),
        (sexual, SEXUAL_HEALTH),
        (reproductive, REPRODUCTIVE),
        (mental, MENTAL_HEALTH),
    ]
    .into_iter()
    .map(|(words, area)| {
        (
            Regex::new(&format!(r"(?i)\b(?:{words})(?:\b|$)")).unwrap(),
            area,
        )
    })
    .collect()
});

/// What has the words of one and is none: what is lower than it was, a liver's
/// that drink does not explain, what one hears with, and what anyone is asked.
static OTHERWISE: LazyLock<Regex> = LazyLock::new(|| {
    let lower = "st|segment|respiratory|marrow|myocardial|cardiac|cns|nervous system";
    Regex::new(&format!(
        r"(?i)\b(?:(?:{lower}) depression|non[- ]alcoholic|hearing[- ]aids?)\b|^gender identity$"
    ))
    .unwrap()
});

/// The area of a condition by what it is called.
fn condition_name(text: &str) -> Option<&'static str> {
    first_area(&CONDITIONS, &OTHERWISE.replace_all(text, " "))
}

/// The area of a paper by its title.
fn title(text: &str) -> Option<&'static str> {
    first_area(&PAPERS, text).or_else(|| questionnaires::of(text, None).map(|q| q.area))
}

/// The area of what a visit, a result or a report is called.
fn named(text: &str) -> Option<&'static str> {
    title(text).or_else(|| condition_name(text))
}

/// Tests that are about the areas themselves.
static TESTS: LazyLock<Vec<(Regex, &'static str)>> = LazyLock::new(|| {
    [
        (
            r"chlamydia|gonorrh\w*|trichomonas|syphilis|treponema|rpr|hiv|hsv-?2|herpes simplex (?:virus )?(?:type )?2",
            SEXUAL_HEALTH,
        ),
        (
            r"pregnancy test|hcg|anti-m[uü]llerian|amh|semen|sperm",
            REPRODUCTIVE,
        ),
        (
            r"drug screen|drugs of abuse|urine drug|toxicology screen|cannabinoids?|opiates?|fentanyl|ethyl glucuronide",
            SUBSTANCE_USE,
        ),
    ]
    .into_iter()
    .map(|(words, area)| (Regex::new(&format!(r"(?i)\b(?:{words})\b")).unwrap(), area))
    .collect()
});

/// The words of a title, or of what a bill or receipt charges for, that say
/// an area. Fewer of a place's, since any hospital's bill offers financial
/// counseling, and the services' own.
static PAPERS: LazyLock<Vec<(Regex, &'static str)>> = LazyLock::new(|| {
    [
        (
            r"addiction|substance (?:use|abuse)|detox\w*|chemical dependency|opioid treatment|methadone",
            SUBSTANCE_USE,
        ),
        (
            r"psychiatr\w*|psycholog\w*|behaviou?ral health|mental health|psychotherap\w*|pmhnp|psyd|lcsw|licsw|lmhc|lmft",
            MENTAL_HEALTH,
        ),
        (
            r"fertility|reproductive|ivf|in vitro|embryos?|embryology|oocytes?|egg (?:freezing|retrieval|storage|donation)|cryo-?preserv\w*|cryo-?storage|insemination|iui|andrology|family planning|contracept\w*|vasectomy",
            REPRODUCTIVE,
        ),
        (
            r"sexual health|(?:std|sti) (?:clinic|panel|screen\w*|test\w*)|hiv",
            SEXUAL_HEALTH,
        ),
    ]
    .into_iter()
    .map(|(words, area)| (Regex::new(&format!(r"(?i)\b(?:{words})\b")).unwrap(), area))
    .collect()
});

/// The texts that are the values of a reading's parts.
fn parts_say(r: &Record) -> impl Iterator<Item = &str> {
    let parts = r.fields.get(COMPONENTS).and_then(Value::as_array);
    let values = parts.into_iter().flatten();
    values.filter_map(|p| p.get("value")?.as_str())
}

/// What a record's own words say: the part of it each is, and its area. A
/// condition's name says it where no ICD-10 code of it does, a visit's name
/// and its reason, a report's conclusion, and a result's value that is a text.
pub(crate) fn said(r: &Record) -> Vec<(&'static str, &'static str)> {
    let of = |part: &'static str, text: Option<&str>, area: fn(&str) -> Option<&'static str>| {
        Some((part, area(text?)?))
    };
    let name = Some(r.display.as_str());
    let each = match r.kind.as_str() {
        "condition" => [
            of(
                "display",
                name.filter(|_| icd10(r).is_none()),
                condition_name,
            ),
            None,
        ],
        "encounter" => [
            of("display", name, named),
            of("reason", field(r, "reason"), named),
        ],
        "report" => [of("conclusion", field(r, "conclusion"), words), None],
        "result" => [of("value", field(r, "value"), words), None],
        _ => [None, None],
    };
    each.into_iter().flatten().collect()
}

/// A condition's ICD-10 code.
fn icd10(r: &Record) -> Option<&str> {
    let code = r.code.as_ref();
    let code = code.filter(|c| is_icd10(c.system.as_deref()))?;
    code.code.as_deref()
}

/// The area any words of a text say: of a service, a medicine, a test or a
/// questionnaire.
pub(crate) fn words(text: &str) -> Option<&'static str> {
    page(text).or_else(|| questionnaires::of(text, None).map(|q| q.area))
}

/// The same of a bill's page, which a questionnaire's name is one line of.
fn page(text: &str) -> Option<&'static str> {
    let lists = [&*PAPERS, &*MEDICINES, &*BRANDS, &*TESTS];
    lists.iter().find_map(|list| first_area(list, text))
}

/// The area of one of a claim's diagnoses, by its code.
pub(crate) fn diagnosis(d: &Value) -> Option<&'static str> {
    let code = d.get("code")?;
    let text = |k: &str| code.get(k).and_then(Value::as_str);
    text("code")
        .filter(|_| is_icd10(text("system")))
        .and_then(icd10_area)
}

fn first_area(list: &[(Regex, &'static str)], text: &str) -> Option<&'static str> {
    list.iter()
        .find(|(re, _)| re.is_match(text))
        .map(|(_, a)| *a)
}

/// The area a place's or clinician's name, or a role, says.
pub(crate) fn place_area(text: &str) -> Option<&'static str> {
    first_area(&PLACES, text)
}

/// The portals whose records are all of one area: a behavioral health
/// record system, a fertility clinic's.
fn source_area(r: &Record) -> Option<&'static str> {
    r.provenance
        .iter()
        .find_map(|p| match p.source.cli.as_deref() {
            Some("icanotes") => Some(MENTAL_HEALTH),
            Some("fertemr") => Some(REPRODUCTIVE),
            _ => None,
        })
}

fn field<'a>(r: &'a Record, k: &str) -> Option<&'a str> {
    r.fields.get(k).and_then(Value::as_str)
}

/// A claim's principal diagnosis: the lowest sequence, else the first.
fn principal_diagnosis(r: &Record) -> Option<(Option<&str>, &str)> {
    let list = r.fields.get("diagnoses")?.as_array()?;
    let d = list.iter().min_by_key(|d| {
        d.get("sequence")
            .and_then(Value::as_u64)
            .unwrap_or(u64::MAX)
    })?;
    let code = d.get("code")?;
    Some((
        code.get("system").and_then(Value::as_str),
        code.get("code").and_then(Value::as_str)?,
    ))
}

/// What a pharmacy claim names, without its provider.
pub(crate) fn dispensed(r: &Record) -> Option<&str> {
    let pharmacy = ["category", "type"]
        .iter()
        .any(|k| field(r, k) == Some("pharmacy"));
    pharmacy.then(|| r.display.split(" · ").next()).flatten()
}

/// Why a record is private by the rules, if it is.
fn record_why(r: &Record) -> Option<Why> {
    if let Some(area) = source_area(r) {
        return Some(Why::of(area, "source"));
    }
    let by_names = |keys: &[&str]| {
        keys.iter()
            .filter_map(|k| field(r, k))
            .find_map(|n| first_area(&PLACES, n))
            .map(|area| Why::of(area, "provider"))
    };
    let said_area = || said(r).first().map(|(_, area)| *area);
    match r.kind.as_str() {
        "condition" => {
            let code_name = r.code.as_ref().and_then(|c| c.display.as_deref());
            let code_name = code_name.filter(|_| icd10(r).is_none());
            let by_name = || said_area().or_else(|| condition_name(code_name?));
            icd10(r)
                .and_then(icd10_area)
                .map(|area| Why::of(area, "code"))
                .or_else(|| Some(Why::of(by_name()?, "words")))
        }
        "claim" => principal_diagnosis(r)
            .filter(|(system, _)| is_icd10(*system))
            .and_then(|(_, code)| icd10_area(code))
            .map(|area| Why::of(area, "code"))
            .or_else(|| {
                let area = medicine(dispensed(r)?)?;
                Some(Why::of(area, "medicine"))
            })
            .or_else(|| by_names(&["provider", "facility"]))
            .or_else(|| services::claim(r).map(|area| Why::of(area, "service"))),
        "procedure" => services::procedure(r)
            .map(|area| Why::of(area, "service"))
            .or_else(|| by_names(&["performer", "location"])),
        "encounter" => by_names(&["service_provider", "location", "practitioner"])
            .or_else(|| Some(Why::of(said_area()?, "words"))),
        "medication" => std::iter::once(r.display.as_str())
            .chain(r.code.as_ref().and_then(|c| c.display.as_deref()))
            .find_map(medicine)
            .map(|area| Why::of(area, "medicine"))
            .or_else(|| by_names(&["prescriber"])),
        // A reading is, by any of its parts.
        "result" | "report" => questionnaires::told_by(r)
            .iter()
            .find_map(|(name, _)| first_area(&TESTS, name).or_else(|| named(name)))
            .or_else(|| questionnaires::asked_by(r).first().map(|q| q.area))
            .or_else(|| parts_say(r).find_map(words))
            .or_else(said_area)
            .map(|area| Why::of(area, "test"))
            .or_else(|| by_names(&["performer"])),
        _ => None,
    }
}

/// Why a document is private by the rules, if it is: its organization, its
/// title or the name its file came with, or what a bill or receipt says on its
/// first page with any text.
fn document_why(d: &Document, text: Option<&str>) -> Option<Why> {
    let place = d
        .organization
        .as_deref()
        .and_then(|o| first_area(&PLACES, o))
        .map(|area| Why::of(area, "provider"));
    let words = || {
        let titled = d.title.as_deref().and_then(title).or_else(|| {
            let name = d.original_name.as_deref()?;
            [&['_', '.'][..], &['_', '.', '-']]
                .iter()
                .find_map(|marks| title(&name.replace(*marks, " ")))
        });
        let first = || page(&crate::bill::first_page(d, text?)?);
        titled.or_else(first).map(|area| Why::of(area, "words"))
    };
    place.or_else(words)
}

/// The pairs of documents that tell of one another as copies. So do those
/// [`crate::bills::ties`] ties.
fn papers_tied(idx: &PersonIndex) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for d in &idx.documents {
        let copies = d
            .near_duplicates
            .iter()
            .map(|n| n.document.as_str())
            .chain(d.same_statement_as.as_deref());
        out.extend(copies.map(|c| (d.id.clone(), c.to_string())));
    }
    out
}

// ---------------------------------------------------------------- the view

/// The person's `private.json` as `medrec views write` writes it: from
/// their index and its texts, their events (`events::Built`), their marks,
/// and the items a provider label makes private, with their area
/// (`labels::areas`). What a source takes
/// back still tells what it did, so nothing it made private opens.
pub fn build(
    p: &Person,
    idx: &PersonIndex,
    texts: &PersonText,
    built: &Built,
    all_marks: &Marks,
    labelled: &BTreeMap<String, (&'static str, bool)>,
) -> Value {
    let marks = all_marks.private.as_slice();
    let now_index = standing::index(idx);
    let events = &*built.for_private();
    // A study is private when its report is. One with no report, or a report
    // the person does not have, cannot be judged, with what is entered in
    // error or without it.
    let mut unreported = Vec::new();
    let mut reports: Vec<(&str, [Option<String>; 2])> = Vec::new();
    for (s, now) in idx.studies.iter().zip(&now_index.studies) {
        let report = |of: &PersonIndex, report: &Option<String>| -> Option<String> {
            let has = |r: &&String| {
                of.documents.iter().any(|d| d.id == **r) || of.records.iter().any(|x| x.id == **r)
            };
            report.as_ref().filter(has).cloned()
        };
        let of = [report(idx, &s.report), report(&now_index, &now.report)];
        if of.contains(&None) {
            unreported.push(s.id.clone());
        }
        reports.push((&s.id, of));
    }
    // A document tied to a private one is private.
    let mut ties = papers_tied(idx);
    ties.extend(crate::bills::ties(&crate::bills::build(
        p,
        idx,
        texts,
        &built.events,
        &[],
    )));
    if let Some(whole) = &built.whole {
        ties.extend(crate::bills::whole_ties(p, idx, texts, whole));
    }
    let clinicians = clinicians::Clinicians::of(idx, &all_marks.clinicians);
    let judge = |given: bool| {
        let mut items: BTreeMap<String, Why> = BTreeMap::new();
        let label = |id: &str| {
            let (area, only) = labelled.get(id)?;
            (given || !only).then(|| Why::of(area, "provider"))
        };
        for r in &idx.records {
            let gave = || given::as_given(r).find_map(|x| record_why(&x));
            let w = record_why(r).or_else(|| gave().filter(|_| given));
            let w = w.or_else(|| label(&r.id));
            let named = || {
                let n = clinicians.named(r, given)?;
                let learned = Some(n.learned);
                Some(Why {
                    learned,
                    ..Why::of(n.area, "clinician")
                })
            };
            let w = w.or_else(named);
            items.extend(w.map(|w| (r.id.clone(), w)));
        }
        for d in &idx.documents {
            let text = texts.texts.get(&d.id).map(String::as_str);
            let gave = || given::papers_as_given(d).find_map(|x| document_why(&x, None));
            let w = document_why(d, text).or_else(|| gave().filter(|_| given));
            let w = w.or_else(|| label(&d.id));
            items.extend(w.map(|w| (d.id.clone(), w)));
        }
        follow(&mut items, idx, events, &ties, marks, given, label);
        for (study, of) in &reports {
            let mut why = of.iter().flatten();
            if let Some(w) = why.find_map(|r| items.get(r).copied()) {
                items.insert(study.to_string(), Why::of(w.area, "report"));
            }
        }
        items
    };
    let mut items = judge(true);
    let gave = idx.records.iter().any(|r| !r.given.is_empty())
        || idx.documents.iter().any(|d| !d.given.is_empty())
        || labelled.values().any(|(_, only)| *only);
    if gave {
        // As they are by what is shown; else only by what a source gave.
        let shown = judge(false);
        for (id, w) in &mut items {
            *w = match shown.get(id) {
                // What a source gave can tie its clinician more surely.
                Some(s) => Why {
                    learned: match (s.learned, w.learned) {
                        (Some(a), Some(b)) => Some(a.min(b)),
                        (a, _) => a,
                    },
                    ..*s
                },
                None => Why { given: true, ..*w },
            };
        }
    }

    let mut out = json!({"schema": SCHEMA, "person": p.id,
        "records": {}, "documents": {}, "events": {}, "studies": {}});
    let heads = idx.records.iter().filter(|r| crate::panels::heads(r));
    let heads: BTreeSet<&str> = heads.map(|r| r.id.as_str()).collect();
    for (id, w) in items {
        let kind = match id.get(..2) {
            Some("r_") => "records",
            Some("d_") => "documents",
            Some("e_") => "events",
            Some("m_") => "studies",
            _ => continue,
        };
        let mut v = w.json();
        if heads.contains(id.as_str()) {
            v["heading"] = true.into();
        }
        out[kind][id] = v;
    }
    if !unreported.is_empty() {
        out["unreported_studies"] = unreported.into();
    }
    let parts = parts_of(p, idx, &now_index, texts, built, marks, &out);
    out["parts"] = Value::Object(parts);
    let clear = clear_events(built, &out);
    if !clear.is_empty() {
        out["clear_events"] = clear.into();
    }
    out
}

/// The events another person's link to ([`crate::events`], X1) that are open
/// and have no private member of their own: the place of one is in the other's.
fn clear_events(built: &Built, private: &Value) -> Vec<String> {
    let listed = built.events["events"].as_array().into_iter().flatten();
    let linked = listed.filter(|e| e["links"].as_array().is_some_and(|l| !l.is_empty()));
    let clear = linked.filter(|e| {
        let mut ids = own_members(e);
        ids.extend(e["id"].as_str().map(str::to_string));
        let kinds = ["records", "documents", "events"];
        !ids.iter()
            .any(|id| kinds.iter().any(|k| private[*k].get(id).is_some()))
    });
    clear
        .filter_map(|e| e["id"].as_str().map(str::to_string))
        .collect()
}

/// What is left out of each open item that has a private part
/// ([`crate::parts`]), of one the household marked not private nothing. An
/// event is what its open items say.
fn parts_of(
    p: &Person,
    idx: &PersonIndex,
    now: &PersonIndex,
    texts: &PersonText,
    built: &Built,
    marks: &[Mark],
    private: &Value,
) -> serde_json::Map<String, Value> {
    let mark_of: BTreeMap<&str, bool> =
        marks.iter().map(|m| (m.item.as_str(), m.private)).collect();
    let held = |id: &str| {
        let kinds = ["records", "documents", "events"];
        kinds.iter().any(|k| private[*k].get(id).is_some())
    };
    let judged = |id: &str| !held(id) && mark_of.get(id) != Some(&false);
    let mut out = serde_json::Map::new();
    let mut claims: BTreeMap<String, Record> = BTreeMap::new();
    let mut services: BTreeMap<String, usize> = BTreeMap::new();
    for r in idx.records.iter().filter(|r| r.kind == "claim") {
        let of = parts::OfClaim::of(r);
        if !of.is_empty() && judged(&r.id) {
            out.insert(r.id.clone(), of.json(r));
            claims.insert(r.id.clone(), of.without(r));
            services.insert(r.id.clone(), of.lines.len());
        }
    }
    for d in idx.documents.iter().filter(|d| judged(&d.id)) {
        let text = texts.texts.get(&d.id).filter(|_| crate::bill::charges(d));
        let places = text.map(|t| parts::of_text(t)).unwrap_or_default();
        if !places.is_empty() {
            out.insert(d.id.clone(), parts::text_json(&places));
        }
    }
    for e in built.open(&p.id, now, idx, &|id| !held(id), &claims) {
        let services = e.items.iter().filter_map(|id| services.get(id)).sum();
        let mut said = e.said;
        if services > 0 {
            said.insert("says".into(), parts::services_not_shown(services).into());
        }
        if !said.is_empty() && !held(&e.id) {
            out.insert(e.id, Value::Object(said));
        }
    }
    out
}

/// What is private with what is: by the household's marks, the records and
/// papers tied to it, and events. `given`: by what a panel listed before too.
fn follow(
    items: &mut BTreeMap<String, Why>,
    idx: &PersonIndex,
    events: &Value,
    ties: &[(String, String)],
    marks: &[Mark],
    given: bool,
    by_label: impl Fn(&str) -> Option<Why>,
) {
    // The household's marks on records, then the documents a private record
    // links, then the marks on documents: an event follows what its members
    // are.
    let mark_of: BTreeMap<&str, bool> =
        marks.iter().map(|m| (m.item.as_str(), m.private)).collect();
    let household = Why::of(HOUSEHOLD, "household");
    let apply = |items: &mut BTreeMap<String, Why>, id: &str, mark: Option<bool>| match mark {
        Some(true) => {
            let area = items.get(id).map_or(HOUSEHOLD, |w| w.area);
            items.insert(id.to_string(), Why { area, ..household });
        }
        Some(false) => {
            items.remove(id);
        }
        None => {}
    };
    let event_ids: BTreeSet<&str> = events["events"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|e| e["id"].as_str())
        .collect();
    // A document the person has that any private record links is private,
    // unless the household marked it not.
    let documents: BTreeSet<&str> = idx.documents.iter().map(|d| d.id.as_str()).collect();
    let link = |items: &mut BTreeMap<String, Why>| {
        let linked: Vec<(String, Why)> = idx
            .records
            .iter()
            .filter_map(|r| items.get(&r.id).map(|w| (r, *w)))
            .flat_map(|(r, w)| r.documents.iter().map(move |d| (d.clone(), w)))
            .filter(|(d, _)| documents.contains(d.as_str()))
            .filter(|(d, _)| mark_of.get(d.as_str()) != Some(&false))
            .collect();
        for (d, w) in linked {
            items.entry(d).or_insert(Why::of(w.area, "record"));
        }
    };
    // So is one tied to a private one.
    let tie = |items: &mut BTreeMap<String, Why>| loop {
        let tied: Vec<(String, Why)> = ties
            .iter()
            .flat_map(|(a, b)| [(a, b), (b, a)])
            .filter_map(|(from, to)| Some((to.clone(), *items.get(from)?)))
            .filter(|(d, _)| documents.contains(d.as_str()) && !items.contains_key(d))
            .filter(|(d, _)| mark_of.get(d.as_str()) != Some(&false))
            .collect();
        if tied.is_empty() {
            return;
        }
        for (d, w) in tied {
            items.entry(d).or_insert(Why::of(w.area, "paper"));
        }
    };
    // A mark naming an item the person no longer has is skipped.
    for r in &idx.records {
        apply(items, &r.id, mark_of.get(r.id.as_str()).copied());
    }
    // A panel or a report one list of which has none but private results, with
    // those entered in error or without them. To any depth.
    let in_error = idx.records.iter().filter(|r| standing::record_in_error(r));
    let in_error: BTreeSet<&str> = in_error.map(|r| r.id.as_str()).collect();
    let panels: Vec<(&str, Vec<panels::List>)> = panels::every_list(&idx.records)
        .into_iter()
        .map(|(i, lists)| (idx.records[i].id.as_str(), lists))
        .filter(|(id, _)| mark_of.get(id) != Some(&false))
        .collect();
    let panel = |items: &mut BTreeMap<String, Why>| loop {
        let before = items.len();
        for (id, lists) in &panels {
            let private = |id: &str| items.contains_key(id);
            let mut lists = lists.iter().filter(|l| given || l.now);
            let list = lists.find(|l| panels::all(&l.ids, &in_error, &private));
            let first = list.and_then(|l| l.ids.iter().find_map(|id| items.get(*id)));
            let why = first.map(|w| Why::of(w.area, "members"));
            if let Some(w) = why.filter(|_| !items.contains_key(*id)) {
                items.insert(id.to_string(), w);
            }
        }
        if items.len() == before {
            break;
        }
    };
    panel(items);
    link(items);
    for d in &idx.documents {
        apply(items, &d.id, mark_of.get(d.id.as_str()).copied());
    }

    // An event is private when a claim or portal visit in it is, or every
    // record and paper in it is; then so is all of it, and what it makes
    // private can link more. Until nothing changes.
    loop {
        let before = items.len();
        let mut by_event: Vec<(String, Vec<String>)> = Vec::new();
        for e in events["events"].as_array().into_iter().flatten() {
            let Some(eid) = e["id"].as_str() else {
                continue;
            };
            let ids = own_members(e);
            let lead = e["members"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|m| m.get("person").is_none())
                .filter(|m| m["kind"] == "claim" || m["role"] == "portal")
                .filter_map(|m| m["record"].as_str().or(m["document"].as_str()))
                .find_map(|id| items.get(id).copied());
            let all = (!ids.is_empty() && ids.iter().all(|id| items.contains_key(id)))
                .then(|| items[&ids[0]]);
            if let Some(w) = lead.or(all) {
                items.insert(eid.to_string(), Why::of(w.area, "members"));
            }
            // Where it was, as the household calls a provider no member of
            // it prints.
            if let Some(w) = by_label(eid).filter(|_| !items.contains_key(eid)) {
                items.insert(eid.to_string(), w);
            }
            // The last mark that applies: its own, else one that followed
            // its members here.
            let mark = marks
                .iter()
                .rev()
                .find(|m| m.item == eid)
                .or_else(|| {
                    marks
                        .iter()
                        .rev()
                        .find(|m| marks_event(m, eid, &ids, &event_ids))
                })
                .map(|m| m.private);
            apply(items, eid, mark);
            by_event.push((eid.to_string(), ids));
        }
        for (eid, ids) in &by_event {
            let Some(w) = items.get(eid).copied() else {
                continue;
            };
            for id in ids {
                if !items.contains_key(id) && mark_of.get(id.as_str()) != Some(&false) {
                    items.insert(id.clone(), Why::of(w.area, "event"));
                }
            }
        }
        panel(items);
        link(items);
        tie(items);
        if items.len() == before {
            break;
        }
    }
}

/// The person's `private.json` as `medrec views write` would write it now.
pub fn of_person(
    folder: &crate::folder::Folder,
    h: &Household,
    p: &Person,
) -> Result<Value, CliError> {
    let idx = folder.index(p)?;
    Ok(with_events(folder, h, p, &idx)?.0)
}

/// The person's `private.json` now, and the events it was made from.
pub fn with_events(
    folder: &crate::folder::Folder,
    h: &Household,
    p: &Person,
    idx: &PersonIndex,
) -> Result<(Value, Built), CliError> {
    let events = crate::events::both(folder, h, p)?;
    let marks = folder.marks_if_readable(p)?;
    // As the last pass left it: what is private is the same on a machine
    // with the directory and on one without it.
    let state = crate::viewfiles::provider_state(folder, p)?;
    let none = Marks::new(&p.id);
    let said = marks.as_ref().unwrap_or(&none);
    let mut v = judged(p, idx, &folder.text(p)?, &events, said, &state);
    if state.lost() {
        v["labels_unreadable"] = Value::Bool(true);
    }
    if marks.is_none() || state.lost() {
        v["marks_unreadable"] = Value::Bool(true);
    }
    Ok((v, events))
}

/// [`build`], with the items a label makes private by what the marks say of
/// providers.
fn judged(
    p: &Person,
    idx: &PersonIndex,
    texts: &PersonText,
    events: &Built,
    marks: &Marks,
    state: &crate::labels::State,
) -> Value {
    let areas = crate::labels::areas(idx, events, state, &marks.providers);
    build(p, idx, texts, events, marks, &areas)
}

/// What a request would change of what the household decided.
#[derive(Clone, Copy)]
pub enum Change<'a> {
    Events(&'a crate::events::Overrides),
    Marks(&'a Marks),
}

/// A person's items that are private other than by a mark of the
/// household's, and the members of each event.
struct Ruled {
    items: BTreeSet<String>,
    parts: Parts,
    events: Vec<(String, Vec<String>)>,
}

/// Whether a change would open something: whether what is private now, and
/// not only by the household's own marks, would no longer be. What they
/// marked private is judged by the rules alone, so that no request opens it
/// in two steps; what they marked not private is open. Of an event that is
/// private, each that would have a private member of it must be.
pub fn opens(
    folder: &crate::folder::Folder,
    h: &Household,
    p: &Person,
    change: Change,
) -> Result<bool, Failed> {
    let idx = folder.index(p).map_err(failed(RECORDS))?;
    let texts = folder.text(p).map_err(failed(RECORDS))?;
    let overrides = folder.overrides(p).map_err(failed(READ_DECISIONS))?;
    let marks = folder.marks(p).map_err(failed(READ))?;
    let state = known_labels(folder, p)?;
    let ruled = |overrides: &crate::events::Overrides, marks: &Marks| -> Result<Ruled, Failed> {
        let built =
            crate::events::both_by(folder, h, p, Some(overrides)).map_err(failed(RECORDS))?;
        let mut marks = marks.clone();
        marks.private.retain(|m| !m.private);
        let v = judged(p, &idx, &texts, &built, &marks, &state);
        let events = built.for_private();
        let listed = events["events"].as_array().into_iter().flatten();
        let id = |e: &Value| e["id"].as_str().unwrap_or_default().to_string();
        Ok(Ruled {
            parts: Parts::of(&v),
            items: Private::of(&v).items.into_keys().collect(),
            events: listed.map(|e| (id(e), own_members(e))).collect(),
        })
    };
    let now = ruled(&overrides, &marks)?;
    let then = match change {
        Change::Events(o) => ruled(o, &marks)?,
        Change::Marks(m) => ruled(&overrides, m)?,
    };
    let is_event = |id: &str| id.starts_with("e_");
    let mut items = now.items.iter().filter(|id| !is_event(id));
    if items.any(|id| !then.items.contains(id)) {
        return Ok(true);
    }
    let mut parts = now.parts.ids().filter(|id| !is_event(id));
    if parts.any(|id| !then.items.contains(id) && !then.parts.leave_out(id, &now.parts)) {
        return Ok(true);
    }
    let held = |(id, members): &(String, Vec<String>)| -> Vec<String> {
        let private = members.iter().filter(|m| now.items.contains(*m));
        match now.items.contains(id) {
            true => private.cloned().collect(),
            false => Vec::new(),
        }
    };
    let held: BTreeSet<String> = now.events.iter().flat_map(held).collect();
    let mut after = then.events.iter();
    Ok(after
        .any(|(id, members)| !then.items.contains(id) && members.iter().any(|m| held.contains(m))))
}

/// A person's private items by id, with their area.
#[derive(Debug, Clone, Default)]
pub struct Private {
    pub items: BTreeMap<String, String>,
    /// Studies no report judges: held back from household-wide answers.
    pub unreported_studies: Vec<String>,
}

impl Private {
    /// From a `private.json`.
    pub fn of(v: &Value) -> Private {
        let mut items = BTreeMap::new();
        for kind in ["records", "documents", "events", "studies"] {
            for (id, w) in v[kind].as_object().into_iter().flatten() {
                if let Some(a) = w["area"].as_str() {
                    items.insert(id.clone(), a.to_string());
                }
            }
        }
        let unreported_studies = v["unreported_studies"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|x| x.as_str().map(str::to_string))
            .collect();
        Private {
            items,
            unreported_studies,
        }
    }

    /// The area of a private item.
    pub fn area(&self, id: &str) -> Option<&str> {
        self.items.get(id).map(String::as_str)
    }
}

/// The people holding an item: a record, document or event id.
pub fn holder<'a>(
    folder: &crate::folder::Folder,
    h: &'a Household,
    item: &str,
) -> Result<Option<&'a Person>, CliError> {
    for p in &h.people {
        if item.starts_with("e_") {
            let events = crate::events::of_person(folder, h, p)?;
            if events["events"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|e| e["id"] == item)
            {
                return Ok(Some(p));
            }
        } else {
            let idx = folder.index(p)?;
            if idx.records.iter().any(|r| r.id == item)
                || idx.documents.iter().any(|d| d.id == item)
            {
                return Ok(Some(p));
            }
        }
    }
    Ok(None)
}

/// Mark an item of the person's private (`Some(true)`), not private
/// (`Some(false)`), or back to the rules (`None`). The mark, and whether
/// anything changed. A `request` never marks an item not private: it drops
/// the mark that it is, and is refused where the item is private without it.
pub fn mark(
    folder: &crate::folder::Folder,
    h: &Household,
    p: &Person,
    item: &str,
    private: Option<bool>,
    by: Option<&str>,
    request: Option<&str>,
) -> Result<(Option<Mark>, bool), Failed> {
    if !["r", "d", "e"]
        .iter()
        .any(|k| crate::ids::is_valid(k, item))
    {
        return Err(Failed::Refused(CliError::Usage(format!(
            "{} is not a record, document or event id",
            crate::printable(item)
        ))));
    }
    // An event's own members, and the person's events now.
    let (known, members, events) = if item.starts_with("e_") {
        let v = crate::events::of_person(folder, h, p).map_err(failed(RECORDS))?;
        let list = v["events"].as_array().cloned().unwrap_or_default();
        let members = list.iter().find(|e| e["id"] == item).map(own_members);
        let ids: BTreeSet<String> = list
            .iter()
            .filter_map(|e| e["id"].as_str().map(str::to_string))
            .collect();
        (members.is_some(), members.unwrap_or_default(), ids)
    } else {
        let idx = folder.index(p).map_err(failed(RECORDS))?;
        let known =
            idx.records.iter().any(|r| r.id == item) || idx.documents.iter().any(|d| d.id == item);
        (known, Vec::new(), BTreeSet::new())
    };
    if !known {
        return Err(Failed::Refused(CliError::NotFound(format!(
            "{} is not one of {}'s records, documents or events",
            crate::printable(item),
            crate::printable(&p.name)
        ))));
    }
    let mut m = folder.marks(p).map_err(failed(READ))?;
    // The marks that apply to the item: its own, and for an event those
    // that followed its members from an id it no longer has.
    let events: BTreeSet<&str> = events.iter().map(String::as_str).collect();
    let applies = |x: &Mark| {
        if item.starts_with("e_") {
            marks_event(x, item, &members, &events)
        } else {
            x.item == item
        }
    };
    let found: Vec<usize> = (0..m.private.len())
        .filter(|i| applies(&m.private[*i]))
        .collect();
    let private = match (private, request) {
        (Some(false), Some(_)) => {
            let own = found.iter().rev().find(|i| m.private[**i].item == item);
            let counts = own.or(found.last()).map(|i| &m.private[*i]);
            if let Some(open) = counts.filter(|x| !x.private) {
                return Ok((Some(open.clone()), false));
            }
            if private_without(folder, h, p, item, &m, &found)? {
                return Err(Failed::KeptPrivate);
            }
            None
        }
        _ => private,
    };
    match (private, found.as_slice()) {
        (None, []) => return Ok((None, false)),
        (Some(b), [i]) if m.private[*i].item == item && m.private[*i].private == b => {
            return Ok((Some(m.private[*i].clone()), false))
        }
        _ => {}
    }
    let changed = !found.is_empty();
    m.private = std::mem::take(&mut m.private)
        .into_iter()
        .enumerate()
        .filter(|(i, _)| !found.contains(i))
        .map(|(_, x)| x)
        .collect();
    let Some(private) = private else {
        folder.save_marks(p, &m).map_err(failed(WRITE))?;
        return Ok((None, changed));
    };
    let new = Mark {
        id: crate::ids::new("o"),
        item: item.to_string(),
        private,
        at: crate::refresh::time::fmt(crate::refresh::time::now()),
        by: by.map(str::to_string),
        request: request.map(str::to_string),
        members,
    };
    m.private.push(new.clone());
    folder.save_marks(p, &m).map_err(failed(WRITE))?;
    Ok((Some(new), true))
}

/// What the household's word on a record's clinician did.
#[derive(Debug, Clone, Default, Serialize)]
pub struct ClinicianWord {
    /// The ids of the marks it added, or took back.
    pub marks: Vec<String>,
    /// The person's items it opened, and those private again.
    pub opened: usize,
    pub held: usize,
}

/// Say that the clinician a record prints is of no private area (`open`):
/// none of the person's items is private by the `clinician` rule for them. Or
/// take that back. At the Mac alone: no request says it.
pub fn clinician(
    folder: &crate::folder::Folder,
    h: &Household,
    p: &Person,
    item: &str,
    open: bool,
) -> Result<ClinicianWord, Failed> {
    if !crate::ids::is_valid("r", item) {
        let not = format!("{} is not a record id", crate::printable(item));
        return Err(Failed::Refused(CliError::Usage(not)));
    }
    let idx = folder.index(p).map_err(failed(RECORDS))?;
    let Some(r) = idx.records.iter().find(|r| r.id == item) else {
        return Err(Failed::Refused(CliError::NotFound(format!(
            "{item} is not one of {}'s records",
            crate::printable(&p.name)
        ))));
    };
    if clinicians::printing(&r.kind).is_none() {
        let kind = crate::printable(&r.kind);
        let none = format!("{item} is a {kind}, and prints no clinician");
        return Err(Failed::Refused(CliError::Usage(none)));
    }
    let texts = folder.text(p).map_err(failed(RECORDS))?;
    let overrides = folder.overrides(p).map_err(failed(READ_DECISIONS))?;
    let state = known_labels(folder, p)?;
    let built = crate::events::both_by(folder, h, p, Some(&overrides)).map_err(failed(RECORDS))?;
    let private = |marks: &Marks| -> BTreeSet<String> {
        let v = judged(p, &idx, &texts, &built, marks, &state);
        Private::of(&v).items.into_keys().collect()
    };
    let mut marks = folder.marks(p).map_err(failed(READ))?;
    let before = private(&marks);
    let mut out = ClinicianWord::default();
    if open {
        let tied = clinicians::Clinicians::of(&idx, &marks.clinicians).tied(r);
        let already = marks.clinicians.iter().any(|o| o.of(r));
        if tied.is_empty() && !already {
            let not = format!("{item} is not private by a clinician it prints");
            return Err(Failed::Refused(CliError::Usage(not)));
        }
        let at = crate::refresh::time::fmt(crate::refresh::time::now());
        for (printed, people) in tied {
            let reading = |(surname, initial): (String, char)| clinicians::Reading {
                surname,
                initial: initial.to_string(),
            };
            let o = clinicians::Opened {
                id: crate::ids::new("o"),
                printed,
                people: people.into_iter().map(reading).collect(),
                item: item.to_string(),
                at: at.clone(),
                by: None,
                other: serde_json::Map::new(),
            };
            out.marks.push(o.id.clone());
            marks.clinicians.push(o);
        }
    } else {
        let of = |o: &clinicians::Opened| o.of(r);
        out.marks = marks
            .clinicians
            .iter()
            .filter(|o| of(o))
            .map(|o| o.id.clone())
            .collect();
        marks.clinicians.retain(|o| !of(o));
    }
    if !out.marks.is_empty() {
        folder.save_marks(p, &marks).map_err(failed(WRITE))?;
    }
    let after = private(&marks);
    out.opened = before.difference(&after).count();
    out.held = after.difference(&before).count();
    Ok(out)
}

/// Whether an item, or something in it, is private without these of the
/// person's marks.
fn private_without(
    folder: &crate::folder::Folder,
    h: &Household,
    p: &Person,
    item: &str,
    marks: &Marks,
    without: &[usize],
) -> Result<bool, Failed> {
    let idx = folder.index(p).map_err(failed(RECORDS))?;
    let texts = folder.text(p).map_err(failed(RECORDS))?;
    let overrides = folder.overrides(p).map_err(failed(READ_DECISIONS))?;
    let state = known_labels(folder, p)?;
    let built = crate::events::both_by(folder, h, p, Some(&overrides)).map_err(failed(RECORDS))?;
    let mut marks = marks.clone();
    let mut i = 0..;
    marks
        .private
        .retain(|_| i.next().is_none_or(|i| !without.contains(&i)));
    let v = judged(p, &idx, &texts, &built, &marks, &state);
    Ok(Private::of(&v).items.contains_key(item) || Parts::of(&v).has(item))
}

/// What the person's provider labels make private, where that is known.
fn known_labels(
    folder: &crate::folder::Folder,
    p: &Person,
) -> Result<crate::labels::State, Failed> {
    let state = crate::viewfiles::provider_state(folder, p).map_err(failed(LABELS))?;
    if state.lost() {
        let lost = CliError::Other("the person's provider labels do not read".into());
        return Err(failed(LABELS)(lost));
    }
    Ok(state)
}

/// Words that name no item on their own: forms, units, kinds of record and
/// visit, and the words between them.
const GENERIC: &[&str] = &[
    "tablet",
    "tablets",
    "capsule",
    "capsules",
    "oral",
    "solution",
    "suspension",
    "injection",
    "cream",
    "patch",
    "extended",
    "release",
    "delayed",
    "chewable",
    "hcl",
    "mg",
    "mcg",
    "ml",
    "unit",
    "units",
    "dose",
    "visit",
    "office",
    "encounter",
    "appointment",
    "telehealth",
    "video",
    "follow",
    "hospital",
    "clinic",
    "test",
    "tests",
    "result",
    "results",
    "lab",
    "labs",
    "panel",
    "screen",
    "screening",
    "report",
    "note",
    "notes",
    "letter",
    "bill",
    "statement",
    "summary",
    "record",
    "records",
    "document",
    "total",
    "serum",
    "blood",
    "urine",
    "level",
    "disorder",
    "unspecified",
    "other",
    "episode",
    "single",
    "recurrent",
    "moderate",
    "mild",
    "severe",
    "major",
    "minor",
    "history",
    "chronic",
    "acute",
    "score",
    "scale",
    "risk",
    "use",
    "status",
    "survey",
    "assessment",
    "questionnaire",
    "inventory",
    "checklist",
    "index",
    "item",
    "patient",
    "health",
    "doing",
    "things",
    "having",
    "feeling",
    "trouble",
    "little",
    "often",
    "days",
    "with",
    "without",
    "type",
    "the",
    "and",
    "for",
    "from",
];

/// Whether a question names an item: every word of it starts a word of the
/// item's title, and one of those title words is its own (not a form, unit
/// or kind of record), so "tablets" names no medicine and "sertraline" does.
pub fn names(title: &str, query: &str) -> bool {
    let words: BTreeSet<String> = crate::intake::classify::fold(query)
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_string)
        .collect();
    let title = crate::intake::classify::fold(title);
    let in_title: BTreeSet<&str> = title
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();
    let starts = |w: &str| -> Vec<&str> {
        in_title
            .iter()
            .copied()
            .filter(|t| t.starts_with(w))
            .collect()
    };
    !words.is_empty()
        && words.iter().all(|w| !starts(w).is_empty())
        && words.iter().any(|w| {
            w.chars().count() >= 3
                && starts(w)
                    .iter()
                    .any(|t| !GENERIC.contains(t) && t.chars().any(char::is_alphabetic))
        })
}

/// A map of ids to areas, as `json!` writes it, for a test.
#[cfg(test)]
fn areas(v: &Value, kind: &str) -> serde_json::Map<String, Value> {
    v[kind]
        .as_object()
        .unwrap()
        .iter()
        .map(|(k, w)| (k.clone(), w["area"].clone()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icd10_codes_fall_in_their_areas() {
        for (code, area) in [
            ("F41.1", Some(MENTAL_HEALTH)),
            ("F32A", Some(MENTAL_HEALTH)),
            ("F10.20", Some(SUBSTANCE_USE)),
            ("F17.210", Some(SUBSTANCE_USE)),
            ("Z31.41", Some(REPRODUCTIVE)),
            ("Z32.00", Some(REPRODUCTIVE)),
            ("Z33.2", Some(REPRODUCTIVE)),
            ("Z33.1", None),
            ("O03.9", Some(REPRODUCTIVE)),
            ("O80", None),
            ("N97.9", Some(REPRODUCTIVE)),
            ("A56.01", Some(SEXUAL_HEALTH)),
            ("B20", Some(SEXUAL_HEALTH)),
            ("Z11.3", Some(SEXUAL_HEALTH)),
            ("Z11.59", None),
            ("Z71.41", Some(SUBSTANCE_USE)),
            ("Z71.3", None),
            ("Z13.31", Some(MENTAL_HEALTH)),
            ("Z13.6", None),
            ("R45.851", Some(MENTAL_HEALTH)),
            ("R45.89", None),
            ("T14.91XA", Some(MENTAL_HEALTH)),
            ("T14.90XA", None),
            ("Z91.51", Some(MENTAL_HEALTH)),
            ("Z91.19", None),
            ("Z86.59", Some(MENTAL_HEALTH)),
            ("Z86.73", None),
            ("Z81.0", Some(MENTAL_HEALTH)),
            ("Z81.1", Some(SUBSTANCE_USE)),
            ("Z81.2", Some(SUBSTANCE_USE)),
            ("Z81.8", Some(MENTAL_HEALTH)),
            ("Z82.49", None),
            ("Z29.81", Some(SEXUAL_HEALTH)),
            ("Z29.8", None),
            ("Z97.5", Some(REPRODUCTIVE)),
            ("Z97.4", None),
            ("Z79.3", Some(REPRODUCTIVE)),
            ("Z79.4", None),
            ("O99.310", Some(SUBSTANCE_USE)),
            ("O99.323", Some(SUBSTANCE_USE)),
            ("O99.335", Some(SUBSTANCE_USE)),
            ("O99.345", Some(MENTAL_HEALTH)),
            ("O99.35", None),
            ("O99.89", None),
            ("Z34.00", None),
            ("I10", None),
            ("", None),
            ("F", None),
        ] {
            assert_eq!(icd10_area(code), area, "{code}");
        }
    }

    fn why_of(kind: &str, display: &str, code: Value, fields: Value) -> Option<(String, String)> {
        let mut r = record("r_ownwordsaaaaaaaaaaaa", kind, display, fields);
        if !code.is_null() {
            r["code"] = code;
        }
        let w = record_why(&serde_json::from_value(r).unwrap())?;
        Some((w.area.to_string(), w.from.to_string()))
    }

    /// A condition with no ICD-10 code is of what it is called; one with a code
    /// is of its code.
    #[test]
    fn a_condition_with_no_icd10_code_is_private_by_its_name() {
        let snomed =
            |name: &str| json!({"system": "http://snomed.info/sct", "code": "1", "display": name});
        let icd10 =
            |code: &str| json!({"system": "http://hl7.org/fhir/sid/icd-10-cm", "code": code});
        for (name, area) in [
            ("Generalized anxiety disorder", MENTAL_HEALTH),
            ("Major depressive disorder, single episode", MENTAL_HEALTH),
            ("History of suicidal thoughts", MENTAL_HEALTH),
            ("Attention deficit disorder", MENTAL_HEALTH),
            ("Post-traumatic stress disorder", MENTAL_HEALTH),
            ("Alcohol use disorder, mild", SUBSTANCE_USE),
            ("Opioid dependence", SUBSTANCE_USE),
            ("Former smoker", SUBSTANCE_USE),
            ("Genital herpes", SEXUAL_HEALTH),
            ("Exposure to an STI", SEXUAL_HEALTH),
            ("Female infertility", REPRODUCTIVE),
            ("Contraceptive management", REPRODUCTIVE),
            // By the forms its name can have.
            ("Alcohol-related disorder", SUBSTANCE_USE),
            ("Alcohol related disorders", SUBSTANCE_USE),
            ("Opioid related disorders", SUBSTANCE_USE),
            ("Substance-related disorders", SUBSTANCE_USE),
            (
                "Alcohol-induced persisting amnestic disorder",
                SUBSTANCE_USE,
            ),
            ("Opioid type dependence", SUBSTANCE_USE),
            ("Dependence on opioids", SUBSTANCE_USE),
            ("Abuse of cannabis", SUBSTANCE_USE),
            ("Harmful use of alcohol", SUBSTANCE_USE),
            ("Benzodiazepine dependence", SUBSTANCE_USE),
            ("Injection drug user", SUBSTANCE_USE),
            ("Smokes tobacco daily", SUBSTANCE_USE),
            ("Second hand smoke exposure", SUBSTANCE_USE),
            ("Problem drinker", SUBSTANCE_USE),
            ("Personality disorders", MENTAL_HEALTH),
            ("Eating disorders", MENTAL_HEALTH),
            ("Adjustment disorders", MENTAL_HEALTH),
            ("Conduct disorders", MENTAL_HEALTH),
            ("Tic disorders", MENTAL_HEALTH),
            ("Mental disorders", MENTAL_HEALTH),
            ("Mental and behavioral disorders", MENTAL_HEALTH),
            ("Persistent mood [affective] disorder", MENTAL_HEALTH),
            ("Seasonal affective disorder", MENTAL_HEALTH),
            ("Premenstrual dysphoric disorder", MENTAL_HEALTH),
            ("Pervasive developmental disorders", MENTAL_HEALTH),
            ("Developmental coordination disorder", MENTAL_HEALTH),
            ("Trichotillomania", MENTAL_HEALTH),
            ("Delirium", MENTAL_HEALTH),
            ("Depressed", MENTAL_HEALTH),
            ("Recurrent miscarriages", REPRODUCTIVE),
            ("Termination of pregnancy", REPRODUCTIVE),
            ("Elective termination of pregnancy", REPRODUCTIVE),
            ("Female sterility", REPRODUCTIVE),
            ("Habitual aborter", REPRODUCTIVE),
            ("Procreative management", REPRODUCTIVE),
            ("Ectopic pregnancies", REPRODUCTIVE),
            ("Presence of intrauterine device", REPRODUCTIVE),
            ("Acquired immunodeficiency syndrome", SEXUAL_HEALTH),
            ("Acquired immune deficiency syndrome", SEXUAL_HEALTH),
            ("AIDS", SEXUAL_HEALTH),
            ("Anogenital herpesviral infection", SEXUAL_HEALTH),
            ("Herpesviral infection of genitalia", SEXUAL_HEALTH),
            ("Herpes simplex infection of genitalia", SEXUAL_HEALTH),
            (
                "Contact with infections with a predominantly sexual mode of transmission",
                SEXUAL_HEALTH,
            ),
            ("Sexually transmissible infection", SEXUAL_HEALTH),
            ("High risk sexual behavior", SEXUAL_HEALTH),
            ("High risk heterosexual behavior", SEXUAL_HEALTH),
            ("High-risk homosexual behavior", SEXUAL_HEALTH),
            ("High risk bisexual behavior", SEXUAL_HEALTH),
        ] {
            let want = Some((area.to_string(), "words".to_string()));
            let why = |code: Value| why_of("condition", name, code, json!({}));
            assert_eq!(why(Value::Null), want, "{name}");
            assert_eq!(why(snomed("Example finding")), want, "{name}");
            // Its code decides, where it has one.
            assert_eq!(why(icd10("I10")), None, "{name}");
            let by_code = Some((MENTAL_HEALTH.to_string(), "code".to_string()));
            assert_eq!(why(icd10("F41.1")), by_code, "{name}");
            // By what its other code calls it too.
            let why = why_of("condition", "Example finding", snomed(name), json!({}));
            assert_eq!(why, want, "{name}");
        }
        for name in [
            "ST segment depression",
            "ST-segment depression",
            "Respiratory depression",
            "Stress reaction of bone",
            "Drug allergy",
            "Loss of appetite",
            "Ear pain",
            "Non-alcoholic fatty liver disease",
            "Nonalcoholic steatohepatitis",
            "Hearing aids",
            "HEARING AIDS",
            "Walking aids",
            "Depressed skull fracture",
            "Drug-induced movement disorder",
            "Opioid-induced constipation",
            "Long-term use of opioid analgesic",
            "Smoke inhalation",
            "Skin excoriation",
            "Altered mental status",
            "Developmental dysplasia of the hip",
            "Photophobia",
            "Sterile pyuria",
            "Pregnancy",
            "Language barrier",
        ] {
            assert_eq!(
                why_of("condition", name, Value::Null, json!({})),
                None,
                "{name}"
            );
        }
    }

    /// A visit is of its name and its reason, a report of its conclusion, and a
    /// result of a value that is a text.
    #[test]
    fn a_visit_a_report_and_a_result_are_private_by_their_own_words() {
        let words = |area: &str, from: &str| Some((area.to_string(), from.to_string()));
        let visit = |name: &str, reason: &str| {
            why_of("encounter", name, Value::Null, json!({"reason": reason}))
        };
        assert_eq!(visit("Sample visit", "Ear pain"), None);
        for (name, reason) in [
            ("Sample visit", "Anxiety"),
            ("Sample visit", "Psychotherapy"),
            ("Behavioral health visit", "Follow up"),
            ("Sample visit", "PHQ-9"),
        ] {
            assert_eq!(
                visit(name, reason),
                words(MENTAL_HEALTH, "words"),
                "{name}, {reason}"
            );
        }
        let told = |text: &str| {
            why_of(
                "report",
                "Example report",
                Value::Null,
                json!({"conclusion": text}),
            )
        };
        assert_eq!(told("ST depression seen"), None);
        // As a line of a bill's text is judged, a questionnaire's name included.
        assert_eq!(told("PHQ-9 of 3"), words(MENTAL_HEALTH, "test"));
        assert_eq!(
            told("A smoker, 20 pack-years"),
            words(SUBSTANCE_USE, "test")
        );
        assert_eq!(told("HIV antibody not seen"), words(SEXUAL_HEALTH, "test"));
        assert_eq!(told("Started Zoloft"), words(MENTAL_HEALTH, "test"));
        let value = |v: Value| why_of("result", "Example smear", Value::Null, json!({"value": v}));
        assert_eq!(value("Not seen".into()), None);
        assert_eq!(value(json!({"value": 3, "unit": "hiv"})), None);
        assert_eq!(
            value("Chlamydia not seen".into()),
            words(SEXUAL_HEALTH, "test")
        );
        // A result and a report are of their own names, and their parts', as
        // a visit is.
        let named = |kind: &str, name: &str| why_of(kind, name, Value::Null, json!({}));
        for (kind, name, area) in [
            ("result", "Psychotherapy session count", MENTAL_HEALTH),
            ("report", "Depression screening report", MENTAL_HEALTH),
            ("result", "Embryo grade", REPRODUCTIVE),
            ("report", "Substance use evaluation", SUBSTANCE_USE),
        ] {
            assert_eq!(named(kind, name), words(area, "test"), "{name}");
        }
        for name in [
            "Hemoglobin",
            "ST depression",
            "Gender identity",
            "Hearing aids",
        ] {
            assert_eq!(named("result", name), None, "{name}");
        }
        let part = json!({"components": [{"code": {"display": "Anxiety score"}, "value": 2}]});
        let of_part = why_of("result", "Example panel", Value::Null, part);
        assert_eq!(of_part, words(MENTAL_HEALTH, "test"));
    }

    /// A medicine is of its brand too. A brand that is a name is of a medicine
    /// only, and not of a text that has the name.
    #[test]
    fn a_medicines_brand_says_its_area() {
        let of = |name: &str| why_of("medication", name, Value::Null, json!({})).map(|w| w.0);
        let area = |area: &str| Some(area.to_string());
        for (name, want) in [
            ("Zoloft 50 MG", area(MENTAL_HEALTH)),
            ("CONCERTA 18 mg ER", area(MENTAL_HEALTH)),
            ("Chantix starting pack", area(SUBSTANCE_USE)),
            ("Gonal-F", area(REPRODUCTIVE)),
            ("Yasmin 28", area(REPRODUCTIVE)),
            ("Plan B One-Step", area(REPRODUCTIVE)),
            ("Biktarvy", area(SEXUAL_HEALTH)),
            ("Loratadine 10 MG", None),
            ("Celebrex", None),
        ] {
            assert_eq!(of(name), want, "{name}");
        }
        let fill = |name: &str| {
            let fields = json!({"type": "pharmacy", "category": "pharmacy"});
            why_of(
                "claim",
                &format!("{name} \u{b7} Example Pharmacy"),
                Value::Null,
                fields,
            )
        };
        assert_eq!(fill("Lexapro").unwrap().1, "medicine");
        assert_eq!(fill("Heather").unwrap().1, "medicine");
        assert_eq!(words("Started Lexapro"), Some(MENTAL_HEALTH));
        for text in [
            "Seen by Heather Example",
            "Yasmin Placeholder",
            "Plan B deductible",
        ] {
            assert_eq!(words(text), None, "{text}");
        }
    }

    /// What a source takes back of a record's own words still tells, and its
    /// other words are not kept.
    #[test]
    fn own_words_a_source_took_back_still_tell() {
        let of = |kind: &str, display: &str, code: Value, fields: Value| -> Record {
            let mut r = record("r_ownwordsaaaaaaaaaaaa", kind, display, fields);
            if !code.is_null() {
                r["code"] = code;
            }
            serde_json::from_value(r).unwrap()
        };
        let icd10 = json!({"system": "http://hl7.org/fhir/sid/icd-10-cm", "code": "I10"});
        let none = Value::Null;
        let each = [
            (
                of("condition", "Anxiety", none.clone(), json!({})),
                of("condition", "Anxiety", icd10, json!({})),
            ),
            (
                of(
                    "encounter",
                    "Sample visit",
                    none.clone(),
                    json!({"reason": "Anxiety"}),
                ),
                of(
                    "encounter",
                    "Sample visit",
                    none.clone(),
                    json!({"reason": "Ear pain"}),
                ),
            ),
            (
                of("encounter", "Psychotherapy", none.clone(), json!({})),
                of("encounter", "Sample visit", none.clone(), json!({})),
            ),
            (
                of(
                    "report",
                    "Example report",
                    none.clone(),
                    json!({"conclusion": "HIV not seen"}),
                ),
                of(
                    "report",
                    "Example report",
                    none.clone(),
                    json!({"conclusion": "Not seen"}),
                ),
            ),
            (
                of(
                    "result",
                    "Example smear",
                    none.clone(),
                    json!({"value": "Chlamydia seen"}),
                ),
                of("result", "Example smear", none.clone(), json!({"value": 2})),
            ),
        ];
        for (before, mut now) in each {
            let kind = now.kind.clone();
            assert!(record_why(&before).is_some(), "{kind}");
            assert!(record_why(&now).is_none(), "{kind}");
            assert!(given::keep(&mut now, given::gives(&before)), "{kind}");
            let gave = given::as_given(&now).find_map(|x| record_why(&x));
            assert!(gave.is_some(), "{kind}");
            // The other way round nothing is kept that the rules do not read.
            let mut then = before.clone();
            given::keep(&mut then, given::gives(&now));
            let kept = then
                .given
                .iter()
                .all(|g| g.fields.is_empty() && g.display.is_none());
            assert!(kept || kind == "condition", "{kind}: {:?}", then.given);
        }
    }

    /// A reading is private by the name or the code of any of its parts, with a
    /// value or without, and by a text that is the value of one.
    #[test]
    fn a_reading_is_private_by_any_of_its_parts() {
        let why = |fields: Value| {
            let r = record("r_readingaaaaaaaaaaaaa", "result", "Example panel", fields);
            record_why(&serde_json::from_value(r).unwrap()).map(|w| w.area)
        };
        let parts = |code: Value, value: Value| {
            let first = json!({"code": {"display": "First piece"}, "value": 1});
            json!({"components": [first, {"code": code, "value": value}]})
        };
        let named = |name: &str| json!({"display": name});
        let coded = json!({"system": "http://loinc.org", "code": "44250-9", "display": "Item"});
        assert_eq!(why(parts(named("Second piece"), 2.into())), None);
        assert_eq!(why(parts(named("Second piece"), "Not seen".into())), None);
        assert_eq!(
            why(parts(named("Chlamydia DNA"), 2.into())),
            Some(SEXUAL_HEALTH)
        );
        assert_eq!(why(parts(coded.clone(), 2.into())), Some(MENTAL_HEALTH));
        let seen = parts(named("Second piece"), "Chlamydia seen".into());
        assert_eq!(why(seen), Some(SEXUAL_HEALTH));
        // As `given` has one with no value.
        let absent = |code: Value| json!({"components": [{ "code": code }], "value": 3});
        assert_eq!(why(absent(named("Second piece"))), None);
        assert_eq!(why(absent(named("Chlamydia DNA"))), Some(SEXUAL_HEALTH));
        assert_eq!(why(absent(coded)), Some(MENTAL_HEALTH));
    }

    /// The view with no provider labels.
    fn built(
        p: &Person,
        idx: &PersonIndex,
        texts: &PersonText,
        events: &Value,
        marks: &[Mark],
    ) -> Value {
        let built = Built {
            events: events.clone(),
            ..Built::default()
        };
        let marks = Marks {
            private: marks.to_vec(),
            ..Marks::new(&p.id)
        };
        build(p, idx, texts, &built, &marks, &BTreeMap::new())
    }

    #[test]
    fn names_medicines_and_tests_say_their_area() {
        assert_eq!(
            first_area(&PLACES, "Example Behavioral Health"),
            Some(MENTAL_HEALTH)
        );
        assert_eq!(
            first_area(&PLACES, "Kim Example, LCSW"),
            Some(MENTAL_HEALTH)
        );
        assert_eq!(
            first_area(&PLACES, "Example Addiction and Behavioral Health"),
            Some(SUBSTANCE_USE)
        );
        assert_eq!(
            first_area(&PLACES, "Example Fertility Center"),
            Some(REPRODUCTIVE)
        );
        assert_eq!(first_area(&PLACES, "Example Physical Therapy"), None);
        assert_eq!(first_area(&PLACES, "Example Hospital"), None);
        assert_eq!(
            first_area(&MEDICINES, "Sertraline 50 mg tablet"),
            Some(MENTAL_HEALTH)
        );
        assert_eq!(
            first_area(&MEDICINES, "Vitamin D3 2,000 unit capsule"),
            None
        );
        assert_eq!(
            first_area(&TESTS, "Chlamydia trachomatis DNA"),
            Some(SEXUAL_HEALTH)
        );
        assert_eq!(first_area(&TESTS, "Hemoglobin A1c"), None);
        assert!(names("Sertraline 50 mg tablet", "sertraline"));
        assert!(names("Sertraline 50 mg tablet", "sert 50"));
        assert!(!names("Sertraline 50 mg tablet", "medicines"));
        assert!(!names("Sertraline 50 mg tablet", ""));
        assert!(!names("Sertraline 50 mg tablet", "tablet"));
        assert!(!names("Sertraline 50 mg tablet", "50 mg"));
        assert!(!names("Major depressive disorder", "major disorder"));
        assert!(names("Major depressive disorder", "depressive"));
        assert!(names("Example Behavioral Health", "behavioral"));
    }

    fn no_text() -> PersonText {
        PersonText::new("p_exampleaaaaaaaaaaaaa")
    }

    fn person() -> Person {
        serde_json::from_value(
            json!({"id": "p_testaaaaaaaaaaaaaaaa", "name": "Sam Example",
            "folder": "Sam Example", "created_at": "2026-01-01T00:00:00Z"}),
        )
        .unwrap()
    }

    fn record(id: &str, kind: &str, display: &str, fields: Value) -> Value {
        json!({"id": id, "kind": kind, "key": id, "date": "2024-08-13", "display": display,
            "fields": fields, "provenance": [{"source": {"kind": "fhir", "cli": "fakeprov"},
            "method": "fhir-api", "filed_at": "2026-11-20T10:00:00Z", "filed_by": "medrec"}]})
    }

    fn document(id: &str) -> Value {
        json!({"id": id, "path": format!("Documents/2026/{id}.pdf"), "sha256": id, "md5": id,
            "size": 1, "media_type": "application/pdf", "doc_type": "letter", "date": "2026-10-29",
            "filed_at": "2026-11-20T10:00:00Z", "origin": {"kind": "file", "action": "copied"},
            "provenance": {"source": {"kind": "scan"}, "method": "file",
            "filed_at": "2026-11-20T10:00:00Z", "filed_by": "medrec"}})
    }

    fn study(id: &str, report: Option<&str>) -> Value {
        let mut v = json!({"id": id, "study_uid": id, "path": format!("Imaging/{id}/"), "date": "2026-10-30",
            "series": [], "instances": 0, "size": 0, "filed_at": "2026-11-20T10:00:00Z", "provenance": []});
        if let Some(r) = report {
            v["report"] = r.into();
        }
        v
    }

    /// A document follows the record that links it, and a study its report.
    #[test]
    fn a_document_follows_the_record_that_links_it_and_a_study_its_report() {
        let mut program = record(
            "r_programclaimaaaaaaaa",
            "claim",
            "Class",
            json!({"provider": "Kim Example",
                "diagnoses": [{"sequence": 1, "code": {"system": crate::normalize::ICD10, "code": "F17.210"}}]}),
        );
        program["documents"] = json!([
            "d_eobaaaaaaaaaaaaaaaaa",
            "d_sharedaaaaaaaaaaaaaa",
            "d_goneaaaaaaaaaaaaaaa"
        ]);
        let mut foot = record(
            "r_footclaimaaaaaaaaaaa",
            "claim",
            "Class",
            json!({"provider": "Sample Foot Room"}),
        );
        foot["documents"] = json!(["d_sharedaaaaaaaaaaaaaa", "d_plainaaaaaaaaaaaaaaa"]);
        let v_of = |marks: &[Mark]| {
            let idx: PersonIndex = serde_json::from_value(json!({
                "schema": "medrec-index/v1", "person": "p_testaaaaaaaaaaaaaaaa", "rev": 1, "imports": [],
                "documents": [document("d_eobaaaaaaaaaaaaaaaaa"), document("d_sharedaaaaaaaaaaaaaa"),
                    document("d_plainaaaaaaaaaaaaaaa"), document("d_reportaaaaaaaaaaaaa")],
                "studies": [
                    study("m_eobaaaaaaaaaaaaaaaaa", Some("d_eobaaaaaaaaaaaaaaaaa")),
                    study("m_plainaaaaaaaaaaaaaa", Some("d_plainaaaaaaaaaaaaaaa")),
                    study("m_recordaaaaaaaaaaaaa", Some("r_programclaimaaaaaaaa")),
                    study("m_noneaaaaaaaaaaaaaaa", None),
                    study("m_lostaaaaaaaaaaaaaaa", Some("d_lostaaaaaaaaaaaaaaaa"))],
                "records": [program.clone(), foot.clone()]
            }))
            .unwrap();
            built(&person(), &idx, &no_text(), &json!({"events": []}), marks)
        };
        let v = v_of(&[]);
        // Any record that links it is enough, and an unlinked one is not.
        let docs = areas(&v, "documents");
        assert_eq!(
            docs,
            json!({"d_eobaaaaaaaaaaaaaaaaa": SUBSTANCE_USE, "d_sharedaaaaaaaaaaaaaa": SUBSTANCE_USE})
                .as_object()
                .unwrap()
                .clone()
        );
        assert_eq!(v["documents"]["d_eobaaaaaaaaaaaaaaaaa"]["from"], "record");
        let studies = areas(&v, "studies");
        assert_eq!(
            studies,
            json!({"m_eobaaaaaaaaaaaaaaaaa": SUBSTANCE_USE, "m_recordaaaaaaaaaaaaa": SUBSTANCE_USE})
                .as_object()
                .unwrap()
                .clone()
        );
        assert_eq!(v["studies"]["m_eobaaaaaaaaaaaaaaaaa"]["from"], "report");
        // No report, or one the person does not have: not judged.
        let mut none = v["unreported_studies"].as_array().unwrap().clone();
        none.sort_by_key(|x| x.to_string());
        assert_eq!(
            none,
            [
                json!("m_lostaaaaaaaaaaaaaaa"),
                json!("m_noneaaaaaaaaaaaaaaa")
            ]
        );

        // The household's mark on a document wins over the record that links
        // it, and a study then follows the document.
        let mark = |item: &str, private: bool| Mark {
            id: "o_markaaaaaaaaaaaaaaaa".into(),
            item: item.into(),
            private,
            at: "2026-11-24T12:00:00Z".into(),
            by: None,
            request: None,
            members: Vec::new(),
        };
        let v = v_of(&[
            mark("d_eobaaaaaaaaaaaaaaaaa", false),
            mark("d_plainaaaaaaaaaaaaaaa", true),
        ]);
        assert_eq!(
            areas(&v, "documents"),
            json!({"d_plainaaaaaaaaaaaaaaa": HOUSEHOLD, "d_sharedaaaaaaaaaaaaaa": SUBSTANCE_USE})
                .as_object()
                .unwrap()
                .clone()
        );
        assert_eq!(
            areas(&v, "studies"),
            json!({"m_plainaaaaaaaaaaaaaa": HOUSEHOLD, "m_recordaaaaaaaaaaaaa": SUBSTANCE_USE})
                .as_object()
                .unwrap()
                .clone()
        );
        // A record marked not private links nothing.
        let v = v_of(&[mark("r_programclaimaaaaaaaa", false)]);
        assert!(areas(&v, "documents").is_empty(), "{v}");
        assert!(areas(&v, "studies").is_empty(), "{v}");
    }

    /// A document linked to a record an event makes private is private too.
    #[test]
    fn a_link_follows_what_an_event_makes_private() {
        let program = record(
            "r_programclaimaaaaaaaa",
            "claim",
            "Class",
            json!({"provider": "Kim Example",
                "diagnoses": [{"sequence": 1, "code": {"system": crate::normalize::ICD10, "code": "F17.210"}}]}),
        );
        let mut visit = record(
            "r_programvisitaaaaaaaa",
            "encounter",
            "Group Visit",
            json!({"class": "AMB"}),
        );
        visit["documents"] = json!(["d_summaryaaaaaaaaaaaa"]);
        let idx: PersonIndex = serde_json::from_value(json!({
            "schema": "medrec-index/v1", "person": "p_testaaaaaaaaaaaaaaaa", "rev": 1, "imports": [],
            "documents": [document("d_summaryaaaaaaaaaaaa")], "studies": [],
            "records": [program, visit]
        }))
        .unwrap();
        let events = json!({"events": [{"id": "e_programaaaaaaaaaaaaa", "members": [
            {"record": "r_programclaimaaaaaaaa", "kind": "claim", "role": "office"},
            {"record": "r_programvisitaaaaaaaa", "kind": "encounter", "role": "portal"}]}]});
        let v = built(&person(), &idx, &no_text(), &events, &[]);
        assert_eq!(v["documents"]["d_summaryaaaaaaaaaaaa"]["from"], "record");
    }

    /// The papers of `tests/fixtures/private/papers.json`, and their texts.
    fn papers() -> (PersonIndex, PersonText) {
        let list: Vec<Value> =
            serde_json::from_str(include_str!("../tests/fixtures/private/papers.json")).unwrap();
        let (mut docs, mut texts) = (Vec::new(), no_text());
        for p in list {
            let id = p["id"].as_str().unwrap();
            let mut d = document(id);
            d["doc_type"] = p["type"].clone();
            d["date"] = p["date"].clone();
            for (to, from) in [
                ("organization", "org"),
                ("title", "title"),
                ("statement", "statement"),
                ("same_statement_as", "same_statement_as"),
            ] {
                if let Some(v) = p.get(from) {
                    d[to] = v.clone();
                }
            }
            for other in p["near_duplicates"].as_array().into_iter().flatten() {
                d["near_duplicates"] = json!([{"document": other, "similarity": 0.95}]);
            }
            let text = p["text"].as_str().unwrap();
            texts.texts.insert(id.to_string(), text.to_string());
            docs.push(d);
        }
        let idx = serde_json::from_value(json!({
            "schema": "medrec-index/v1", "person": "p_testaaaaaaaaaaaaaaaa", "rev": 1,
            "imports": [], "records": [], "studies": [], "documents": docs}));
        (idx.unwrap(), texts)
    }

    /// A bill or receipt is private by its first page's words, any document
    /// by its title's, and a paper with the private one it is a copy of,
    /// comes before or after, pays or is paid by.
    #[test]
    fn a_paper_is_private_by_its_own_words_and_takes_its_statements_along() {
        let (idx, texts) = papers();
        let events = json!({"events": []});
        let why = |marks: &[Mark]| -> BTreeMap<String, (String, String)> {
            let v = built(&person(), &idx, &texts, &events, marks);
            v["documents"]
                .as_object()
                .unwrap()
                .iter()
                .map(|(id, w)| {
                    let s = |k: &str| w[k].as_str().unwrap().to_string();
                    (id.clone(), (s("area"), s("from")))
                })
                .collect()
        };
        let want = |list: &[(&str, &str, &str)]| -> BTreeMap<String, (String, String)> {
            list.iter()
                .map(|(id, a, f)| (id.to_string(), (a.to_string(), f.to_string())))
                .collect()
        };
        let all = [
            ("d_firstaaaaaaaaaaaaaaa", SUBSTANCE_USE, "words"),
            ("d_lozengesaaaaaaaaaaaa", SUBSTANCE_USE, "words"),
            ("d_photoaaaaaaaaaaaaaaa", SUBSTANCE_USE, "paper"),
            ("d_secondaaaaaaaaaaaaaa", SUBSTANCE_USE, "paper"),
            ("d_secondcopyaaaaaaaaaa", SUBSTANCE_USE, "paper"),
            ("d_testbillaaaaaaaaaaaa", SEXUAL_HEALTH, "words"),
            ("d_thirdaaaaaaaaaaaaaaa", SUBSTANCE_USE, "paper"),
            ("d_thirdpaidaaaaaaaaaaa", SUBSTANCE_USE, "paper"),
            ("d_titledaaaaaaaaaaaaaa", SUBSTANCE_USE, "words"),
        ];
        assert_eq!(why(&[]), want(&all));

        // The household's word comes first, and what it takes out ties no
        // other paper: without the last statement the receipt that pays it, and
        // the second, with its copy, are out too.
        let mark = |item: &str, private: bool| Mark {
            id: "o_markaaaaaaaaaaaaaaaa".into(),
            item: item.into(),
            private,
            at: "2026-11-24T12:00:00Z".into(),
            by: None,
            request: None,
            members: Vec::new(),
        };
        let out = ["d_second", "d_third"];
        let left: Vec<_> = all
            .iter()
            .filter(|(id, ..)| !out.iter().any(|o| id.starts_with(o)))
            .copied()
            .collect();
        assert_eq!(
            why(&[mark("d_thirdaaaaaaaaaaaaaaa", false)]),
            want(&left),
            "the statement taken out superseded the second"
        );
        // And marking one private takes the same papers along.
        let marked = why(&[
            mark("d_firstaaaaaaaaaaaaaaa", false),
            mark("d_secondcopyaaaaaaaaaa", true),
        ]);
        assert_eq!(marked["d_thirdpaidaaaaaaaaaaa"].1, "paper");
        assert_eq!(marked["d_thirdaaaaaaaaaaaaaaa"].0, HOUSEHOLD);
        assert!(!marked.contains_key("d_firstaaaaaaaaaaaaaaa"));
        assert!(!marked.contains_key("d_photoaaaaaaaaaaaaaaa"));
    }

    /// A paper whose only source says it is entered in error.
    fn in_error(d: &mut Document) {
        let raw = format!("Records/example/Binary/{}.json", d.id);
        d.provenance.raw_path = Some(raw.clone());
        d.words = serde_json::from_value(json!([{"raw_path": raw, "status": "entered-in-error"}]))
            .unwrap();
    }

    /// A study no report judges stays so whatever is entered in error: one of
    /// its day's two reports, or a paper of another day.
    #[test]
    fn what_is_entered_in_error_opens_no_study_without_a_report() {
        let (one, two, other) = (
            "d_reportoneaaaaaaaaaaa",
            "d_reporttwoaaaaaaaaaaa",
            "d_letteraaaaaaaaaaaaaa",
        );
        let report = |id: &str| {
            let mut d = document(id);
            d["doc_type"] = "imaging_report".into();
            d["date"] = "2026-10-30".into();
            d
        };
        let mut idx: PersonIndex = serde_json::from_value(json!({
            "schema": "medrec-index/v1", "person": "p_testaaaaaaaaaaaaaaaa", "rev": 1, "imports": [],
            "documents": [report(one), report(two), document(other)],
            "studies": [study("m_studyaaaaaaaaaaaaaaa", None)], "records": []
        }))
        .unwrap();
        // Two reports of its day: none is its own.
        assert_eq!(crate::imaging::link_reports(&mut idx), 0);
        let unreported = |idx: &PersonIndex| {
            let v = built(&person(), idx, &no_text(), &json!({"events": []}), &[]);
            v["unreported_studies"].clone()
        };
        let none = json!(["m_studyaaaaaaaaaaaaaaa"]);
        assert_eq!(unreported(&idx), none);
        for i in 0..3 {
            let mut gone = idx.clone();
            in_error(&mut gone.documents[i]);
            assert_eq!(unreported(&gone), none, "{}", idx.documents[i].id);
            // Nor does a link made while it is.
            assert_eq!(crate::imaging::link_reports(&mut gone), 0);
        }
        // One of one report not linked yet is not linked by another's.
        idx.documents.remove(1);
        in_error(&mut idx.documents[1]);
        assert_eq!(unreported(&idx), none);
        assert_eq!(crate::standing::index(&idx).studies[0].report, None);
    }

    /// What a source takes back still tells: each paper stays private, as do
    /// those that were by it.
    #[test]
    fn nothing_opens_because_a_paper_is_entered_in_error() {
        let (idx, texts) = papers();
        let none = || json!({"events": []});
        let before = built(&person(), &idx, &texts, &none(), &[]);
        let before = areas(&before, "documents");
        assert!(before.len() > 5);
        for i in 0..idx.documents.len() {
            let mut idx = idx.clone();
            in_error(&mut idx.documents[i]);
            let both = Built {
                events: none(),
                whole: Some(none()),
                ..Built::default()
            };
            let v = build(
                &person(),
                &idx,
                &texts,
                &both,
                &Marks::new("p_test"),
                &BTreeMap::new(),
            );
            let after = areas(&v, "documents");
            for (id, area) in &before {
                assert_eq!(after.get(id), Some(area), "{id} by {}", idx.documents[i].id);
            }
        }
    }

    /// So does a visit its claim made private, and its event, though a source
    /// takes the claim back; and a study whose only report is.
    #[test]
    fn nothing_opens_because_a_claim_or_a_report_is_entered_in_error() {
        let (claim, visit) = ("r_programclaimaaaaaaaa", "r_programvisitaaaaaaaa");
        let (event, report) = ("e_programaaaaaaaaaaaaa", "d_reportaaaaaaaaaaaaa");
        let mut program = record(
            claim,
            "claim",
            "Class",
            json!({"provider": "Kim Example",
                "diagnoses": [{"sequence": 1, "code": {"system": crate::normalize::ICD10, "code": "F17.210"}}]}),
        );
        program["documents"] = json!([report]);
        let mut idx: PersonIndex = serde_json::from_value(json!({
            "schema": "medrec-index/v1", "person": "p_testaaaaaaaaaaaaaaaa", "rev": 1, "imports": [],
            "documents": [document(report)],
            "studies": [study("m_reportaaaaaaaaaaaaa", Some(report))],
            "records": [program, record(visit, "encounter", "Group Visit", json!({"class": "AMB"}))]
        }))
        .unwrap();
        let member = |id: &str, kind: &str| json!({"record": id, "kind": kind, "role": "office"});
        let events = |members: &[Value]| json!({"events": [{"id": event, "members": members}]});
        let whole = events(&[member(claim, "claim"), member(visit, "encounter")]);
        let before = built(&person(), &idx, &no_text(), &whole, &[]);
        assert_eq!(before["events"][event]["from"], "members");
        assert_eq!(before["records"][visit]["from"], "event");
        idx.records[0].status = Some("entered-in-error".into());
        in_error(&mut idx.documents[0]);
        let both = Built {
            events: events(&[member(visit, "encounter")]),
            whole: Some(whole),
            ..Built::default()
        };
        let v = build(
            &person(),
            &idx,
            &no_text(),
            &both,
            &Marks::new("p_test"),
            &BTreeMap::new(),
        );
        for kind in ["records", "documents", "events", "studies"] {
            assert_eq!(areas(&v, kind), areas(&before, kind), "{kind}");
        }
        // No report judges the study now, and it is not opened for that.
        assert_eq!(v["unreported_studies"], json!(["m_reportaaaaaaaaaaaaa"]));
        // Once its claim is gone from the events as from the index, no more.
        let gone = Built {
            events: both.events.clone(),
            ..Built::default()
        };
        idx.records.remove(0);
        let v = build(
            &person(),
            &idx,
            &no_text(),
            &gone,
            &Marks::new("p_test"),
            &BTreeMap::new(),
        );
        assert!(areas(&v, "events").is_empty(), "{v}");
    }

    #[test]
    fn an_event_follows_its_visit_and_the_household_has_the_last_word() {
        let idx: PersonIndex = serde_json::from_value(json!({
            "schema": "medrec-index/v1", "person": "p_testaaaaaaaaaaaaaaaa", "rev": 1,
            "documents": [], "studies": [], "imports": [],
            "records": [
                record("r_programclaimaaaaaaaa", "claim", "Class", json!({
                    "provider": "Kim Example",
                    "diagnoses": [{"sequence": 2, "code": {"system": crate::normalize::ICD10, "code": "I10"}},
                                  {"sequence": 1, "code": {"system": crate::normalize::ICD10, "code": "F17.210"}}]})),
                record("r_programvisitaaaaaaaa", "encounter", "Group Visit", json!({"class": "AMB"})),
                record("r_lozengeaaaaaaaaaaaaa", "medication", "Nicotine 2 mg lozenge", json!({"source": "dispense"})),
                record("r_loratadineaaaaaaaaaa", "medication", "Loratadine 10 mg tablet", json!({"source": "dispense"})),
                record("r_earclaimaaaaaaaaaaaa", "claim", "Class", json!({
                    "provider": "Sample Ear Room",
                    "diagnoses": [{"sequence": 1, "code": {"system": crate::normalize::ICD10, "code": "H61.20"}},
                                  {"sequence": 2, "code": {"system": crate::normalize::ICD10, "code": "Z71.41"}}]})),
                record("r_screenaaaaaaaaaaaaaa", "result", "Drug screen", json!({})),
            ]
        }))
        .unwrap();
        let events = json!({"events": [
            {"id": "e_programaaaaaaaaaaaaa", "members": [
                {"record": "r_programclaimaaaaaaaa", "kind": "claim", "role": "office"},
                {"record": "r_programvisitaaaaaaaa", "kind": "encounter", "role": "portal"}]},
            {"id": "e_earaaaaaaaaaaaaaaaaa", "members": [
                {"record": "r_earclaimaaaaaaaaaaaa", "kind": "claim", "role": "office"},
                {"record": "r_screenaaaaaaaaaaaaaa", "kind": "result", "role": "satellite"}]},
            {"id": "e_fillaaaaaaaaaaaaaaaa", "members": [
                {"record": "r_lozengeaaaaaaaaaaaaa", "kind": "medication", "role": "fill"}]},
            {"id": "e_loratadinefillaaaaaa", "members": [
                {"record": "r_loratadineaaaaaaaaaa", "kind": "medication", "role": "fill"}]}
        ]});
        let v = built(&person(), &idx, &no_text(), &events, &[]);
        assert_eq!(v["schema"], SCHEMA);
        // A claim by its principal diagnosis, and its visit with it; a fill
        // by its medicine; a result by its test, but not its event, whose
        // claim has another principal diagnosis.
        assert_eq!(
            areas(&v, "records"),
            json!({
                "r_screenaaaaaaaaaaaaaa": SUBSTANCE_USE,
                "r_lozengeaaaaaaaaaaaaa": SUBSTANCE_USE,
                "r_programclaimaaaaaaaa": SUBSTANCE_USE,
                "r_programvisitaaaaaaaa": SUBSTANCE_USE,
            })
            .as_object()
            .unwrap()
            .clone()
        );
        assert_eq!(v["records"]["r_programclaimaaaaaaaa"]["from"], "code");
        assert_eq!(v["records"]["r_programvisitaaaaaaaa"]["from"], "event");
        assert_eq!(
            areas(&v, "events"),
            json!({"e_fillaaaaaaaaaaaaaaaa": SUBSTANCE_USE, "e_programaaaaaaaaaaaaa": SUBSTANCE_USE})
                .as_object()
                .unwrap()
                .clone()
        );

        // The household marks the other fill private and the program's event
        // not: the event and the visit leave, the claim stays private by its
        // own code.
        let mark = |item: &str, private: bool| Mark {
            id: "o_markaaaaaaaaaaaaaaaa".into(),
            item: item.into(),
            private,
            at: "2026-11-24T12:00:00Z".into(),
            by: None,
            request: None,
            members: Vec::new(),
        };
        let v = built(
            &person(),
            &idx,
            &no_text(),
            &events,
            &[
                mark("r_loratadineaaaaaaaaaa", true),
                mark("e_programaaaaaaaaaaaaa", false),
                // Items the person no longer has.
                mark("r_goneaaaaaaaaaaaaaaaa", true),
                mark("e_goneaaaaaaaaaaaaaaaa", true),
            ],
        );
        assert!(v["records"].get("r_goneaaaaaaaaaaaaaaaa").is_none(), "{v}");
        assert!(v["events"].get("e_goneaaaaaaaaaaaaaaaa").is_none(), "{v}");

        assert_eq!(
            v["records"]["r_loratadineaaaaaaaaaa"],
            json!({"area": HOUSEHOLD, "from": "household"})
        );
        assert_eq!(
            v["events"]["e_loratadinefillaaaaaa"],
            json!({"area": HOUSEHOLD, "from": "members"})
        );
        assert!(v["events"].get("e_programaaaaaaaaaaaaa").is_none(), "{v}");
        assert!(v["records"].get("r_programvisitaaaaaaaa").is_none(), "{v}");
        assert_eq!(v["records"]["r_programclaimaaaaaaaa"]["from"], "code");

        // A mark on an event whose id changed (its anchor did) follows its
        // members; one whose members are all gone is skipped.
        let followed = Mark {
            members: vec!["r_loratadineaaaaaaaaaa".into()],
            ..mark("e_oldloratadinefillaaa", false)
        };
        let w = built(
            &person(),
            &idx,
            &no_text(),
            &events,
            &[mark("r_loratadineaaaaaaaaaa", true), followed],
        );
        assert!(w["events"].get("e_loratadinefillaaaaaa").is_none(), "{w}");
        assert!(w["events"].get("e_oldloratadinefillaaa").is_none(), "{w}");
        // Its own mark wins over one that followed.
        let own = mark("e_loratadinefillaaaaaa", true);
        let followed = Mark {
            members: vec!["r_loratadineaaaaaaaaaa".into()],
            ..mark("e_oldloratadinefillaaa", false)
        };
        let w = built(&person(), &idx, &no_text(), &events, &[own, followed]);
        assert_eq!(
            w["events"]["e_loratadinefillaaaaaa"],
            json!({"area": HOUSEHOLD, "from": "household"})
        );
    }
}
