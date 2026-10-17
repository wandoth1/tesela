//! Minimal persistence (PLAN.md §2.11): `%APPDATA%\Tesela\config.toml` stores the theme, language, and window size.
//! The tree and scan data are never saved. An unreadable configuration is ignored (defaults are used) rather than
//! preventing startup.

use std::path::{Path, PathBuf};
use tesela_ui::{LangPref, Settings, ThemePref, UpdatePrefs};
use tesela_update::Version;

/// Default and minimum window sizes, in logical points.
pub const DEFAULT_SIZE: (f32, f32) = (1280.0, 800.0);
pub const MIN_SIZE: (f32, f32) = (960.0, 600.0);

#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Config {
    #[serde(default)]
    pub theme: Option<ThemePref>,
    #[serde(default)]
    pub language: Option<LangPref>,
    #[serde(default)]
    pub window_width: Option<f32>,
    #[serde(default)]
    pub window_height: Option<f32>,
    /// "Check for updates when Tesela starts." If this key is absent or `false`, Tesela connects to nothing.
    #[serde(default, deserialize_with = "lenient")]
    pub check_updates: Option<bool>,
    /// Unix timestamp of the last check attempt (at most one request every 24 hours).
    #[serde(default, deserialize_with = "lenient")]
    pub update_last_check: Option<u64>,
    /// Most recently seen published version (tag only, `vX.Y.Z`). An unreadable tag is ignored.
    #[serde(default, deserialize_with = "tesela_update::deserialize_lenient")]
    pub update_latest_seen: Option<Version>,
    /// Version whose notification was dismissed.
    #[serde(default, deserialize_with = "tesela_update::deserialize_lenient")]
    pub update_dismissed: Option<Version>,
}

/// For a NEW key (update checking), a value of the wrong type (for example `check_updates = "yes"` or a negative timestamp
/// from manual editing) is treated as absent instead of invalidating the ENTIRE configuration, including theme, language,
/// and window size.
fn lenient<'de, D, T>(d: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::Deserialize<'de>,
{
    #[derive(serde::Deserialize)]
    #[serde(untagged)]
    enum Any<T> {
        Value(T),
        Other(serde::de::IgnoredAny),
    }
    Ok(match <Any<T> as serde::Deserialize>::deserialize(d)? {
        Any::Value(v) => Some(v),
        Any::Other(_) => None,
    })
}

impl Config {
    /// UI settings stored in this configuration.
    pub fn settings(&self) -> Settings {
        Settings {
            theme: self.theme.unwrap_or(ThemePref::System),
            language: self.language.unwrap_or(LangPref::System),
            update: UpdatePrefs {
                check: self.check_updates.unwrap_or(false),
                last_check: self.update_last_check,
                latest_seen: self.update_latest_seen,
                dismissed: self.update_dismissed,
            },
        }
    }

    /// Records ONLY settings that changed between `before` and `after`: settings the user did not touch (for example a
    /// language set with `--lang` for one session) are not written to the file.
    pub fn apply_changes(&mut self, before: Settings, after: Settings) {
        if after.theme != before.theme {
            self.theme = Some(after.theme);
        }
        if after.language != before.language {
            self.language = Some(after.language);
        }
        let (b, a) = (before.update, after.update);
        if a.check != b.check {
            self.check_updates = Some(a.check);
        }
        if a.last_check != b.last_check {
            self.update_last_check = a.last_check;
        }
        if a.latest_seen != b.latest_seen {
            self.update_latest_seen = a.latest_seen;
        }
        if a.dismissed != b.dismissed {
            self.update_dismissed = a.dismissed;
        }
    }

    /// Saved window size, clamped to reasonable values.
    pub fn window_size(&self) -> (f32, f32) {
        match (self.window_width, self.window_height) {
            (Some(w), Some(h)) if w.is_finite() && h.is_finite() => {
                (w.clamp(MIN_SIZE.0, 8192.0), h.clamp(MIN_SIZE.1, 8192.0))
            }
            _ => DEFAULT_SIZE,
        }
    }

    /// Loads configuration from `path`; uses defaults if the file is missing or invalid.
    pub fn load(path: &Path) -> Config {
        match std::fs::read_to_string(path) {
            Ok(text) => toml::from_str(&text).unwrap_or_else(|e| {
                eprintln!("tesela: config.toml is unreadable; using default values: {e}");
                Config::default()
            }),
            Err(_) => Config::default(),
        }
    }

    /// Saves configuration (creates the directory, writes to a temporary file, then renames it to avoid a partial file).
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let text = toml::to_string_pretty(self).map_err(std::io::Error::other)?;
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, text)?;
        std::fs::rename(&tmp, path)
    }
}

/// Path to `config.toml`: `%APPDATA%\Tesela\config.toml`.
pub fn config_path() -> Option<PathBuf> {
    let appdata = std::env::var_os("APPDATA").filter(|v| !v.is_empty())?;
    Some(PathBuf::from(appdata).join("Tesela").join("config.toml"))
}

