//! Orquestador de red (mundo async/tokio). Posee el cliente REST, lanza el
//! Gateway y traduce los `Command` de la UI en llamadas REST o en sesiones de
//! voz. Se comunica con la UI solo por canales (mpsc).

use crate::gateway::{self, GatewayCommand};
use crate::model::VoiceStateUpdateData;
use crate::rest::RestClient;
use crate::state::{AppEvent, Command, VoiceControl, VoiceSignal};
use crate::voice::{self, VoiceConnInfo};
use tokio::sync::mpsc;

/// Punto de entrada del subsistema de red. Bloquea hasta que se cierra el canal
/// de comandos (cierre de la app) o se solicita logout.
pub async fn run(
    token: String,
    events: mpsc::UnboundedSender<AppEvent>,
    mut commands: mpsc::UnboundedReceiver<Command>,
) {
    let rest = match RestClient::new(token.clone()) {
        Ok(c) => c,
        Err(e) => {
            let _ = events.send(AppEvent::Error(format!("HTTP: {e}")));
            return;
        }
    };

    // Validación temprana del token (falla rápido y claro) y user_id propio.
    let user_id = match rest.validate_token().await {
        Ok(user) => {
            tracing::info!("token válido para {}", user.display_name());
            let id = user.id.clone();
            let _ = events.send(AppEvent::Ready(user));
            id
        }
        Err(e) => {
            let _ = events.send(AppEvent::Error(crate::rest::friendly_auth_error(&e)));
            return;
        }
    };

    // Gateway + canal de señales de voz.
    let (gw_tx, gw_rx) = mpsc::channel::<GatewayCommand>(32);
    let (voice_tx, mut voice_rx) = mpsc::unbounded_channel::<VoiceSignal>();
    tokio::spawn(gateway::run(token, events.clone(), gw_rx, voice_tx));

    // Estado de voz pendiente/activa.
    let mut pending: Option<(String, String)> = None; // (guild, channel)
    let mut sig_session: Option<String> = None;
    let mut sig_server: Option<(String, String, String)> = None; // (guild, endpoint, token)
    let mut voice_ctrl: Option<mpsc::UnboundedSender<VoiceControl>> = None;

    loop {
        tokio::select! {
            cmd = commands.recv() => {
                let Some(cmd) = cmd else { break };
                match cmd {
                    Command::LoadHistory { channel_id } => {
                        let rest = rest.clone();
                        let events = events.clone();
                        tokio::spawn(async move {
                            match rest.get_channel_messages(&channel_id, 50).await {
                                Ok(messages) => {
                                    let _ = events.send(AppEvent::History { channel_id, messages });
                                }
                                Err(e) => {
                                    let _ = events.send(AppEvent::Error(format!("historial: {e}")));
                                }
                            }
                        });
                    }
                    Command::ResolveChannel { channel_id } => {
                        let rest = rest.clone();
                        let events = events.clone();
                        tokio::spawn(async move {
                            if let Ok(ch) = rest.get_channel(&channel_id).await {
                                let _ = events.send(AppEvent::ChannelInfo(ch));
                            }
                        });
                    }
                    Command::LoadDms => {
                        let rest = rest.clone();
                        let events = events.clone();
                        tokio::spawn(async move {
                            match rest.list_dms().await {
                                Ok(dms) => { let _ = events.send(AppEvent::Dms(dms)); }
                                Err(e) => { let _ = events.send(AppEvent::Error(format!("DMs: {e}"))); }
                            }
                        });
                    }
                    Command::SendMessage { channel_id, content } => {
                        let rest = rest.clone();
                        let events = events.clone();
                        tokio::spawn(async move {
                            match rest.create_message(&channel_id, &content).await {
                                Ok(msg) => { let _ = events.send(AppEvent::Sent(msg)); }
                                Err(e) => { let _ = events.send(AppEvent::Error(format!("envío: {e}"))); }
                            }
                        });
                    }
                    Command::JoinVoice { guild_id, channel_id } => {
                        pending = Some((guild_id.clone(), channel_id.clone()));
                        sig_session = None;
                        sig_server = None;
                        let _ = gw_tx.send(GatewayCommand::VoiceState(VoiceStateUpdateData {
                            guild_id,
                            channel_id: Some(channel_id),
                            self_mute: false,
                            self_deaf: false,
                        })).await;
                    }
                    Command::LeaveVoice { guild_id } => {
                        if let Some(ctrl) = voice_ctrl.take() {
                            let _ = ctrl.send(VoiceControl::Stop);
                        }
                        pending = None;
                        let _ = gw_tx.send(GatewayCommand::VoiceState(VoiceStateUpdateData {
                            guild_id,
                            channel_id: None,
                            self_mute: false,
                            self_deaf: false,
                        })).await;
                    }
                    Command::VoiceMute(m) => {
                        if let Some(ctrl) = &voice_ctrl {
                            let _ = ctrl.send(VoiceControl::Mute(m));
                        }
                    }
                    Command::VoiceDeaf(d) => {
                        if let Some(ctrl) = &voice_ctrl {
                            let _ = ctrl.send(VoiceControl::Deaf(d));
                        }
                    }
                    Command::Logout => {
                        if let Err(e) = crate::auth::delete_token() {
                            tracing::warn!("logout: {e}");
                        }
                        tracing::info!("logout solicitado; terminando red");
                        break;
                    }
                }
            }

            sig = voice_rx.recv() => {
                let Some(sig) = sig else { continue };
                match sig {
                    VoiceSignal::State { user_id: uid, session_id, .. } => {
                        if uid == user_id {
                            sig_session = Some(session_id);
                        }
                    }
                    VoiceSignal::Server { guild_id, endpoint, token } => {
                        sig_server = Some((guild_id, endpoint.unwrap_or_default(), token));
                    }
                }
                // ¿Tenemos todo para arrancar la voz?
                if let (Some((pg, pc)), Some(session), Some((sg, endpoint, vtoken))) =
                    (pending.clone(), sig_session.clone(), sig_server.clone())
                {
                    if pg == sg && !endpoint.is_empty() {
                        let info = VoiceConnInfo {
                            guild_id: pg,
                            channel_id: pc,
                            user_id: user_id.clone(),
                            session_id: session,
                            endpoint,
                            token: vtoken,
                        };
                        let (ctrl_tx, ctrl_rx) = mpsc::unbounded_channel::<VoiceControl>();
                        voice_ctrl = Some(ctrl_tx);
                        tokio::spawn(voice::run(info, ctrl_rx, events.clone()));
                        pending = None;
                        sig_session = None;
                        sig_server = None;
                    }
                }
            }
        }
    }

    if let Some(ctrl) = voice_ctrl.take() {
        let _ = ctrl.send(VoiceControl::Stop);
    }
}
