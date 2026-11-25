//! What a provider does, in medrec's own words, by the taxonomy code its
//! directory row gives. The code set's own descriptors are not used: they are
//! licensed. A code with no words here has none.

use crate::private::{MENTAL_HEALTH, REPRODUCTIVE, SUBSTANCE_USE};

/// What kind of provider a role is, to tell which services one fits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Group {
    Doctor,
    Anesthesiologist,
    /// A nurse practitioner, physician assistant or midwife.
    Advanced,
    /// Gives anesthesia, and is no doctor.
    Anesthetist,
    Dental,
    Eye,
    Foot,
    Chiropractic,
    Therapy,
    Mind,
    Nurse,
    Pharmacist,
    /// Dietitians, acupuncturists and the like.
    Allied,
    /// Technicians, aides and the like.
    Support,
    Organization,
}

/// What a provider was printed for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Service {
    Prescription,
    Vaccine,
    Dental,
    Anesthesia,
    Imaging,
    Lab,
    Procedure,
    /// An office visit, as a claim codes it.
    Visit,
    /// A visit, as a portal lists it.
    Encounter,
    Any,
}

impl Service {
    /// Whether a provider of this group gives it.
    pub fn fits(self, g: Group) -> bool {
        use Group::*;
        match self {
            Service::Prescription => {
                matches!(
                    g,
                    Doctor | Anesthesiologist | Advanced | Dental | Eye | Foot
                )
            }
            Service::Vaccine => {
                matches!(g, Doctor | Anesthesiologist | Advanced | Nurse | Pharmacist)
            }
            Service::Dental => g == Dental,
            Service::Anesthesia => matches!(g, Anesthesiologist | Anesthetist),
            Service::Imaging => {
                matches!(
                    g,
                    Doctor | Anesthesiologist | Advanced | Foot | Chiropractic
                )
            }
            Service::Lab => matches!(g, Doctor | Anesthesiologist | Advanced),
            Service::Procedure => matches!(
                g,
                Doctor | Anesthesiologist | Advanced | Dental | Eye | Foot | Chiropractic | Therapy
            ),
            Service::Visit => matches!(
                g,
                Doctor | Anesthesiologist | Advanced | Eye | Foot | Chiropractic | Mind
            ),
            Service::Encounter => !matches!(g, Pharmacist | Support | Organization),
            Service::Any => g != Organization,
        }
    }
}

#[derive(Debug)]
pub struct Role {
    pub words: &'static str,
    pub group: Group,
    /// The private area the role is of, if any.
    pub area: Option<&'static str>,
    /// Codes, or the beginnings of codes: the longest that matches decides.
    codes: &'static [&'static str],
}

const fn role(words: &'static str, group: Group, codes: &'static [&'static str]) -> Role {
    Role {
        words,
        group,
        area: None,
        codes,
    }
}

const fn private(
    words: &'static str,
    group: Group,
    area: &'static str,
    codes: &'static [&'static str],
) -> Role {
    Role {
        words,
        group,
        area: Some(area),
        codes,
    }
}

use Group::{
    Advanced, Allied, Anesthesiologist, Anesthetist, Chiropractic, Dental, Doctor, Eye, Foot, Mind,
    Nurse, Organization, Pharmacist, Support, Therapy,
};

