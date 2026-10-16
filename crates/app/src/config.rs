//! Persistencia mínima (PLAN.md §2.11): `%APPDATA%\Tesela\config.toml` con el tema, el idioma y el tamaño de la
//! ventana. Nunca se guarda nada del árbol ni de los escaneos. Una configuración ilegible se ignora (se usan los
//! valores por defecto) en vez de impedir el arranque.

use std::path::{Path, PathBuf};
use tesela_ui::{LangPref, Settings, ThemePref};

/// Tamaño de ventana por defecto (en puntos lógicos) y mínimo.
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
}

impl Config {
    /// Ajustes de la interfaz que contiene.
    pub fn settings(&self) -> Settings {
        Settings {
            theme: self.theme.unwrap_or(ThemePref::System),
            language: self.language.unwrap_or(LangPref::System),
        }
    }

    /// Registra SOLO los ajustes que cambiaron entre `before` y `after`: lo que el usuario no tocó (por ejemplo un
    /// idioma impuesto con `--lang` para una sesión) no se escribe en el archivo.
    pub fn apply_changes(&mut self, before: Settings, after: Settings) {
        if after.theme != before.theme {
            self.theme = Some(after.theme);
        }
        if after.language != before.language {
            self.language = Some(after.language);
        }
    }

    /// Tamaño de ventana guardado, acotado a valores razonables.
    pub fn window_size(&self) -> (f32, f32) {
        match (self.window_width, self.window_height) {
            (Some(w), Some(h)) if w.is_finite() && h.is_finite() => {
                (w.clamp(MIN_SIZE.0, 8192.0), h.clamp(MIN_SIZE.1, 8192.0))
            }
            _ => DEFAULT_SIZE,
        }
    }

    /// Lee la configuración de `path`; si no existe o no se entiende, la de por defecto.
    pub fn load(path: &Path) -> Config {
        match std::fs::read_to_string(path) {
            Ok(text) => toml::from_str(&text).unwrap_or_else(|e| {
                eprintln!("tesela: config.toml ilegible, se usan los valores por defecto: {e}");
                Config::default()
            }),
            Err(_) => Config::default(),
        }
    }

    /// Guarda la configuración (crea la carpeta; escribe en un temporal y lo renombra para no dejar un archivo a medias).
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

/// Ruta de `config.toml`: `%APPDATA%\Tesela\config.toml`.
pub fn config_path() -> Option<PathBuf> {
    let appdata = std::env::var_os("APPDATA").filter(|v| !v.is_empty())?;
    Some(PathBuf::from(appdata).join("Tesela").join("config.toml"))
}

/// Configuración de ESTA sesión y dónde se guarda.
pub struct Session {
    pub config: Config,
    /// `None` = no se guarda en ningún sitio.
    pub path: Option<PathBuf>,
}

impl Session {
    /// Sesión AISLADA (la demostración y las herramientas de medición): valores por defecto en memoria; no lee ni escribe nada
    /// bajo `%APPDATA%`. La sesión normal lee `config.toml` y lo guarda al cambiar un ajuste y al salir.
    pub fn new(isolated: bool) -> Session {
        Session::with_path(isolated, config_path())
    }

    /// Igual que [`Session::new`] con la ruta de `config.toml` inyectada (las pruebas no tocan el entorno del proceso).
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

    /// ¿Se guarda algo? Solo la sesión normal.
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
}
