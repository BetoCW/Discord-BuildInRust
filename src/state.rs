//! Estado de la aplicación y los mensajes que cruzan entre el mundo async
//! (tokio) y el mundo UI (hilo principal). Comunicación SOLO por canales.

use crate::model::{Channel, Message, User};
use std::collections::{HashMap, VecDeque};

/// Órdenes de la UI hacia la red (`ui → net`).
#[derive(Debug, Clone)]
pub enum Command {
    /// Cargar historial reciente de un canal/DM.
    LoadHistory { channel_id: String },
    /// Resolver metadatos (nombre, tipo) de un canal por su ID.
    ResolveChannel { channel_id: String },
    /// Cargar la lista de mensajes directos (DMs) del usuario.
    LoadDms,
    /// Enviar un mensaje de texto.
    SendMessage { channel_id: String, content: String },
    /// (Voz, fase 2) Unirse a un canal de voz.
    JoinVoice { guild_id: String, channel_id: String },
    /// (Voz, fase 2) Salir del canal de voz.
    LeaveVoice { guild_id: String },
    /// (Voz) Silenciar/activar micrófono.
    VoiceMute(bool),
    /// (Voz) Silenciar/activar salida.
    VoiceDeaf(bool),
    /// Cerrar sesión (borrar token) y terminar.
    Logout,
}

/// Señales de voz que el Gateway principal reenvía al orquestador `net`.
#[derive(Debug, Clone)]
pub enum VoiceSignal {
    /// `VOICE_STATE_UPDATE`: aporta el `session_id` de voz (de nuestro usuario).
    State {
        user_id: String,
        guild_id: Option<String>,
        channel_id: Option<String>,
        session_id: String,
    },
    /// `VOICE_SERVER_UPDATE`: aporta el endpoint y token del servidor de voz.
    Server {
        guild_id: String,
        endpoint: Option<String>,
        token: String,
    },
}

/// Control de una sesión de voz activa (`net`/UI → tarea de voz).
#[derive(Debug, Clone)]
pub enum VoiceControl {
    Mute(bool),
    Deaf(bool),
    Stop,
}

/// Eventos de la red hacia la UI (`net → ui`).
#[derive(Debug, Clone)]
pub enum AppEvent {
    /// Sesión iniciada: usuario actual.
    Ready(User),
    /// Cambió el estado de la conexión del Gateway.
    Connection(ConnState),
    /// Llegó un mensaje nuevo (Gateway `MESSAGE_CREATE`).
    NewMessage(Message),
    /// Resultado de cargar historial.
    History {
        channel_id: String,
        messages: Vec<Message>,
    },
    /// Confirmación de envío (mensaje ya creado en Discord).
    Sent(Message),
    /// Lista de canales disponibles para seguir (para la UI de ajustes).
    Channels(Vec<Channel>),
    /// Lista de mensajes directos (DMs) abiertos del usuario.
    Dms(Vec<Channel>),
    /// Metadatos de un canal concreto (resuelto por `ResolveChannel`).
    ChannelInfo(Channel),
    /// Mensaje de error legible para mostrar al usuario.
    Error(String),
    /// Estado del subsistema de voz (texto legible para la UI).
    VoiceUpdate {
        connected: bool,
        text: String,
    },
}

/// Estado de la conexión en tiempo real.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnState {
    Connecting,
    Connected,
    Reconnecting,
    Offline,
}

impl ConnState {
    pub fn label(&self) -> &'static str {
        match self {
            ConnState::Connecting => "conectando…",
            ConnState::Connected => "conectado",
            ConnState::Reconnecting => "reconectando…",
            ConnState::Offline => "sin conexión",
        }
    }
}

/// Estado central que la UI lee y repinta. Vive en el hilo de la UI; se muta
/// drenando los `AppEvent`. Los buffers por canal están acotados (RNF-2).
pub struct AppState {
    pub me: Option<User>,
    pub conn: ConnState,
    pub active_channel: Option<String>,
    pub messages: HashMap<String, VecDeque<Message>>,
    pub channels: Vec<Channel>,
    /// Mensajes directos (DMs) del usuario, cargados aparte de los canales.
    pub dms: Vec<Channel>,
    /// Nombre legible por ID de canal (resuelto vía REST).
    pub channel_names: HashMap<String, String>,
    pub history_limit: usize,
    pub last_error: Option<String>,
    pub voice_status: Option<String>,
    pub voice_connected: bool,
}

impl AppState {
    pub fn new(history_limit: usize) -> Self {
        Self {
            me: None,
            conn: ConnState::Connecting,
            active_channel: None,
            messages: HashMap::new(),
            channels: Vec::new(),
            dms: Vec::new(),
            channel_names: HashMap::new(),
            history_limit: history_limit.max(10),
            last_error: None,
            voice_status: None,
            voice_connected: false,
        }
    }

    /// Aplica un evento al estado.
    pub fn apply(&mut self, event: AppEvent) {
        match event {
            AppEvent::Ready(user) => self.me = Some(user),
            AppEvent::Connection(c) => self.conn = c,
            AppEvent::NewMessage(msg) => self.push_message(msg),
            AppEvent::History {
                channel_id,
                messages,
            } => {
                let buf = self.messages.entry(channel_id).or_default();
                buf.clear();
                for m in messages {
                    buf.push_back(m);
                }
                self.trim_all();
            }
            AppEvent::Sent(msg) => self.push_message(msg),
            AppEvent::Channels(chs) => self.channels = chs,
            AppEvent::Dms(chs) => {
                // Guarda los nombres legibles para el título/lista y la lista de DMs.
                for ch in &chs {
                    self.channel_names.insert(ch.id.clone(), ch.label());
                }
                self.dms = chs;
            }
            AppEvent::ChannelInfo(ch) => {
                self.channel_names.insert(ch.id.clone(), ch.label());
                self.channels.push(ch);
            }
            AppEvent::Error(e) => {
                tracing::error!("{e}");
                self.last_error = Some(e);
            }
            AppEvent::VoiceUpdate { connected, text } => {
                self.voice_connected = connected;
                self.voice_status = Some(text);
            }
        }
    }

    fn push_message(&mut self, msg: Message) {
        let limit = self.history_limit;
        let buf = self.messages.entry(msg.channel_id.clone()).or_default();
        // Evita duplicados por eco (mismo id ya presente).
        if buf.iter().any(|m| m.id == msg.id) {
            return;
        }
        buf.push_back(msg);
        while buf.len() > limit {
            buf.pop_front();
        }
    }

    fn trim_all(&mut self) {
        let limit = self.history_limit;
        for buf in self.messages.values_mut() {
            while buf.len() > limit {
                buf.pop_front();
            }
        }
    }

    pub fn channel_messages(&self, channel_id: &str) -> impl Iterator<Item = &Message> {
        self.messages
            .get(channel_id)
            .into_iter()
            .flat_map(|b| b.iter())
    }
}
