//! Cleanup rules: category, risk, destination, and minimum age in an embedded TOML file (PLAN.md §2.5 and §2.6).
//!
//! Risk and destination are separate: risk determines whether an item can be preselected; destination determines what
//! happens to it (permanent deletion is limited to regenerable items classified by a rule, or it goes to the Recycle Bin).

use serde::Deserialize;
use std::collections::HashMap;
use std::sync::OnceLock;
use std::time::Duration;

/// Risk level for a category.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Risk {
    /// Can be preselected and included in "Clean safe items".
    Safe,
    /// Is never preselected.
    Review,
    /// Information only: no checkbox or destination.
    Info,
}

/// Destination for an item.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Destination {
    /// Is deleted and recreated automatically.
    Permanent,
    /// Goes to the Recycle Bin.
    Recycle,
}

/// Action performed for a category.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Action {
    /// Deletes children of category roots (the roots themselves are not deleted).
    Contents,
    /// Empties the Recycle Bin within an enumerated scope.
    EmptyBin,
    /// None (information is shown or a "View" action is offered).
    None,
}

/// MVP category identifier.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum CategoryId {
    UserTemp,
    WindowsTemp,
    ChromeCache,
    EdgeCache,
    FirefoxCache,
    RecycleBin,
    Downloads,
    /// User Windows Error Reporting folders (`ReportArchive` and `ReportQueue`).
    WerReports,
    /// Application crash dumps (`%LOCALAPPDATA%\CrashDumps`).
    CrashDumps,
    Pagefile,
    Hiberfil,
    Swapfile,
}

impl CategoryId {
    pub const ALL: [CategoryId; 12] = [
        CategoryId::UserTemp,
        CategoryId::WindowsTemp,
        CategoryId::ChromeCache,
        CategoryId::EdgeCache,
        CategoryId::FirefoxCache,
        CategoryId::RecycleBin,
        CategoryId::Downloads,
        CategoryId::WerReports,
        CategoryId::CrashDumps,
        CategoryId::Pagefile,
        CategoryId::Hiberfil,
        CategoryId::Swapfile,
    ];

    /// Text identifier used in TOML and UI strings.
    pub fn key(self) -> &'static str {
        match self {
            CategoryId::UserTemp => "user-temp",
            CategoryId::WindowsTemp => "windows-temp",
            CategoryId::ChromeCache => "chrome-cache",
            CategoryId::EdgeCache => "edge-cache",
            CategoryId::FirefoxCache => "firefox-cache",
            CategoryId::RecycleBin => "recycle-bin",
            CategoryId::Downloads => "downloads",
            CategoryId::WerReports => "wer-reports",
            CategoryId::CrashDumps => "crash-dumps",
            CategoryId::Pagefile => "pagefile",
            CategoryId::Hiberfil => "hiberfil",
            CategoryId::Swapfile => "swapfile",
        }
    }

    pub fn from_key(k: &str) -> Option<CategoryId> {
        CategoryId::ALL.into_iter().find(|c| c.key() == k)
    }

    /// Is this a user diagnostic category (error reports or crash dumps)? It has "Review" risk and a Recycle Bin
    /// destination, with a checkbox; it is never preselected.
    pub fn is_diagnostic(self) -> bool {
        matches!(self, CategoryId::WerReports | CategoryId::CrashDumps)
    }

    /// Is this a browser cache?
    pub fn is_browser_cache(self) -> bool {
        matches!(
            self,
            CategoryId::ChromeCache | CategoryId::EdgeCache | CategoryId::FirefoxCache
        )
    }
}

/// A validated rule.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rule {
    pub id: CategoryId,
    pub risk: Risk,
    pub destination: Option<Destination>,
    pub action: Action,
    /// Only files older than this are touched (0 = no limit).
    pub min_age: Duration,
    /// Has a checkbox in Recommendations.
    pub selectable: bool,
}

impl Rule {
    /// Is it preselected? Only safe items with a checkbox; review items never are.
    pub fn preselected(&self) -> bool {
        self.risk == Risk::Safe && self.selectable
    }
}

