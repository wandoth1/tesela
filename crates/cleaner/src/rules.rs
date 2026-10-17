//! Reglas de limpieza: categoría, riesgo, destino y antigüedad mínima, en un TOML embebido (PLAN.md §2.5 y §2.6).
//!
//! Riesgo y destino son dos ejes distintos: el riesgo dice si se puede preseleccionar; el destino dice qué le pasa al
//! elemento (se borra definitivamente, solo lo regenerable que clasifica una regla, o va a la Papelera).

use serde::Deserialize;
use std::collections::HashMap;
use std::sync::OnceLock;
use std::time::Duration;

/// Riesgo de una categoría.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Risk {
    /// Se puede preseleccionar y entra en "Limpiar lo seguro".
    Safe,
    /// Nunca se preselecciona.
    Review,
    /// Solo informar: sin casilla ni destino.
    Info,
}

/// Destino de un elemento.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Destination {
    /// Se elimina y se vuelve a crear sola.
    Permanent,
    /// Va a la Papelera de reciclaje.
    Recycle,
}

/// Qué hace la categoría al ejecutarse.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Action {
    /// Borra los hijos de las raíces de la categoría (las raíces no se borran).
    Contents,
    /// Vacía la Papelera con alcance enumerado.
    EmptyBin,
    /// Ninguna (se informa o se ofrece "Ver").
    None,
}

/// Identificador de categoría del MVP.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum CategoryId {
    UserTemp,
    WindowsTemp,
    ChromeCache,
    EdgeCache,
    FirefoxCache,
    RecycleBin,
    Downloads,
    /// Informes de errores de Windows del usuario (`ReportArchive` y `ReportQueue`).
    WerReports,
    /// Volcados de bloqueo de aplicaciones (`%LOCALAPPDATA%\CrashDumps`).
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

    /// Identificador de texto (el del TOML y de las cadenas de la interfaz).
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

    /// ¿Es una categoría de diagnóstico del usuario (informes de errores y volcados)? Riesgo "Revisar", con casilla y
    /// destino Papelera: nunca se preselecciona.
    pub fn is_diagnostic(self) -> bool {
        matches!(self, CategoryId::WerReports | CategoryId::CrashDumps)
    }

    /// ¿Es una caché de navegador?
    pub fn is_browser_cache(self) -> bool {
        matches!(
            self,
            CategoryId::ChromeCache | CategoryId::EdgeCache | CategoryId::FirefoxCache
        )
    }
}

/// Una regla ya validada.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rule {
    pub id: CategoryId,
    pub risk: Risk,
    pub destination: Option<Destination>,
    pub action: Action,
    /// Solo se tocan los archivos más antiguos que esto (0 = sin límite).
    pub min_age: Duration,
    /// Tiene casilla en Recomendaciones.
    pub selectable: bool,
}

impl Rule {
    /// ¿Se preselecciona? Solo lo seguro que tiene casilla: lo "revisar" no, nunca.
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

/// Fuente embebida de las reglas.
pub const RULES_TOML: &str = include_str!("../data/rules.toml");

/// El conjunto de reglas.
#[derive(Clone, Debug)]
pub struct Rules {
    by_id: HashMap<CategoryId, Rule>,
}

impl Rules {
    /// Interpreta y valida una tabla de reglas. Devuelve el motivo si algo es incoherente.
    pub fn parse(src: &str) -> Result<Rules, String> {
        let raw: RawFile = toml::from_str(src).map_err(|e| e.to_string())?;
        let mut by_id = HashMap::new();
        for r in raw.rule {
            let id = CategoryId::from_key(&r.id)
                .ok_or_else(|| format!("categoría desconocida: {}", r.id))?;
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
                return Err(format!("regla repetida: {}", r.id));
            }
        }
        for id in CategoryId::ALL {
            if !by_id.contains_key(&id) {
                return Err(format!("falta la regla de {}", id.key()));
            }
        }
        Ok(Rules { by_id })
    }

    /// La regla de `id` (todas existen: lo valida [`Rules::parse`]).
    pub fn get(&self, id: CategoryId) -> &Rule {
        &self.by_id[&id]
    }

    pub fn iter(&self) -> impl Iterator<Item = &Rule> {
        CategoryId::ALL.into_iter().map(|id| &self.by_id[&id])
    }
}

