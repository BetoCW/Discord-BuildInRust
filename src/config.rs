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
    /// Cancelación de eco (atenúa el micro mientras suena la voz de otros).
    #[serde(default = "default_true")]
    pub echo_suppression: bool,
    /// Supresión de ruido (puerta de ruido).
    #[serde(default = "default_true")]
    pub noise_suppression: bool,
    /// Control automático de ganancia (sube los micros que se oyen bajos).
    #[serde(default = "default_true")]
    pub auto_gain: bool,
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
            noise_suppression: true,
            auto_gain: true,
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