#[derive(Deserialize)]
struct RawRule {
    id: String,
    risk: Risk,
    destination: Option<Destination>,
    action: Action,
    #[serde(default)]
    min_age_hours: u64,
    #[serde(default)]
    selectable: bool,
}

#[derive(Deserialize)]
struct RawFile {
    rule: Vec<RawRule>,
}

/// Embedded rule source.
pub const RULES_TOML: &str = include_str!("../data/rules.toml");

/// The rule set.
#[derive(Clone, Debug)]
pub struct Rules {
    by_id: HashMap<CategoryId, Rule>,
}

impl Rules {
    /// Parses and validates a rule table. Returns a reason if any rule is inconsistent.
    pub fn parse(src: &str) -> Result<Rules, String> {
        let raw: RawFile = toml::from_str(src).map_err(|e| e.to_string())?;
        let mut by_id = HashMap::new();
        for r in raw.rule {
            let id =
                CategoryId::from_key(&r.id).ok_or_else(|| format!("unknown category: {}", r.id))?;
            let rule = Rule {
                id,
                risk: r.risk,
                destination: r.destination,
                action: r.action,
                min_age: Duration::from_secs(r.min_age_hours * 3600),
                selectable: r.selectable,
            };
            validate(&rule)?;
            if by_id.insert(id, rule).is_some() {
                return Err(format!("duplicate rule: {}", r.id));
            }
        }
        for id in CategoryId::ALL {
            if !by_id.contains_key(&id) {
                return Err(format!("missing rule for {}", id.key()));
            }
        }
        Ok(Rules { by_id })
    }

    /// Rule for `id` (all IDs are present; this is checked by [`Rules::parse`]).
    pub fn get(&self, id: CategoryId) -> &Rule {
        &self.by_id[&id]
    }

    pub fn iter(&self) -> impl Iterator<Item = &Rule> {
        CategoryId::ALL.into_iter().map(|id| &self.by_id[&id])
    }
}

/// Security invariants for a rule (PLAN.md §2.5 and §2.6).
fn validate(r: &Rule) -> Result<(), String> {
    let k = r.id.key();
    match r.risk {
        Risk::Info => {
            if r.destination.is_some() || r.action != Action::None || r.selectable {
                return Err(format!(
                    "{k}: information-only rules cannot have a destination, action, or checkbox"
                ));
            }
        }
        Risk::Safe => {
            if r.action != Action::Contents || !r.selectable || r.destination.is_none() {
                return Err(format!(
                    "{k}: safe rules must delete root contents, have a destination, and be selectable"
                ));
            }
        }
        Risk::Review => {}
    }
    // Permanent deletion is only for regenerable items classified by a rule, or for the empty-bin action itself.
    if r.destination == Some(Destination::Permanent)
        && !(r.action == Action::Contents && r.risk == Risk::Safe || r.action == Action::EmptyBin)
    {
        return Err(format!(
            "{k}: permanent deletion is only for rule-classified regenerable items or for emptying the Recycle Bin"
        ));
    }
    if r.action == Action::EmptyBin && r.risk == Risk::Safe {
        return Err(format!(
            "{k}: emptying the Recycle Bin is not considered safe"
        ));
    }
    if r.id.is_diagnostic()
        && (r.risk != Risk::Review || r.destination != Some(Destination::Recycle))
    {
        return Err(format!(
            "{k}: error reports and crash dumps must have Review risk and go to the Recycle Bin; they are never safe"
        ));
    }
    // M-1/m-7: these categories are safe for Recycle Bin cleanup only with a minimum age (required by `prepare_recycle`
    // and `fresh_size`) and when deleting the contents of their roots.
    if r.id.is_diagnostic()
        && (r.min_age != Duration::from_secs(7 * 24 * 3600) || r.action != Action::Contents)
    {
        // Exactly 7 days: the UI text always says "7 days".
        return Err(format!(
            "{k}: error reports and crash dumps require action = contents and min_age_hours = 168"
        ));
    }
    if r.id == CategoryId::Downloads && (r.risk == Risk::Safe || r.selectable) {
        return Err("downloads: es informativo, nunca «seguro» ni con casilla".into());
    }
    Ok(())
}