static ROLES: &[Role] = &[
    // Doctors.
    role(
        "allergist",
        Doctor,
        &["207K", "207RA0201X", "2080P0201X", "207YX0602X"],
    ),
    role("anesthesiologist", Anesthesiologist, &["207L"]),
    role(
        "pain doctor",
        Anesthesiologist,
        &["207LP2900X", "208VP", "2081P2900X", "2084P2900X"],
    ),
    private(
        "addiction medicine doctor",
        Doctor,
        SUBSTANCE_USE,
        &[
            "207LA0401X",
            "207QA0401X",
            "207RA0401X",
            "2083A0300X",
            "2084A0401X",
        ],
    ),
    role("dermatologist", Doctor, &["207N"]),
    role(
        "emergency doctor",
        Doctor,
        &["207P", "2080P0204X", "2083P0011X"],
    ),
    role("family doctor", Doctor, &["207Q"]),
    role(
        "sports medicine doctor",
        Doctor,
        &[
            "207PS0010X",
            "207QS0010X",
            "207RS0010X",
            "2080S0010X",
            "2081S0010X",
            "2083S0010X",
            "2084S0010X",
            "207XX0005X",
        ],
    ),
    role(
        "sleep doctor",
        Doctor,
        &[
            "207QS1201X",
            "207RS0012X",
            "207YS0012X",
            "2080S0012X",
            "2084S0012X",
        ],
    ),
    role(
        "palliative care doctor",
        Doctor,
        &[
            "207LH0002X",
            "207PH0002X",
            "207QH0002X",
            "207RH0002X",
            "207VH0002X",
            "2080H0002X",
            "2081H0002X",
            "2084H0002X",
            "2085H0002X",
            "2086H0002X",
        ],
    ),
    role(
        "doctor for older adults",
        Doctor,
        &["207QG0300X", "207RG0300X"],
    ),
    role(
        "weight management doctor",
        Doctor,
        &[
            "207QB0002X",
            "207RB0002X",
            "207VB0002X",
            "2080B0002X",
            "2083B0002X",
            "2084B0002X",
        ],
    ),
    role("internal medicine doctor", Doctor, &["207R"]),
    role(
        "cardiologist",
        Doctor,
        &[
            "207RC0000X",
            "207RC0001X",
            "207RI0011X",
            "207RA0001X",
            "207RA0002X",
        ],
    ),
    role("critical care doctor", Doctor, &["207RC0200X"]),
    role("endocrinologist", Doctor, &["207RE0101X"]),
    role("gastroenterologist", Doctor, &["207RG0100X"]),
    role("blood specialist", Doctor, &["207RH0000X"]),
    role("cancer doctor", Doctor, &["207RH0003X", "207RX0202X"]),
    role("liver specialist", Doctor, &["207RI0008X", "207RT0003X"]),
    role(
        "infectious disease doctor",
        Doctor,
        &["207RI0200X", "2080P0208X"],
    ),
    role("kidney specialist", Doctor, &["207RN0300X"]),
    role("lung specialist", Doctor, &["207RP1001X"]),
    role("rheumatologist", Doctor, &["207RR0500X"]),
    role("genetics doctor", Doctor, &["207S"]),
    role("neurosurgeon", Doctor, &["207T"]),
    role("nuclear medicine doctor", Doctor, &["207U"]),
    role("OB-GYN", Doctor, &["207V"]),
    private("fertility doctor", Doctor, REPRODUCTIVE, &["207VE0102X"]),
    private(
        "family planning doctor",
        Doctor,
        REPRODUCTIVE,
        &["207VC0300X"],
    ),
    role("high-risk pregnancy doctor", Doctor, &["207VM0101X"]),
    role(
        "pelvic floor surgeon",
        Doctor,
        &["207VF0040X", "2088F0040X"],
    ),
    role("gynecologic cancer surgeon", Doctor, &["207VX0201X"]),
    role("eye doctor", Eye, &["207W", "152W"]),
    role("orthopedic surgeon", Doctor, &["207X"]),
    role(
        "hand surgeon",
        Doctor,
        &["207XS0106X", "2082S0105X", "2086S0105X"],
    ),
    role("spine surgeon", Doctor, &["207XS0117X"]),
    role("ear, nose and throat doctor", Doctor, &["207Y"]),
    role("pathologist", Doctor, &["207Z"]),
    role("pediatrician", Doctor, &["2080"]),
    role("newborn doctor", Doctor, &["2080N0001X"]),
    role(
        "child development doctor",
        Doctor,
        &["2080P0006X", "2080P0008X"],
    ),
    role("pediatric cardiologist", Doctor, &["2080P0202X"]),
    role("pediatric critical care doctor", Doctor, &["2080P0203X"]),
    role("pediatric endocrinologist", Doctor, &["2080P0205X"]),
    role("pediatric gastroenterologist", Doctor, &["2080P0206X"]),
    role("pediatric cancer doctor", Doctor, &["2080P0207X"]),
    role("pediatric kidney specialist", Doctor, &["2080P0210X"]),
    role("pediatric lung specialist", Doctor, &["2080P0214X"]),
    role("pediatric rheumatologist", Doctor, &["2080P0216X"]),
    role("rehabilitation doctor", Doctor, &["2081"]),
    role("plastic surgeon", Doctor, &["2082", "2086S0122X"]),
    role("preventive medicine doctor", Doctor, &["2083"]),
    role("occupational medicine doctor", Doctor, &["2083X0100X"]),
    role(
        "neurologist",
        Doctor,
        &[
            "2084A2900X",
            "2084D0003X",
            "2084E0001X",
            "2084N",
            "2084P0301X",
            "2084V",
        ],
    ),
    role("pediatric neurologist", Doctor, &["2084N0402X"]),
    private(
        "psychiatrist",
        Doctor,
        MENTAL_HEALTH,
        &["2084P0800X", "2084P0015X", "2084P0805X", "2084F0202X"],
    ),
    private("child psychiatrist", Doctor, MENTAL_HEALTH, &["2084P0804X"]),
    private(
        "addiction psychiatrist",
        Doctor,
        SUBSTANCE_USE,
        &["2084P0802X"],
    ),
    role("radiologist", Doctor, &["2085"]),
    role(
        "radiation oncologist",
        Doctor,
        &["2085R0001X", "2085R0203X"],
    ),
    role("interventional radiologist", Doctor, &["2085R0204X"]),
    role("surgeon", Doctor, &["2086"]),
    role("pediatric surgeon", Doctor, &["2086S0120X"]),
    role("trauma surgeon", Doctor, &["2086S0127X"]),
    role("vascular surgeon", Doctor, &["2086S0129X"]),
    role("cancer surgeon", Doctor, &["2086X0206X"]),
    role("urologist", Doctor, &["2088"]),
    role("pediatric urologist", Doctor, &["2088P0231X"]),
    role("colon and rectal surgeon", Doctor, &["208C"]),
    role("general doctor", Doctor, &["208D"]),
    role("heart and chest surgeon", Doctor, &["208G"]),
    role("hospital doctor", Doctor, &["208M"]),
    role("oral surgeon", Dental, &["204E", "1223S0112X"]),
    role("transplant surgeon", Doctor, &["204F"]),
    role("osteopathic doctor", Doctor, &["204C", "204D"]),
    role("vein doctor", Doctor, &["202K"]),
    // Advanced practice and nursing.
    role("nurse practitioner", Advanced, &["363L"]),
    private(
        "psychiatric nurse practitioner",
        Advanced,
        MENTAL_HEALTH,
        &["363LP0808X"],
    ),
    role("physician assistant", Advanced, &["363A"]),
    role("clinical nurse specialist", Advanced, &["364S"]),
    private(
        "psychiatric nurse specialist",
        Advanced,
        MENTAL_HEALTH,
        &["364SP08"],
    ),
    role("nurse-midwife", Advanced, &["367A"]),
    role("midwife", Advanced, &["176B", "175M"]),
    role("nurse anesthetist", Anesthetist, &["3675"]),
    role("anesthesiologist assistant", Anesthetist, &["367H"]),
    role("nurse", Nurse, &["163W", "164W", "164X"]),
    private("psychiatric nurse", Nurse, MENTAL_HEALTH, &["163WP08"]),
    private("addiction nurse", Nurse, SUBSTANCE_USE, &["163WA0400X"]),
    role("lactation consultant", Allied, &["163WL0100X", "174N"]),
    // The mind.
    private("counselor", Mind, MENTAL_HEALTH, &["101Y"]),
    private(
        "substance use counselor",
        Mind,
        SUBSTANCE_USE,
        &["101YA0400X"],
    ),
    private(
        "mental health counselor",
        Mind,
        MENTAL_HEALTH,
        &["101YM0800X"],
    ),
    private("psychoanalyst", Mind, MENTAL_HEALTH, &["102L"]),
    private("neuropsychologist", Mind, MENTAL_HEALTH, &["103G"]),
    private("behavior analyst", Mind, MENTAL_HEALTH, &["103K", "106E"]),
    private("psychologist", Mind, MENTAL_HEALTH, &["103T"]),
    private(
        "addiction psychologist",
        Mind,
        SUBSTANCE_USE,
        &["103TA0400X"],
    ),
    private("social worker", Mind, MENTAL_HEALTH, &["1041"]),
    private("family therapist", Mind, MENTAL_HEALTH, &["106H"]),
    private("behavior technician", Support, MENTAL_HEALTH, &["106S"]),
    private("peer support specialist", Support, MENTAL_HEALTH, &["175T"]),
    // Teeth, eyes, feet and backs.
    role("dentist", Dental, &["1223"]),
    role("endodontist", Dental, &["1223E0200X"]),
    role("pediatric dentist", Dental, &["1223P0221X"]),
    role("gum specialist", Dental, &["1223P0300X"]),
    role("prosthodontist", Dental, &["1223P0700X"]),
    role("orthodontist", Dental, &["1223X0400X"]),
    role("dental hygienist", Dental, &["124Q"]),
    role("dental assistant", Support, &["1268"]),
    role("optician", Support, &["156F"]),
    role("podiatrist", Foot, &["213E"]),
    role("chiropractor", Chiropractic, &["111N"]),
    // Therapy.
    role("physical therapist", Therapy, &["2251"]),
    role("physical therapist assistant", Therapy, &["2252"]),
    role("athletic trainer", Support, &["2255A"]),
    role("massage therapist", Therapy, &["2257"]),
    role("occupational therapist", Therapy, &["225X"]),
    role("occupational therapy assistant", Therapy, &["224Z"]),
    role("speech therapist", Therapy, &["235Z"]),
    role("speech therapy assistant", Therapy, &["2355"]),
    role("audiologist", Therapy, &["231H", "2376"]),
    role("hearing aid specialist", Support, &["2377"]),
    role("respiratory therapist", Therapy, &["2278", "2279"]),
    role("developmental therapist", Therapy, &["222Q"]),
    // Others a person sees.
    role("dietitian", Allied, &["133V"]),
    role("nutritionist", Allied, &["133N"]),
    role("acupuncturist", Allied, &["1711"]),
    role("naturopathic doctor", Allied, &["175F"]),
    role("genetic counselor", Allied, &["1703"]),
    role("health educator", Allied, &["174H"]),
    role("health coach", Allied, &["1714"]),
    role("doula", Support, &["374J"]),
    role("community health worker", Support, &["172V"]),
    role("case manager", Support, &["171M"]),
    role("home health aide", Support, &["374U", "376K", "3747"]),
    role("caregiver", Support, &["3725", "3726", "376J", "373H"]),
    role("pharmacist", Pharmacist, &["1835"]),
    role("pharmacy technician", Support, &["1837"]),
    // Places.
    role(
        "medical group",
        Organization,
        &["1932", "1934", "261QM1300X", "261QM2500X"],
    ),
    role("hospital", Organization, &["281P", "282N", "2843", "2865"]),
    role(
        "children's hospital",
        Organization,
        &["281PC2000X", "282NC2000X", "283XC2000X"],
    ),
    role("long-term care hospital", Organization, &["282E"]),
    private(
        "psychiatric hospital",
        Organization,
        MENTAL_HEALTH,
        &["283Q", "273R"],
    ),
    role("rehabilitation hospital", Organization, &["283X", "273Y"]),
    private(
        "substance use treatment center",
        Organization,
        SUBSTANCE_USE,
        &["2764", "3245", "261QM2800X", "261QR0405X", "261QR0800X"],
    ),
    role("clinic", Organization, &["261Q"]),
    private(
        "family planning clinic",
        Organization,
        REPRODUCTIVE,
        &["261QA0005X", "261QF0050X"],
    ),
    private(
        "fertility clinic",
        Organization,
        REPRODUCTIVE,
        &["261QA0006X"],
    ),
    private(
        "mental health clinic",
        Organization,
        MENTAL_HEALTH,
        &["261QM0801X", "261QM0850X", "261QM0855X"],
    ),
    role("surgery center", Organization, &["261QA1903X"]),
    role("endoscopy center", Organization, &["261QE0800X"]),
    role("birth center", Organization, &["261QB0400X"]),
    role(
        "community health center",
        Organization,
        &["261QC1500X", "261QF0400X", "261QR1300X"],
    ),
    role("primary care clinic", Organization, &["261QP2300X"]),
    role("urgent care", Organization, &["261QU0200X"]),
    role("emergency room", Organization, &["261QE0002X"]),
    role("dental clinic", Organization, &["261QD0000X"]),
    role("dialysis center", Organization, &["261QE0700X"]),
    role("infusion center", Organization, &["261QI0500X"]),
    role("physical therapy clinic", Organization, &["261QP2000X"]),
    role(
        "rehabilitation clinic",
        Organization,
        &["261QR0400X", "261QR0401X", "261QR0404X"],
    ),
    role(
        "imaging center",
        Organization,
        &[
            "261QM1200X",
            "261QR0200X",
            "261QR0206X",
            "261QR0207X",
            "261QR0208X",
            "335V",
        ],
    ),
    role("pain clinic", Organization, &["261QP3300X"]),
    role("sleep center", Organization, &["261QS1200X"]),
    role("cancer center", Organization, &["261QX0200X", "261QX0203X"]),
    role("hearing and speech clinic", Organization, &["261QH0700X"]),
    role("adult day care", Organization, &["261QA0600X"]),
    role("laboratory", Organization, &["291"]),
    role("dental laboratory", Organization, &["2922"]),
    role("pharmacy", Organization, &["3336"]),
    role("mail-order pharmacy", Organization, &["3336M0002X"]),
    role("specialty pharmacy", Organization, &["3336S0011X"]),
    role("infusion pharmacy", Organization, &["3336H0001X", "251F"]),
    role("medical equipment supplier", Organization, &["332B"]),
    role("eyewear supplier", Organization, &["332H"]),
    role("hearing aid supplier", Organization, &["332S"]),
    role("prosthetics supplier", Organization, &["335E"]),
    role("ambulance service", Organization, &["3416"]),
    role("medical transportation", Organization, &["3439"]),
    role("home health agency", Organization, &["251E"]),
    role("home care agency", Organization, &["253Z", "251J"]),
    role("hospice", Organization, &["251G", "315D"]),
    role("public health department", Organization, &["251K"]),
    private(
        "behavioral health agency",
        Organization,
        MENTAL_HEALTH,
        &["251S"],
    ),
    private(
        "mental health residence",
        Organization,
        MENTAL_HEALTH,
        &["3208", "322D", "323P"],
    ),
    role("early intervention agency", Organization, &["252Y"]),
    role("school district", Organization, &["2513"]),
    role("nursing home", Organization, &["313M", "314", "311Z"]),
    role("assisted living", Organization, &["3104"]),
    role("health plan", Organization, &["302R", "305"]),
];