/// Invariantes de seguridad de una regla (PLAN.md §2.5 y §2.6).
fn validate(r: &Rule) -> Result<(), String> {
    let k = r.id.key();
    match r.risk {
        Risk::Info => {
            if r.destination.is_some() || r.action != Action::None || r.selectable {
                return Err(format!(
                    "{k}: «solo informar» no lleva destino, acción ni casilla"
                ));
            }
        }
        Risk::Safe => {
            if r.action != Action::Contents || !r.selectable || r.destination.is_none() {
                return Err(format!(
                    "{k}: lo seguro borra el contenido de unas raíces con destino y tiene casilla"
                ));
            }
        }
        Risk::Review => {}
    }
    // El destino definitivo es solo para lo regenerable clasificado por una regla, o la propia acción de vaciar.
    if r.destination == Some(Destination::Permanent)
        && !(r.action == Action::Contents && r.risk == Risk::Safe || r.action == Action::EmptyBin)
    {
        return Err(format!(
            "{k}: el destino definitivo solo es para lo regenerable por regla o para vaciar la Papelera"
        ));
    }
    if r.action == Action::EmptyBin && r.risk == Risk::Safe {
        return Err(format!("{k}: vaciar la Papelera no es «seguro»"));
    }
    if r.id.is_diagnostic()
        && (r.risk != Risk::Review || r.destination != Some(Destination::Recycle))
    {
        return Err(format!(
            "{k}: los informes de errores y los volcados son «Revisar» y van a la Papelera, nunca «seguros»"
        ));
    }
    // M-1/m-7: la Papelera por elementos de estas categorías solo es seguro con una antigüedad mínima (la comparación de
    // `prepare_recycle` y de `fresh_size` la exige) y borrando el contenido de las raíces.
    if r.id.is_diagnostic()
        && (r.min_age != Duration::from_secs(7 * 24 * 3600) || r.action != Action::Contents)
    {
        // Exactamente 7 días: los textos de la interfaz dicen "7 días" fijo.
        return Err(format!(
            "{k}: los informes de errores y los volcados exigen action = contents y min_age_hours = 168"
        ));
    }
    if r.id == CategoryId::Downloads && (r.risk == Risk::Safe || r.selectable) {
        return Err("downloads: es informativo, nunca «seguro» ni con casilla".into());
    }
    Ok(())
}

/// Las reglas embebidas (ya validadas; un test lo comprueba).
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
        // Descargas y la Papelera: Revisar, nunca "seguro".
        assert_eq!(r.get(CategoryId::Downloads).risk, Risk::Review);
        assert_eq!(
            r.get(CategoryId::Downloads).destination,
            Some(Destination::Recycle)
        );
        assert_eq!(r.get(CategoryId::RecycleBin).risk, Risk::Review);
        assert_eq!(r.get(CategoryId::RecycleBin).action, Action::EmptyBin);
        // Solo informar: sin destino.
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
        // Ninguna otra categoría es de diagnóstico.
        assert_eq!(
            CategoryId::ALL.iter().filter(|c| c.is_diagnostic()).count(),
            2
        );
    }

    fn with(extra: &str) -> String {
        // Una tabla mínima válida más una regla de prueba (sustituye a la existente con ese id).
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
        // Destino definitivo en algo que no es regenerable por regla.
        let e = Rules::parse(&with("[[rule]]\nid = \"downloads\"\nrisk = \"review\"\ndestination = \"permanent\"\naction = \"none\"\n")).unwrap_err();
        assert!(e.contains("definitivo"), "{e}");
        // Informes de errores o volcados: ni definitivos (no son regenerables) ni "seguros" (se preseleccionarían).
        for id in ["wer-reports", "crash-dumps"] {
            let rule = |risk: &str, dest: &str| {
                with(&format!(
                    "[[rule]]\nid = \"{id}\"\nrisk = \"{risk}\"\ndestination = \"{dest}\"\naction = \"contents\"\nselectable = true\n"
                ))
            };
            let e = Rules::parse(&rule("review", "permanent")).unwrap_err();
            assert!(e.contains("definitivo"), "{e}");
            let e = Rules::parse(&rule("safe", "recycle")).unwrap_err();
            assert!(e.contains(id) && e.contains("Revisar"), "{e}");
            // m-7: sin antigüedad mínima de 7 días, o sin borrar el contenido de la raíz, tampoco valen.
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
        // «Solo informar» con destino.
        let e = Rules::parse(&with("[[rule]]\nid = \"pagefile\"\nrisk = \"info\"\ndestination = \"recycle\"\naction = \"none\"\n")).unwrap_err();
        assert!(e.contains("solo informar"), "{e}");
        // Vaciar la Papelera como «seguro».
        let e = Rules::parse(&with("[[rule]]\nid = \"recycle-bin\"\nrisk = \"safe\"\ndestination = \"permanent\"\naction = \"empty-bin\"\nselectable = true\n")).unwrap_err();
        assert!(e.contains("seguro") || e.contains("Papelera"), "{e}");
        // Una categoría repetida, una desconocida y una que falta.
        assert!(
            Rules::parse(&format!(
                "{}{}",
                RULES_TOML, "[[rule]]\nid = \"user-temp\"\nrisk = \"info\"\naction = \"none\"\n"
            ))
            .unwrap_err()
            .contains("repetida")
        );
        assert!(
            Rules::parse(&with(
                "[[rule]]\nid = \"inventada\"\nrisk = \"info\"\naction = \"none\"\n"
            ))
            .unwrap_err()
            .contains("desconocida")
        );
        assert!(
            Rules::parse("[[rule]]\nid = \"user-temp\"\nrisk = \"info\"\naction = \"none\"\n")
                .unwrap_err()
                .contains("falta")
        );
        assert!(Rules::parse("esto no es toml [").is_err());
    }
}