/// Embedded rules (already validated; a test checks this).
pub fn rules() -> &'static Rules {
    static R: OnceLock<Rules> = OnceLock::new();
    R.get_or_init(|| {
        Rules::parse(RULES_TOML).expect("rules.toml embebido inválido (lo cubre un test)")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn las_reglas_embebidas_son_validas_y_completas() {
        let r = Rules::parse(RULES_TOML).expect("válidas");
        assert_eq!(r.iter().count(), CategoryId::ALL.len());
        for id in CategoryId::ALL {
            assert_eq!(r.get(id).id, id);
            assert_eq!(CategoryId::from_key(id.key()), Some(id));
        }
        let _ = rules();
    }

    #[test]
    fn temporales_solo_los_de_mas_de_7_dias_y_cachas_sin_limite() {
        let r = rules();
        assert_eq!(
            r.get(CategoryId::UserTemp).min_age,
            Duration::from_secs(7 * 24 * 3600)
        );
        assert_eq!(
            r.get(CategoryId::WindowsTemp).min_age,
            Duration::from_secs(7 * 24 * 3600)
        );
        assert_eq!(r.get(CategoryId::ChromeCache).min_age, Duration::ZERO);
    }

    #[test]
    fn lo_seguro_es_regenerable_y_se_preselecciona_y_lo_demas_no() {
        let r = rules();
        for rule in r.iter() {
            match rule.risk {
                Risk::Safe => {
                    assert!(rule.preselected());
                    assert_eq!(rule.destination, Some(Destination::Permanent));
                }
                Risk::Review | Risk::Info => assert!(!rule.preselected(), "{:?}", rule.id),
            }
        }
        // Downloads and the Recycle Bin: Review, never "safe".
        assert_eq!(r.get(CategoryId::Downloads).risk, Risk::Review);
        assert_eq!(
            r.get(CategoryId::Downloads).destination,
            Some(Destination::Recycle)
        );
        assert_eq!(r.get(CategoryId::RecycleBin).risk, Risk::Review);
        assert_eq!(r.get(CategoryId::RecycleBin).action, Action::EmptyBin);
        // Information only: no destination.
        for id in [
            CategoryId::Pagefile,
            CategoryId::Hiberfil,
            CategoryId::Swapfile,
        ] {
            assert_eq!(r.get(id).risk, Risk::Info);
            assert_eq!(r.get(id).destination, None);
        }
    }

    #[test]
    fn informes_y_volcados_son_revisar_a_la_papelera_con_7_dias_y_sin_preseleccionar() {
        let r = rules();
        for id in [CategoryId::WerReports, CategoryId::CrashDumps] {
            let rule = r.get(id);
            assert!(id.is_diagnostic() && !id.is_browser_cache());
            assert_eq!(rule.risk, Risk::Review, "{id:?}");
            assert_eq!(rule.destination, Some(Destination::Recycle), "{id:?}");
            assert_eq!(rule.action, Action::Contents, "{id:?}");
            assert_eq!(rule.min_age, Duration::from_secs(7 * 24 * 3600), "{id:?}");
            assert!(rule.selectable && !rule.preselected(), "{id:?}");
        }
        assert_eq!(CategoryId::WerReports.key(), "wer-reports");
        assert_eq!(CategoryId::CrashDumps.key(), "crash-dumps");
        // No other category is diagnostic.
        assert_eq!(
            CategoryId::ALL.iter().filter(|c| c.is_diagnostic()).count(),
            2
        );
    }

    fn with(extra: &str) -> String {
        // A minimal valid table plus a test rule (replaces the existing rule with that id).
        let mut s = String::new();
        for rule in rules().iter() {
            if extra.contains(&format!("id = \"{}\"", rule.id.key())) {
                continue;
            }
            s.push_str(&format!(
                "[[rule]]\nid = \"{}\"\nrisk = \"{}\"\n{}action = \"{}\"\nmin_age_hours = {}\nselectable = {}\n",
                rule.id.key(),
                match rule.risk {
                    Risk::Safe => "safe",
                    Risk::Review => "review",
                    Risk::Info => "info",
                },
                match rule.destination {
                    Some(Destination::Permanent) => "destination = \"permanent\"\n",
                    Some(Destination::Recycle) => "destination = \"recycle\"\n",
                    None => "",
                },
                match rule.action {
                    Action::Contents => "contents",
                    Action::EmptyBin => "empty-bin",
                    Action::None => "none",
                },
                rule.min_age.as_secs() / 3600,
                rule.selectable
            ));
        }
        s.push_str(extra);
        s
    }

    #[test]
    fn la_validacion_rechaza_lo_peligroso() {
        // Descargas "seguro".
        let e = Rules::parse(&with("[[rule]]\nid = \"downloads\"\nrisk = \"safe\"\ndestination = \"permanent\"\naction = \"contents\"\nselectable = true\n")).unwrap_err();
        assert!(e.contains("downloads"), "{e}");
        // Permanent destination for something not marked regenerable by a rule.
        let e = Rules::parse(&with("[[rule]]\nid = \"downloads\"\nrisk = \"review\"\ndestination = \"permanent\"\naction = \"none\"\n")).unwrap_err();
        assert!(e.contains("permanent deletion"), "{e}");
        // Error reports or dumps: neither permanent (they are not regenerable) nor "safe" (which would preselect them).
        for id in ["wer-reports", "crash-dumps"] {
            let rule = |risk: &str, dest: &str| {
                with(&format!(
                    "[[rule]]\nid = \"{id}\"\nrisk = \"{risk}\"\ndestination = \"{dest}\"\naction = \"contents\"\nselectable = true\n"
                ))
            };
            let e = Rules::parse(&rule("review", "permanent")).unwrap_err();
            assert!(e.contains("permanent deletion"), "{e}");
            let e = Rules::parse(&rule("safe", "recycle")).unwrap_err();
            assert!(e.contains(id) && e.contains("Review risk"), "{e}");
            // m-7: a minimum age other than 7 days, or not deleting the root's contents, is also invalid.
            for (hours, action) in [
                (0, "contents"),
                (167, "contents"),
                (169, "contents"),
                (168, "none"),
            ] {
                let e = Rules::parse(&with(&format!(
                    "[[rule]]\nid = \"{id}\"\nrisk = \"review\"\ndestination = \"recycle\"\naction = \"{action}\"\nmin_age_hours = {hours}\nselectable = true\n"
                )))
                .unwrap_err();
                assert!(e.contains(id) && e.contains("168"), "{hours} {action}: {e}");
            }
        }
        // "Information only" with a destination.
        let e = Rules::parse(&with("[[rule]]\nid = \"pagefile\"\nrisk = \"info\"\ndestination = \"recycle\"\naction = \"none\"\n")).unwrap_err();
        assert!(e.contains("information-only"), "{e}");
        // Emptying the Recycle Bin as "safe".
        let e = Rules::parse(&with("[[rule]]\nid = \"recycle-bin\"\nrisk = \"safe\"\ndestination = \"permanent\"\naction = \"empty-bin\"\nselectable = true\n")).unwrap_err();
        assert!(e.contains("safe rules") || e.contains("Recycle Bin"), "{e}");
        // A duplicate category, an unknown one, and a missing one.
        assert!(
            Rules::parse(&format!(
                "{}{}",
                RULES_TOML, "[[rule]]\nid = \"user-temp\"\nrisk = \"info\"\naction = \"none\"\n"
            ))
            .unwrap_err()
            .contains("duplicate")
        );
        assert!(
            Rules::parse(&with(
                "[[rule]]\nid = \"inventada\"\nrisk = \"info\"\naction = \"none\"\n"
            ))
            .unwrap_err()
            .contains("unknown category")
        );
        assert!(
            Rules::parse("[[rule]]\nid = \"user-temp\"\nrisk = \"info\"\naction = \"none\"\n")
                .unwrap_err()
                .contains("missing rule")
        );
        assert!(Rules::parse("esto no es toml [").is_err());
    }
}
