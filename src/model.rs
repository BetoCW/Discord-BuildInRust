//! Tipos de dominio (serde) para la API REST y el Gateway de Discord.
//!
//! Deserialización **tolerante**: campos opcionales y `#[serde(default)]` para
//! resistir cambios de la API (R-3 en `spec.md`). Solo se modela lo que el
//! cliente necesita (texto y, más adelante, voz); el resto se ignora.

use serde::{Deserialize, Serialize};

/// Identificador "snowflake" de Discord. Se maneja como `String` para no perder
/// precisión y porque la API los entrega como cadenas.
pub type Snowflake = String;

// ---------------------------------------------------------------------------
// Entidades REST
// ---------------------------------------------------------------------------

/// Usuario de Discord (p. ej. el de `GET /users/@me` o el autor de un mensaje).
#[derive(Debug, Clone, Deserialize)]
pub struct User {
    pub id: Snowflake,
    #[serde(default)]
    pub username: String,
    /// Nombre visible nuevo (puede faltar en cuentas antiguas).
    #[serde(default)]
    pub global_name: Option<String>,
    #[serde(default)]
    pub discriminator: Option<String>,
    #[serde(default)]
    pub bot: bool,
}

impl User {
    /// Nombre a mostrar: prioriza `global_name`, luego `username`.
    pub fn display_name(&self) -> &str {
        self.global_name
            .as_deref()
            .filter(|s| !s.is_empty())
            .unwrap_or(&self.username)
    }
}

/// Canal (de guild o DM). `name` falta en DMs; ahí se usan `recipients`.
#[derive(Debug, Clone, Deserialize)]
pub struct Channel {
    pub id: Snowflake,
    /// Tipo de canal: 0 = texto de guild, 1 = DM, 2 = voz de guild, 3 = grupo DM…
    #[serde(default, rename = "type")]
    pub kind: u8,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub guild_id: Option<Snowflake>,
    /// Participantes (presente en DMs).
    #[serde(default)]
    pub recipients: Vec<User>,
    /// Posición en la lista del guild (para ordenar).
    #[serde(default)]
    pub position: Option<i32>,
}

impl Channel {
    pub fn is_text(&self) -> bool {
        matches!(self.kind, 0 | 5)
    }
    pub fn is_voice(&self) -> bool {
        matches!(self.kind, 2 | 13)
    }
    pub fn is_dm(&self) -> bool {
        matches!(self.kind, 1 | 3)
    }

    /// Etiqueta legible para la UI.
    pub fn label(&self) -> String {
        if let Some(name) = self.name.as_ref().filter(|s| !s.is_empty()) {
            if self.is_voice() {
                format!("🔊 {name}")
            } else {
                format!("# {name}")
            }
        } else if !self.recipients.is_empty() {
            let names: Vec<&str> = self.recipients.iter().map(|u| u.display_name()).collect();
            format!("@ {}", names.join(", "))
        } else {
            format!("canal {}", self.id)
        }
    }
}

/// Servidor (guild).
#[derive(Debug, Clone, Deserialize)]
pub struct Guild {
    pub id: Snowflake,
    #[serde(default)]
    pub name: String,
}

/// Mensaje de un canal o DM.
#[derive(Debug, Clone, Deserialize)]
pub struct Message {
    pub id: Snowflake,
    pub channel_id: Snowflake,
    #[serde(default)]
    pub author: Option<User>,
    #[serde(default)]
    pub content: String,
    /// ISO-8601; se guarda crudo y se formatea en la UI.
    #[serde(default)]
    pub timestamp: Option<String>,
    #[serde(default)]
    pub guild_id: Option<Snowflake>,
}

impl Message {
    pub fn author_name(&self) -> String {
        self.author
            .as_ref()
            .map(|u| u.display_name().to_string())
            .unwrap_or_else(|| "desconocido".to_string())
    }
}

// ---------------------------------------------------------------------------
// Protocolo Gateway
// ---------------------------------------------------------------------------

/// Opcodes del Gateway que nos interesan.
pub mod opcode {
    pub const DISPATCH: u8 = 0;
    pub const HEARTBEAT: u8 = 1;
    pub const IDENTIFY: u8 = 2;
    pub const VOICE_STATE_UPDATE: u8 = 4;
    pub const RESUME: u8 = 6;
    pub const RECONNECT: u8 = 7;
    pub const INVALID_SESSION: u8 = 9;
    pub const HELLO: u8 = 10;
    pub const HEARTBEAT_ACK: u8 = 11;
}

/// Envoltura genérica de todo payload del Gateway.
#[derive(Debug, Clone, Deserialize)]
pub struct GatewayPayload {
    pub op: u8,
    #[serde(default)]
    pub d: serde_json::Value,
    /// Número de secuencia (solo en DISPATCH).
    #[serde(default)]
    pub s: Option<u64>,
    /// Nombre del evento (solo en DISPATCH).
    #[serde(default)]
    pub t: Option<String>,
}

/// `d` del opcode HELLO.
#[derive(Debug, Clone, Deserialize)]
pub struct Hello {
    pub heartbeat_interval: u64,
}

/// `d` del evento READY (subconjunto que usamos).
#[derive(Debug, Clone, Deserialize)]
pub struct Ready {
    pub session_id: String,
    #[serde(default)]
    pub resume_gateway_url: Option<String>,
    pub user: User,
}

// ---------------------------------------------------------------------------
// Payloads que ENVIAMOS al Gateway
// ---------------------------------------------------------------------------

/// Propiedades de conexión que envía un cliente normal en el IDENTIFY.
#[derive(Debug, Clone, Serialize)]
pub struct IdentifyProperties {
    pub os: String,
    pub browser: String,
    pub device: String,
}

impl Default for IdentifyProperties {
    fn default() -> Self {
        Self {
            os: std::env::consts::OS.to_string(),
            browser: "discord-lite".to_string(),
            device: "discord-lite".to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct IdentifyData {
    pub token: String,
    pub properties: IdentifyProperties,
    /// Compresión desactivada (recibimos JSON de texto).
    pub compress: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ResumeData {
    pub token: String,
    pub session_id: String,
    pub seq: u64,
}

/// `d` para Voice State Update (op 4): unirse/salir de un canal de voz.
#[derive(Debug, Clone, Serialize)]
pub struct VoiceStateUpdateData {
    pub guild_id: Snowflake,
    /// `None` para salir del canal de voz.
    pub channel_id: Option<Snowflake>,
    pub self_mute: bool,
    pub self_deaf: bool,
}