/// Configuration for THIS session and where it is saved.
pub struct Session {
    pub config: Config,
    /// `None` means it is not saved anywhere.
    pub path: Option<PathBuf>,
}

impl Session {
    /// An ISOLATED session (the demo and measurement tools): in-memory defaults; reads and writes nothing under `%APPDATA%`.
    /// A normal session reads `config.toml` and saves it when a setting changes and on exit.
    pub fn new(isolated: bool) -> Session {
        Session::with_path(isolated, config_path())
    }

    /// Like [`Session::new`] with an injected `config.toml` path (tests do not touch the process environment).
    pub fn with_path(isolated: bool, path: Option<PathBuf>) -> Session {
        if isolated {
            return Session {
                config: Config::default(),
                path: None,
            };
        }
        let config = path.as_deref().map(Config::load).unwrap_or_default();
        Session { config, path }
    }

    /// Is anything saved? Only in a normal session.
    pub fn persists(&self) -> bool {
        self.path.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Carpeta temporal de la prueba con guarda: se borra al soltarla, también si la prueba entra en pánico (solo si es
    /// hija directa de `%TEMP%` y se llama `tesela-test-*`).
    struct TempDir(PathBuf);

    impl std::ops::Deref for TempDir {
        type Target = std::path::Path;
        fn deref(&self) -> &std::path::Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let ok = self
                .0
                .file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with("tesela-test-"))
                && self.0.parent().is_some_and(|p| p == std::env::temp_dir());
            if ok {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
    }

    fn temp_dir() -> TempDir {
        let d = std::env::temp_dir().join(format!("tesela-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
        TempDir(d)
    }

    #[test]
    fn la_sesion_aislada_ni_lee_ni_guarda() {
        let dir = temp_dir();
        let path = dir.join("Tesela").join("config.toml");
        // Hay una configuración guardada: la sesión aislada la ignora (valores por defecto) y no tiene dónde guardar.
        Config {
            window_width: Some(1234.0),
            ..Config::default()
        }
        .save(&path)
        .unwrap();
        let aislada = Session::with_path(true, Some(path.clone()));
        assert_eq!(aislada.config, Config::default());
        assert!(aislada.path.is_none() && !aislada.persists());
        // La normal sí la lee y guarda en esa ruta.
        let normal = Session::with_path(false, Some(path.clone()));
        assert_eq!(normal.config.window_width, Some(1234.0));
        assert_eq!(normal.path.as_deref(), Some(path.as_path()));
        assert!(normal.persists());
        assert!(!Session::with_path(false, None).persists());
    }

    #[test]
    fn ida_y_vuelta_y_valores_por_defecto() {
        let dir = temp_dir();
        let path = dir.join("Tesela").join("config.toml");
        // Sin archivo: valores por defecto.
        assert_eq!(Config::load(&path), Config::default());
        assert_eq!(Config::default().window_size(), DEFAULT_SIZE);
        let c = Config {
            theme: Some(ThemePref::Dark),
            language: Some(LangPref::En),
            window_width: Some(1400.0),
            window_height: Some(900.0),
            ..Config::default()
        };
        c.save(&path).unwrap();
        assert_eq!(Config::load(&path), c);
        assert_eq!(c.settings().theme, ThemePref::Dark);
        assert_eq!(c.window_size(), (1400.0, 900.0));
        assert!(
            !path.with_extension("toml.tmp").exists(),
            "no queda el temporal"
        );
    }

    #[test]
    fn solo_se_guarda_lo_que_el_usuario_cambia() {
        // Sesión con `--lang en --theme dark` impuestos: el archivo no recibe nada hasta que se cambia algo en la interfaz.
        let saved = Config {
            theme: Some(ThemePref::Light),
            language: Some(LangPref::Es),
            ..Config::default()
        };
        let session = Settings {
            theme: ThemePref::Dark,
            language: LangPref::En,
            ..Settings::default()
        };
        let mut c = saved.clone();
        c.apply_changes(session, session);
        assert_eq!(c, saved, "sin cambios no se toca nada");
        // El usuario cambia solo el tema en la interfaz: se guarda el tema, y el idioma impuesto NO.
        let now = Settings {
            theme: ThemePref::System,
            ..session
        };
        c.apply_changes(session, now);
        assert_eq!(c.theme, Some(ThemePref::System));
        assert_eq!(
            c.language,
            Some(LangPref::Es),
            "el --lang de la sesión no se guarda"
        );
        // Sin configuración previa y sin cambios: sigue vacía.
        let mut empty = Config::default();
        empty.apply_changes(session, session);
        assert_eq!(empty, Config::default());
    }

    #[test]
    fn una_configuracion_ilegible_o_con_valores_raros_no_rompe() {
        let dir = temp_dir();
        let path = dir.join("config.toml");
        std::fs::write(&path, "esto no es toml = = =").unwrap();
        assert_eq!(Config::load(&path), Config::default());
        // Valores desconocidos y tamaños absurdos se acotan.
        std::fs::write(
            &path,
            "theme = \"dark\"\nwindow_width = 10.0\nwindow_height = 99999.0\nextra = 1\n",
        )
        .unwrap();
        let c = Config::load(&path);
        assert_eq!(c.theme, Some(ThemePref::Dark));
        assert_eq!(c.window_size(), (MIN_SIZE.0, 8192.0));
        std::fs::write(&path, "theme = \"naranja\"\n").unwrap();
        assert_eq!(Config::load(&path), Config::default());
    }

    fn v(major: u32, minor: u32, patch: u32) -> Version {
        Version {
            major,
            minor,
            patch,
        }
    }

    #[test]
    fn la_comprobacion_de_actualizaciones_esta_desactivada_por_defecto_y_no_deja_rastro() {
        // Sin archivo, o con un archivo sin esas claves: desactivada, sin fechas ni versiones.
        let off = Config::default().settings().update;
        assert_eq!(off, UpdatePrefs::default());
        assert!(!off.check && off.last_check.is_none());
        let dir = temp_dir();
        let path = dir.join("config.toml");
        std::fs::write(&path, "theme = \"dark\"\n").unwrap();
        assert!(!Config::load(&path).settings().update.check);
        // Y guardar una configuración sin tocar el ajuste no escribe ninguna clave de la comprobación.
        let mut c = Config::load(&path);
        c.apply_changes(Settings::default(), Settings::default());
        c.save(&path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("update"), "{text}");
    }

    #[test]
    fn los_ajustes_de_la_comprobacion_van_y_vuelven_y_solo_se_guarda_lo_que_cambia() {
        let dir = temp_dir();
        let path = dir.join("Tesela").join("config.toml");
        let before = Settings::default();
        let mut after = before;
        after.update = UpdatePrefs {
            check: true,
            last_check: Some(1_800_000_000),
            latest_seen: Some(v(0, 1, 5)),
            dismissed: Some(v(0, 1, 5)),
        };
        let mut c = Config::default();
        c.apply_changes(before, after);
        assert_eq!(c.check_updates, Some(true));
        assert_eq!(c.update_last_check, Some(1_800_000_000));
        c.save(&path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("update_latest_seen = \"v0.1.5\""), "{text}");
        let back = Config::load(&path);
        assert_eq!(back, c);
        assert_eq!(back.settings().update, after.update);
        // Solo cambia la fecha del intento: lo demás se queda como estaba.
        let mut next = after;
        next.update.last_check = Some(1_800_086_400);
        let mut c2 = back.clone();
        c2.apply_changes(after, next);
        assert_eq!(c2.update_last_check, Some(1_800_086_400));
        assert_eq!(c2.check_updates, Some(true));
        // Desactivarlo se guarda como `false`.
        let mut off = next;
        off.update.check = false;
        c2.apply_changes(next, off);
        assert_eq!(c2.check_updates, Some(false));
        assert!(!c2.settings().update.check);
    }

    #[test]
    fn una_version_guardada_ilegible_se_ignora_sin_perder_el_resto() {
        let dir = temp_dir();
        let path = dir.join("config.toml");
        std::fs::write(
            &path,
            "theme = \"dark\"\ncheck_updates = true\nupdate_latest_seen = \"nada\"\nupdate_dismissed = 7\n",
        )
        .unwrap();
        let c = Config::load(&path);
        assert_eq!(c.theme, Some(ThemePref::Dark), "el resto sigue en pie");
        assert_eq!(c.check_updates, Some(true));
        assert_eq!((c.update_latest_seen, c.update_dismissed), (None, None));
    }

    #[test]
    fn una_clave_nueva_con_el_tipo_equivocado_solo_invalida_esa_clave() {
        let dir = temp_dir();
        let path = dir.join("config.toml");
        for bad in [
            "check_updates = \"si\"\n",
            "check_updates = 1\n",
            "update_last_check = -5\n",
            "update_last_check = \"ayer\"\n",
            "update_last_check = 1.5\n",
            "check_updates = [true]\nupdate_last_check = { a = 1 }\n",
        ] {
            std::fs::write(
                &path,
                format!("theme = \"dark\"\nwindow_width = 1400.0\n{bad}"),
            )
            .unwrap();
            let c = Config::load(&path);
            assert_eq!(c.theme, Some(ThemePref::Dark), "{bad}");
            assert_eq!(c.window_width, Some(1400.0), "{bad}");
            assert_eq!(c.check_updates, None, "{bad}");
            assert_eq!(c.update_last_check, None, "{bad}");
            assert!(!c.settings().update.check, "{bad}");
        }
        // Con el tipo bueno se lee tal cual.
        std::fs::write(
            &path,
            "check_updates = true\nupdate_last_check = 1800000000\n",
        )
        .unwrap();
        let c = Config::load(&path);
        assert_eq!(
            (c.check_updates, c.update_last_check),
            (Some(true), Some(1_800_000_000))
        );
    }
}
