//! Cliente del Gateway de Discord (WebSocket en tiempo real).
//!
//! Responsable de: handshake (HELLO → IDENTIFY), heartbeat periódico con
//! vigilancia de ACK, recepción de `MESSAGE_CREATE`, señalización de voz
//! (Voice State Update op 4) y reconexión automática con RESUME/backoff.
//! (RF-3, RF-6 señalización, RF-7, CA-2, CA-7.)

use crate::model::{
    self, GatewayPayload, Hello, IdentifyData, IdentifyProperties, Message, Ready, ResumeData,
    VoiceStateUpdateData,
};
use crate::state::{AppEvent, ConnState, VoiceSignal};
use anyhow::Result;
use futures_util::{SinkExt, StreamExt};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message as WsMessage;

const GATEWAY_URL: &str = "wss://gateway.discord.gg/?v=10&encoding=json";

/// Órdenes que la UI envía específicamente al Gateway (p. ej. voz).
#[derive(Debug, Clone)]
pub enum GatewayCommand {
    VoiceState(VoiceStateUpdateData),
}

/// Datos para reanudar una sesión caída (RESUME).
#[derive(Clone)]
struct ResumeInfo {
    session_id: String,
    seq: u64,
    url: String,
}

/// Bucle principal: conecta y, ante cualquier caída, reconecta indefinidamente.
pub async fn run(
    token: String,
    events: mpsc::UnboundedSender<AppEvent>,
    mut gw_cmds: mpsc::Receiver<GatewayCommand>,
    voice_tx: mpsc::UnboundedSender<VoiceSignal>,
) {
    let mut backoff = 1u64;
    let mut resume: Option<ResumeInfo> = None;
    let mut first = true;
    // Último estado de voz deseado; se (re)envía tras READY y en reconexiones.
    let mut desired_voice: Option<VoiceStateUpdateData> = None;

    loop {
        let state = if first {
            ConnState::Connecting
        } else {
            ConnState::Reconnecting
        };
        first = false;
        let _ = events.send(AppEvent::Connection(state));

        match connect_once(
            &token,
            &events,
            &mut gw_cmds,
            &voice_tx,
            resume.clone(),
            &mut desired_voice,
        )
        .await
        {
            Ok(outcome) => {
                // Conserva info de sesión para intentar RESUME, salvo que sea inválida.
                resume = outcome.resume;
                if outcome.invalid_session {
                    resume = None;
                }
                backoff = 1; // reconexión "limpia": reinicia backoff
            }
            Err(e) => {
                tracing::warn!("gateway desconectado: {e}");
            }
        }

        let _ = events.send(AppEvent::Connection(ConnState::Reconnecting));
        let jitter = rand::random::<f64>();
        let wait = (backoff as f64 * (0.5 + jitter)).min(30.0);
        tracing::info!("reintentando conexión en {:.1}s", wait);
        tokio::time::sleep(Duration::from_secs_f64(wait)).await;
        backoff = (backoff * 2).min(30);
    }
}

/// Resultado de una sesión del Gateway.
struct Outcome {
    resume: Option<ResumeInfo>,
    invalid_session: bool,
}

