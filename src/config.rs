//! Preferencias persistentes (NO secretas). El token vive en `auth`, nunca aquí.
//!
//! Se guarda como JSON en el directorio de configuración del usuario
//! (`%APPDATA%` en Windows, `~/.config` en Linux) vía `directories`.

use anyhow::{Context, Result};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Último canal de voz al que el usuario se unió (guild + canal), para poder
/// reunirse con un solo clic sin volver a teclear los IDs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VoiceTarget {
    pub guild_id: String,
    pub channel_id: String,
}

/// Modo de supresión de ruido del micrófono, estilo Discord:
/// - `Off`: sin procesar (la voz pasa tal cual).
/// - `Light`: puerta de ruido por umbral (barata; silencia el fondo en silencios).
/// - `VoiceIsolation`: red neuronal RNNoise (aísla la voz del ruido continuo;
///   el equivalente abierto a «Krisp»). Más CPU, mejor resultado.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoiseMode {
    Off,
    Light,
    VoiceIsolation,
}

impl NoiseMode {
    pub fn from_u8(v: u8) -> Self {
        match v {
            0 => NoiseMode::Off,
            1 => NoiseMode::Light,
            _ => NoiseMode::VoiceIsolation,
        }
    }
    pub fn as_u8(self) -> u8 {
        match self {
            NoiseMode::Off => 0,
            NoiseMode::Light => 1,
            NoiseMode::VoiceIsolation => 2,
        }
    }
}

/// Ajustes de voz (panel «Ajustes de voz», estilo Discord). Se aplican en vivo
/// vía `audio::options()` y se persisten aquí.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VoiceSettings {
    /// Dispositivo de entrada elegido (`None` = predeterminado del sistema).
    #[serde(default)]
    pub input_device: Option<String>,
    /// Dispositivo de salida elegido.
    #[serde(default)]
    pub output_device: Option<String>,
    /// Volumen de entrada en % (0–200; 100 = sin cambio).
    #[serde(default = "default_volume")]
    pub input_volume: u32,
    /// Volumen de salida en % (0–200).
    #[serde(default = "default_volume")]
    pub output_volume: u32,
    /// Supresión de eco (atenúa fuerte el micro mientras suena la voz de otros,
    /// para no reenviarla a la sala). ACTIVADA por defecto: es lo que evita el
    /// eco recursivo cuando alguien NO usa auriculares. A quien sí los usa no le
    /// afecta (sin acople acústico, el ducker no atenúa la voz).
    #[serde(default = "default_true")]
    pub echo_suppression: bool,
    /// Modo de supresión de ruido: Off / Ligero (puerta) / Aislamiento de voz
    /// (RNNoise). Por defecto, aislamiento de voz (lo que hacía `noise_suppression`
    /// = true antes). Configs viejas sin este campo migran a este predeterminado.
    #[serde(default = "default_noise_mode")]
    pub noise_mode: NoiseMode,
    /// Control automático de ganancia (sube los micros que se oyen bajos).
    /// Opt-in: cambia el volumen de forma dinámica y conviene probarlo antes.
    #[serde(default = "default_false")]
    pub auto_gain: bool,
    /// Cancelación de eco AVANZADA (AEC adaptativo NLMS, Meta 5). EXPERIMENTAL y
    /// opt-in: resta el eco en vez de atenuar (permite doble-habla), pero requiere
    /// ajuste de retardo en vivo. Cuando está activa, sustituye al ducker básico.
    #[serde(default = "default_false")]
    pub aec: bool,
    /// Sensibilidad de entrada automática.
    #[serde(default = "default_true")]
    pub auto_sensitivity: bool,
    /// Umbral manual de sensibilidad en dBFS (-100..0), si no es automática.
    #[serde(default = "default_sensitivity")]
    pub sensitivity_db: i32,
}

impl Default for VoiceSettings {
    fn default() -> Self {
        Self {
            input_device: None,
            output_device: None,
            input_volume: default_volume(),
            output_volume: default_volume(),
            echo_suppression: true,
            noise_mode: default_noise_mode(),
            auto_gain: false,
            aec: false,
            auto_sensitivity: true,
            sensitivity_db: default_sensitivity(),
        }
    }
}

/// Preferencias del usuario que sobreviven a reinicios (RF-2, RF-8).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Config {
    /// IDs de los canales que el usuario decidió seguir (texto/voz).
    #[serde(default)]
    pub followed_channels: Vec<String>,
    /// Último canal abierto, para reabrirlo al arrancar.
    #[serde(default)]
    pub last_channel: Option<String>,
    /// Último canal de voz usado, para el botón de "reunirse" con un clic.
    #[serde(default)]
    pub last_voice: Option<VoiceTarget>,
    /// Cuántos mensajes recientes conservar por canal en memoria (RNF-2).
    #[serde(default = "default_history_limit")]
    pub history_limit: usize,
    /// Ajustes de voz (dispositivos, volúmenes, procesamiento).
    #[serde(default)]
    pub voice: VoiceSettings,
}

fn default_history_limit() -> usize {
    100
}

fn default_volume() -> u32 {
    100
}

fn default_true() -> bool {
    true
}

fn default_noise_mode() -> NoiseMode {
    NoiseMode::VoiceIsolation
}

fn default_false() -> bool {
    false
}

fn default_sensitivity() -> i32 {
    -60
}

impl Config {
    /// Carga la configuración; si no existe, devuelve la de por defecto.
    pub fn load() -> Result<Self> {
        let path = Self::path()?;
        if !path.exists() {
            return Ok(Self {
                history_limit: default_history_limit(),
                ..Default::default()
            });
        }
        let data = std::fs::read_to_string(&path)
            .with_context(|| format!("leyendo config {}", path.display()))?;
        let cfg: Config = serde_json::from_str(&data).context("parseando config JSON")?;
        Ok(cfg)
    }

    /// Guarda la configuración (crea el directorio si hace falta).
    pub fn save(&self) -> Result<()> {
        let path = Self::path()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creando dir de config {}", parent.display()))?;
        }
        let data = serde_json::to_string_pretty(self).context("serializando config")?;
        std::fs::write(&path, data).with_context(|| format!("escribiendo {}", path.display()))?;
        Ok(())
    }

    pub fn follow(&mut self, channel_id: impl Into<String>) {
        let id = channel_id.into();
        if !self.followed_channels.contains(&id) {
            self.followed_channels.push(id);
        }
    }

    pub fn unfollow(&mut self, channel_id: &str) {
        self.followed_channels.retain(|c| c != channel_id);
        if self.last_channel.as_deref() == Some(channel_id) {
            self.last_channel = None;
        }
    }

    /// Ruta del archivo de configuración.
    pub fn path() -> Result<PathBuf> {
        let dirs = project_dirs()?;
        Ok(dirs.config_dir().join("config.json"))
    }
}

/// Directorios estándar de la app por SO.
pub fn project_dirs() -> Result<ProjectDirs> {
    ProjectDirs::from("dev", "discord-lite", "discord-lite")
        .context("no se pudo determinar el directorio de configuración del usuario")
}
