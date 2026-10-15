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

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir() -> PathBuf {
        let d = std::env::temp_dir().join(format!("tesela-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
        d
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
        std::fs::remove_dir_all(&dir).unwrap();
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
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