async fn connect_once(
    token: &str,
    events: &mpsc::UnboundedSender<AppEvent>,
    gw_cmds: &mut mpsc::Receiver<GatewayCommand>,
    voice_tx: &mpsc::UnboundedSender<VoiceSignal>,
    resume: Option<ResumeInfo>,
    desired_voice: &mut Option<VoiceStateUpdateData>,
) -> Result<Outcome> {
    let url = resume
        .as_ref()
        .map(|r| format!("{}/?v=10&encoding=json", r.url.trim_end_matches('/')))
        .unwrap_or_else(|| GATEWAY_URL.to_string());

    tracing::info!("conectando al Gateway: {}", url);
    let (ws, _resp) = tokio_tungstenite::connect_async(&url).await?;
    let (mut sink, mut stream) = ws.split();

    // Estado de la sesión.
    let mut heartbeat = tokio::time::interval(Duration::from_secs(45));
    let mut last_seq: Option<u64> = resume.as_ref().map(|r| r.seq);
    let mut session_id: Option<String> = resume.as_ref().map(|r| r.session_id.clone());
    let mut resume_url: Option<String> = resume.as_ref().map(|r| r.url.clone());
    let mut got_ack = true;
    let mut identified = false;
    let mut ready = false; // READY recibido: ya se puede enviar Voice State Update
    let resuming = resume.is_some();

    loop {
        tokio::select! {
            // --- Heartbeat periódico -------------------------------------
            _ = heartbeat.tick() => {
                if identified && !got_ack {
                    tracing::warn!("sin ACK de heartbeat: conexión zombie, reconectando");
                    break;
                }
                if identified {
                    let hb = serde_json::json!({ "op": model::opcode::HEARTBEAT, "d": last_seq });
                    sink.send(WsMessage::Text(hb.to_string())).await?;
                    got_ack = false;
                }
            }

            // --- Comandos de la UI hacia el Gateway (voz) ----------------
            cmd = gw_cmds.recv() => {
                match cmd {
                    Some(GatewayCommand::VoiceState(vs)) => {
                        // Guarda el estado deseado y envíalo solo si ya hay READY
                        // (enviarlo antes provoca cierre 4003 "Not authenticated").
                        *desired_voice = Some(vs.clone());
                        if ready {
                            let payload = serde_json::json!({
                                "op": model::opcode::VOICE_STATE_UPDATE,
                                "d": vs,
                            });
                            sink.send(WsMessage::Text(payload.to_string())).await?;
                        }
                    }
                    None => { /* canal cerrado: ignorar */ }
                }
            }

            // --- Frames entrantes ----------------------------------------
            frame = stream.next() => {
                let frame = match frame {
                    Some(f) => f?,
                    None => { tracing::info!("stream cerrado por el servidor"); break; }
                };
                let text = match frame {
                    WsMessage::Text(t) => t,
                    WsMessage::Close(c) => {
                        tracing::info!("close frame: {:?}", c);
                        break;
                    }
                    WsMessage::Ping(p) => { sink.send(WsMessage::Pong(p)).await?; continue; }
                    _ => continue,
                };

                let payload: GatewayPayload = match serde_json::from_str(&text) {
                    Ok(p) => p,
                    Err(e) => { tracing::debug!("payload no parseable: {e}"); continue; }
                };
                if let Some(s) = payload.s { last_seq = Some(s); }

                match payload.op {
                    model::opcode::HELLO => {
                        let hello: Hello = serde_json::from_value(payload.d)?;
                        let interval = Duration::from_millis(hello.heartbeat_interval);
                        heartbeat = tokio::time::interval(interval);
                        // Primer tick inmediato; el siguiente respeta el intervalo.
                        heartbeat.tick().await;

                        if resuming && session_id.is_some() {
                            send_resume(&mut sink, token, session_id.as_ref().unwrap(),
                                        last_seq.unwrap_or(0)).await?;
                        } else {
                            send_identify(&mut sink, token).await?;
                        }
                        identified = true;
                    }
                    model::opcode::HEARTBEAT => {
                        // El servidor pide un heartbeat inmediato.
                        let hb = serde_json::json!({ "op": model::opcode::HEARTBEAT, "d": last_seq });
                        sink.send(WsMessage::Text(hb.to_string())).await?;
                        got_ack = false;
                    }
                    model::opcode::HEARTBEAT_ACK => { got_ack = true; }
                    model::opcode::RECONNECT => {
                        tracing::info!("servidor pidió RECONNECT");
                        return Ok(Outcome {
                            resume: build_resume(session_id, resume_url, last_seq),
                            invalid_session: false,
                        });
                    }
                    model::opcode::INVALID_SESSION => {
                        tracing::info!("sesión inválida: re-IDENTIFY limpio");
                        return Ok(Outcome { resume: None, invalid_session: true });
                    }
                    model::opcode::DISPATCH => {
                        let is_ready = payload.t.as_deref() == Some("READY")
                            || payload.t.as_deref() == Some("RESUMED");
                        handle_dispatch(&payload, events, voice_tx, &mut session_id, &mut resume_url);
                        if is_ready && !ready {
                            ready = true;
                            // Reenvía el estado de voz deseado ahora que hay sesión.
                            if let Some(vs) = desired_voice.clone() {
                                let payload = serde_json::json!({
                                    "op": model::opcode::VOICE_STATE_UPDATE,
                                    "d": vs,
                                });
                                sink.send(WsMessage::Text(payload.to_string())).await?;
                                tracing::info!("Voice State Update (re)enviado tras READY");
                            }
                        }
                    }
                    other => tracing::trace!("opcode no manejado: {other}"),
                }
            }
        }
    }

    Ok(Outcome {
        resume: build_resume(session_id, resume_url, last_seq),
        invalid_session: false,
    })
}