/// An organization under a code of a person's, in words.
const PRACTICE: &str = "'s practice";

/// The role of a taxonomy code.
pub fn of(code: &str) -> Option<&'static Role> {
    ROLES
        .iter()
        .flat_map(|r| r.codes.iter().map(move |c| (c, r)))
        .filter(|(c, _)| code.starts_with(**c))
        .max_by_key(|(c, _)| c.len())
        .map(|(_, r)| r)
}

/// A provider's role in words. An organization under a person's code is that
/// person's practice.
pub fn words(code: &str, organization: bool) -> Option<String> {
    let r = of(code)?;
    Some(if organization && r.group != Organization {
        format!("{}{PRACTICE}", r.words)
    } else {
        r.words.to_string()
    })
}

/// The role these words are of.
pub fn named(words: &str) -> Option<&'static Role> {
    let w = words.strip_suffix(PRACTICE).unwrap_or(words);
    ROLES.iter().find(|r| r.words.eq_ignore_ascii_case(w))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn the_longest_code_decides() {
        for (code, words) in [
            ("2085R0202X", "radiologist"),
            ("2085R0001X", "radiation oncologist"),
            ("207R00000X", "internal medicine doctor"),
            ("207RC0000X", "cardiologist"),
            ("363LF0000X", "nurse practitioner"),
            ("363LP0808X", "psychiatric nurse practitioner"),
            ("291U00000X", "laboratory"),
            ("3336C0003X", "pharmacy"),
        ] {
            assert_eq!(of(code).map(|r| r.words), Some(words), "{code}");
        }
        for code in ["", "390200000X", "174400000X", "ZZZ"] {
            assert!(of(code).is_none(), "{code}");
        }
    }

    #[test]
    fn an_organization_under_a_persons_code_is_their_practice() {
        assert_eq!(
            words("207V00000X", true).as_deref(),
            Some("OB-GYN's practice")
        );
        assert_eq!(words("207V00000X", false).as_deref(), Some("OB-GYN"));
        assert_eq!(words("282N00000X", true).as_deref(), Some("hospital"));
        assert_eq!(
            named("psychiatrist's practice").and_then(|r| r.area),
            Some(MENTAL_HEALTH)
        );
        assert_eq!(
            named("Psychiatrist").and_then(|r| r.area),
            Some(MENTAL_HEALTH)
        );
        assert!(named("sample role").is_none());
    }

    /// A machine with no directory reads a role, and its area, back from its
    /// words alone.
    #[test]
    fn each_role_has_its_own_words_and_codes() {
        let words: BTreeSet<&str> = ROLES.iter().map(|r| r.words).collect();
        assert_eq!(words.len(), ROLES.len());
        let codes: Vec<&str> = ROLES.iter().flat_map(|r| r.codes).copied().collect();
        assert_eq!(codes.iter().collect::<BTreeSet<_>>().len(), codes.len());
        for c in codes {
            assert!((3..=10).contains(&c.len()), "{c}");
            assert!(c
                .bytes()
                .all(|b| b.is_ascii_digit() || b.is_ascii_uppercase()));
        }
    }

    #[test]
    fn a_service_fits_those_who_give_it() {
        let fits = |s: Service, code: &str| s.fits(of(code).unwrap().group);
        assert!(fits(Service::Anesthesia, "207L00000X"));
        assert!(fits(Service::Anesthesia, "367500000X"));
        assert!(!fits(Service::Anesthesia, "207Q00000X"));
        assert!(fits(Service::Dental, "122300000X"));
        assert!(!fits(Service::Dental, "207Q00000X"));
        assert!(!fits(Service::Visit, "106S00000X"));
        assert!(fits(Service::Any, "106S00000X"));
        assert!(!fits(Service::Any, "282N00000X"));
        assert!(fits(Service::Vaccine, "183500000X"));
        assert!(!fits(Service::Prescription, "183500000X"));
    }
}