fn build_resume(
    session_id: Option<String>,
    url: Option<String>,
    seq: Option<u64>,
) -> Option<ResumeInfo> {
    match (session_id, url, seq) {
        (Some(session_id), Some(url), Some(seq)) => Some(ResumeInfo {
            session_id,
            url,
            seq,
        }),
        _ => None,
    }
}

fn handle_dispatch(
    payload: &GatewayPayload,
    events: &mpsc::UnboundedSender<AppEvent>,
    voice_tx: &mpsc::UnboundedSender<VoiceSignal>,
    session_id: &mut Option<String>,
    resume_url: &mut Option<String>,
) {
    let Some(event_name) = payload.t.as_deref() else {
        return;
    };
    match event_name {
        "READY" => match serde_json::from_value::<Ready>(payload.d.clone()) {
            Ok(ready) => {
                *session_id = Some(ready.session_id.clone());
                *resume_url = ready.resume_gateway_url.clone();
                let _ = events.send(AppEvent::Connection(ConnState::Connected));
                let _ = events.send(AppEvent::Ready(ready.user));
                tracing::info!("READY recibido; sesión iniciada");
            }
            Err(e) => tracing::warn!("READY no parseable: {e}"),
        },
        "RESUMED" => {
            let _ = events.send(AppEvent::Connection(ConnState::Connected));
            tracing::info!("RESUMED: sesión reanudada");
        }
        "MESSAGE_CREATE" => match serde_json::from_value::<Message>(payload.d.clone()) {
            Ok(msg) => {
                let _ = events.send(AppEvent::NewMessage(msg));
            }
            Err(e) => tracing::debug!("MESSAGE_CREATE no parseable: {e}"),
        },
        "VOICE_STATE_UPDATE" => {
            let d = &payload.d;
            if let Some(session) = d.get("session_id").and_then(|x| x.as_str()) {
                let _ = voice_tx.send(VoiceSignal::State {
                    user_id: d
                        .get("user_id")
                        .and_then(|x| x.as_str())
                        .unwrap_or_default()
                        .to_string(),
                    guild_id: d
                        .get("guild_id")
                        .and_then(|x| x.as_str())
                        .map(String::from),
                    channel_id: d
                        .get("channel_id")
                        .and_then(|x| x.as_str())
                        .map(String::from),
                    session_id: session.to_string(),
                });
            }
        }
        "VOICE_SERVER_UPDATE" => {
            let d = &payload.d;
            if let Some(token) = d.get("token").and_then(|x| x.as_str()) {
                let _ = voice_tx.send(VoiceSignal::Server {
                    guild_id: d
                        .get("guild_id")
                        .and_then(|x| x.as_str())
                        .unwrap_or_default()
                        .to_string(),
                    endpoint: d.get("endpoint").and_then(|x| x.as_str()).map(String::from),
                    token: token.to_string(),
                });
            }
        }
        _ => {}
    }
}

async fn send_identify<S>(sink: &mut S, token: &str) -> Result<()>
where
    S: SinkExt<WsMessage> + Unpin,
    <S as futures_util::Sink<WsMessage>>::Error: std::error::Error + Send + Sync + 'static,
{
    let data = IdentifyData {
        token: token.to_string(),
        properties: IdentifyProperties::default(),
        compress: false,
    };
    let payload = serde_json::json!({ "op": model::opcode::IDENTIFY, "d": data });
    sink.send(WsMessage::Text(payload.to_string())).await?;
    tracing::info!("IDENTIFY enviado");
    Ok(())
}

async fn send_resume<S>(sink: &mut S, token: &str, session_id: &str, seq: u64) -> Result<()>
where
    S: SinkExt<WsMessage> + Unpin,
    <S as futures_util::Sink<WsMessage>>::Error: std::error::Error + Send + Sync + 'static,
{
    let data = ResumeData {
        token: token.to_string(),
        session_id: session_id.to_string(),
        seq,
    };
    let payload = serde_json::json!({ "op": model::opcode::RESUME, "d": data });
    sink.send(WsMessage::Text(payload.to_string())).await?;
    tracing::info!("RESUME enviado (seq {seq})");
    Ok(())
}
