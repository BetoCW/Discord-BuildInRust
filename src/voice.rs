//! Subsistema de voz (Fase 2). Aislado y desactivable (R-5).
//!
//! Flujo: con los datos de `VOICE_SERVER_UPDATE`/`VOICE_STATE_UPDATE` abre el
//! **Voice Gateway** (WebSocket aparte), hace su handshake (IDENTIFY → READY →
//! IP discovery por **UDP** → SELECT PROTOCOL → SESSION DESCRIPTION), y luego
//! arranca el audio: captura de micrófono (cpal) → **Opus** → cifrado
//! **XChaCha20-Poly1305 (rtpsize)** → UDP, y la ruta inversa para reproducir.
//!
//! ⚠️ Sin verificar contra un canal de voz real todavía; la lógica de protocolo
//! sigue la documentación de Discord (Voice Gateway v4 + modo aead rtpsize).

use crate::dave;
use crate::state::{AppEvent, VoiceControl};
use anyhow::{anyhow, bail, Context, Result};
use audiopus::{
    coder::{Decoder, Encoder},
    Application, Channels, SampleRate,
};
use chacha20poly1305::{
    aead::{Aead, KeyInit, Payload},
    Key, XChaCha20Poly1305, XNonce,
};
use futures_util::{SinkExt, StreamExt};
use std::collections::{HashMap, VecDeque};
use std::net::UdpSocket;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message as WsMessage;

const SAMPLE_RATE: u32 = 48_000;
const CHANNELS: usize = 2;
/// Muestras por canal en un frame de 20 ms a 48 kHz.
const FRAME_SAMPLES: usize = 960;
/// Muestras interleaved (estéreo) por frame.
const FRAME_LEN: usize = FRAME_SAMPLES * CHANNELS;
const PREFERRED_MODE: &str = "aead_xchacha20_poly1305_rtpsize";

/// Datos necesarios para abrir una sesión de voz.
#[derive(Debug, Clone)]
pub struct VoiceConnInfo {
    pub guild_id: String,
    pub channel_id: String,
    pub user_id: String,
    pub session_id: String,
    pub endpoint: String,
    pub token: String,
}

/// Punto de entrada: gestiona una sesión de voz hasta que se pide `Stop`.
pub async fn run(
    info: VoiceConnInfo,
    control: mpsc::UnboundedReceiver<VoiceControl>,
    events: mpsc::UnboundedSender<AppEvent>,
) {
    let _ = events.send(AppEvent::VoiceUpdate {
        connected: false,
        text: "voz: conectando…".into(),
    });
    if let Err(e) = session(&info, control, &events).await {
        tracing::warn!("sesión de voz terminada: {e}");
        let _ = events.send(AppEvent::VoiceUpdate {
            connected: false,
            text: format!("voz: desconectado ({e})"),
        });
    } else {
        let _ = events.send(AppEvent::VoiceUpdate {
            connected: false,
            text: "voz: desconectado".into(),
        });
    }
}

/// Estado de audio compartido con los hilos de tiempo real.
struct Shared {
    stop: Arc<AtomicBool>,
    mute: Arc<AtomicBool>,
    deaf: Arc<AtomicBool>,
    /// Cifrador E2EE (DAVE) para envolver el Opus antes del transporte. `None`
    /// hasta que el handshake MLS termina (op 30 Welcome); a partir de ahí el TX
    /// lo aplica a cada frame. Compartido porque el audio arranca antes (op 4).
    e2ee: Arc<Mutex<Option<dave::FrameCryptor>>>,
    /// Cifradores RX por emisor (SSRC → cryptor). Se pueblan al mapear op5
    /// SPEAKING (user_id↔ssrc) una vez dentro del grupo MLS; cada emisor deriva
    /// su clave con SU user_id como context del exporter. El RX desenvuelve el
    /// frame DAVE antes de decodificar Opus.
    rx_e2ee: Arc<Mutex<HashMap<u32, dave::FrameCryptor>>>,
    /// `true` cuando DAVE/E2EE está activo: el RX descarta frames de emisores aún
    /// sin clave en vez de intentar decodificarlos como Opus en claro (basura).
    e2ee_active: Arc<AtomicBool>,
}

/// Re-deriva las claves de medios del epoch ACTUAL del grupo MLS (la propia para
/// TX + la de cada emisor conocido para RX) y las instala en `shared`. Se llama
/// tras el Welcome inicial y tras cada commit (op29/op22) que avanza el epoch;
/// las claves del epoch anterior se descartan.
fn install_media_keys(m: &dave::MlsSession, shared: &Shared, ssrc_uid: &HashMap<u32, u64>) {
    match m.media_base_secret() {
        Ok(secret) => {
            *shared.e2ee.lock().unwrap() = Some(dave::FrameCryptor::from_base_secret(&secret));
            shared.e2ee_active.store(true, Ordering::Relaxed);
            tracing::info!("DAVE: clave de medios TX derivada (epoch {}) — E2EE listo ✅", m.epoch());
        }
        Err(e) => tracing::warn!("DAVE export secret (TX): {e}"),
    }
    let mut rxmap = shared.rx_e2ee.lock().unwrap();
    rxmap.clear(); // las claves RX del epoch anterior ya no sirven
    for (rssrc, uid) in ssrc_uid.iter() {
        match m.media_base_secret_for(*uid) {
            Ok(s) => {
                rxmap.insert(*rssrc, dave::FrameCryptor::from_base_secret(&s));
                tracing::info!("DAVE RX: clave para ssrc={rssrc} uid={uid}");
            }
            Err(e) => tracing::warn!("DAVE RX secret uid={uid}: {e}"),
        }
    }
}

async fn session(
    info: &VoiceConnInfo,
    mut control: mpsc::UnboundedReceiver<VoiceControl>,
    events: &mpsc::UnboundedSender<AppEvent>,
) -> Result<()> {
    let host = endpoint_host(&info.endpoint);
    // v8: necesario para negociar DAVE (E2EE). Declaramos no soportarlo para
    // intentar degradar a transporte no-E2EE en canales que lo permitan.
    let url = format!("wss://{host}/?v=8");
    tracing::info!("voice gateway: {url}");
    let (ws, _) = tokio_tungstenite::connect_async(&url)
        .await
        .context("conectando al voice gateway")?;
    let (mut sink, mut stream) = ws.split();

    // El Voice Gateway espera el IDENTIFY nada más conectar.
    // `max_dave_protocol_version: 1` = soportamos E2EE/DAVE v1.
    let identify = serde_json::json!({
        "op": 0,
        "d": {
            "server_id": info.guild_id,
            "user_id": info.user_id,
            "session_id": info.session_id,
            "token": info.token,
            "max_dave_protocol_version": dave::PROTOCOL_VERSION,
        }
    });
    sink.send(WsMessage::Text(identify.to_string())).await?;

    let mut heartbeat = tokio::time::interval(Duration::from_secs(13));
    let mut got_ack = true;
    // No enviar heartbeats hasta recibir HELLO y enviar IDENTIFY (si no, Discord
    // cierra la conexión por "no autenticado").
    let mut hello_seen = false;
    // Última secuencia recibida (v8 la incluye en `seq` y la pide en el heartbeat).
    let mut last_seq: Option<u64> = None;

    // Datos que se van rellenando durante el handshake.
    let mut ssrc: u32 = 0;
    let mut udp: Option<UdpSocket> = None;
    let mut server_addr: Option<String> = None;
    let mut local_addr: Option<(String, u16)> = None;
    let mut chosen_mode: Option<String> = None;

    // Estado DAVE/E2EE.
    let mut mls: Option<dave::MlsSession> = None;
    // Mapeo SSRC→user_id aprendido de op5 SPEAKING (puede llegar antes o después
    // del Welcome; por eso lo guardamos y derivamos cuando ambos están listos).
    let mut ssrc_uid: HashMap<u32, u64> = HashMap::new();
    // Transición de epoch pendiente de ejecutar (op29 commit recibido con
    // transition_id != 0; las claves nuevas se activan al llegar op22 con ese id).
    let mut pending_transition: Option<u16> = None;
    // Transición de DOWNGRADE pendiente (op21 con protocol_version 0 → salir de
    // E2EE; al ejecutarse en op22 se desactiva DAVE y se sigue solo con transporte).
    let mut downgrade_transition: Option<u16> = None;

    let shared = Shared {
        stop: Arc::new(AtomicBool::new(false)),
        mute: Arc::new(AtomicBool::new(false)),
        deaf: Arc::new(AtomicBool::new(false)),
        e2ee: Arc::new(Mutex::new(None)),
        rx_e2ee: Arc::new(Mutex::new(HashMap::new())),
        e2ee_active: Arc::new(AtomicBool::new(false)),
    };
    let mut audio_started = false;

    loop {
        tokio::select! {
            _ = heartbeat.tick() => {
                if !hello_seen {
                    continue; // aún no identificados; no latir
                }
                if !got_ack {
                    bail!("sin ACK de heartbeat de voz");
                }
                // Formato v8: d = { t: nonce, seq_ack: última_seq }.
                let hb = serde_json::json!({
                    "op": 3,
                    "d": { "t": now_ms(), "seq_ack": last_seq }
                });
                sink.send(WsMessage::Text(hb.to_string())).await?;
                got_ack = false;
            }

            ctrl = control.recv() => {
                match ctrl {
                    Some(VoiceControl::Mute(m)) => shared.mute.store(m, Ordering::Relaxed),
                    Some(VoiceControl::Deaf(d)) => shared.deaf.store(d, Ordering::Relaxed),
                    Some(VoiceControl::Stop) | None => {
                        shared.stop.store(true, Ordering::Relaxed);
                        return Ok(());
                    }
                }
            }

            frame = stream.next() => {
                let frame = match frame { Some(f) => f?, None => bail!("voice ws cerrado") };
                let text = match frame {
                    WsMessage::Text(t) => t,
                    WsMessage::Binary(b) => {
                        // Mensajes binarios DAVE/MLS del servidor: seq u16 BE + opcode u8 + payload.
                        log_dave_binary(&b);
                        if b.len() < 3 {
                            continue;
                        }
                        let dop = b[2];
                        let payload = &b[3..];
                        match dop {
                            dave::op::EXTERNAL_SENDER => {
                                if let Some(m) = mls.as_mut() {
                                    if let Err(e) = m.set_external_sender(payload) {
                                        tracing::warn!("DAVE external sender: {e}");
                                    } else {
                                        tracing::info!("DAVE: external sender guardado");
                                    }
                                }
                            }
                            dave::op::WELCOME => {
                                // payload = [transition_id u16][Welcome bare]
                                let transition_id = if payload.len() >= 2 {
                                    u16::from_be_bytes([payload[0], payload[1]])
                                } else { 0 };
                                if let Some(m) = mls.as_mut() {
                                    match m.process_welcome(payload) {
                                        Ok(()) => {
                                            tracing::info!("DAVE: Welcome procesado, dentro del grupo MLS (epoch {})", m.epoch());
                                            install_media_keys(m, &shared, &ssrc_uid);
                                            let _ = events.send(AppEvent::VoiceUpdate {
                                                connected: true,
                                                text: "voz: conectado (E2EE)".into(),
                                            });
                                            // Confirmar la transición (si no, 4006 "session no longer valid").
                                            let ready = serde_json::json!({
                                                "op": dave::op::READY_FOR_TRANSITION,
                                                "d": { "transition_id": transition_id }
                                            });
                                            sink.send(WsMessage::Text(ready.to_string())).await?;
                                            tracing::info!("DAVE: transition_ready enviado (transition_id={transition_id})");
                                        }
                                        Err(e) => tracing::warn!("DAVE welcome: {e}"),
                                    }
                                }
                            }
                            dave::op::ANNOUNCE_COMMIT => {
                                // payload = [transition_id u16][MLSMessage commit].
                                // El gateway lo manda cuando un miembro entra/sale: avanza
                                // el epoch MLS y re-deriva las claves de medios.
                                if payload.len() < 2 {
                                    continue;
                                }
                                let transition_id = u16::from_be_bytes([payload[0], payload[1]]);
                                let commit = &payload[2..];
                                if let Some(m) = mls.as_mut() {
                                    match m.process_commit(commit) {
                                        Ok(()) => {
                                            tracing::info!("DAVE: commit procesado, epoch={} (transition_id={transition_id})", m.epoch());
                                            if transition_id == 0 {
                                                // Transición inmediata: instala las claves ya.
                                                install_media_keys(m, &shared, &ssrc_uid);
                                            } else {
                                                // Espera op22 EXECUTE_TRANSITION para activarlas.
                                                pending_transition = Some(transition_id);
                                            }
                                            let ready = serde_json::json!({
                                                "op": dave::op::READY_FOR_TRANSITION,
                                                "d": { "transition_id": transition_id }
                                            });
                                            sink.send(WsMessage::Text(ready.to_string())).await?;
                                            tracing::info!("DAVE: transition_ready enviado (transition_id={transition_id})");
                                        }
                                        Err(e) => tracing::warn!("DAVE commit: {e}"),
                                    }
                                }
                            }
                            _ => { /* PROPOSALS(27): como joiner no commiteamos */ }
                        }
                        continue;
                    }
                    WsMessage::Close(c) => {
                        let info = c
                            .map(|f| format!("{:?}: {}", f.code, f.reason))
                            .unwrap_or_else(|| "sin detalle".into());
                        bail!("voice ws close ({info})");
                    }
                    WsMessage::Ping(p) => { sink.send(WsMessage::Pong(p)).await?; continue; }
                    _ => continue,
                };
                let v: serde_json::Value = serde_json::from_str(&text)?;
                if let Some(s) = v.get("seq").and_then(|x| x.as_u64()) {
                    last_seq = Some(s);
                }
                let op = v.get("op").and_then(|o| o.as_u64()).unwrap_or(999);
                let d = v.get("d").cloned().unwrap_or(serde_json::Value::Null);

                match op {
                    8 => { // HELLO (el IDENTIFY ya se envió al conectar)
                        if let Some(iv) = d.get("heartbeat_interval").and_then(|x| x.as_f64()) {
                            heartbeat = tokio::time::interval(Duration::from_millis(iv as u64));
                            heartbeat.tick().await; // consume el tick inmediato
                        }
                        hello_seen = true;
                    }
                    2 => { // READY
                        ssrc = d.get("ssrc").and_then(|x| x.as_u64()).unwrap_or(0) as u32;
                        let ip = d.get("ip").and_then(|x| x.as_str()).unwrap_or("").to_string();
                        let port = d.get("port").and_then(|x| x.as_u64()).unwrap_or(0) as u16;
                        let modes: Vec<String> = d.get("modes")
                            .and_then(|m| m.as_array())
                            .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
                            .unwrap_or_default();
                        if ssrc == 0 || ip.is_empty() || port == 0 {
                            bail!("READY de voz incompleto");
                        }
                        let mode = pick_mode(&modes)?;
                        chosen_mode = Some(mode.clone());

                        let addr = format!("{ip}:{port}");
                        let sock = UdpSocket::bind("0.0.0.0:0").context("bind UDP voz")?;
                        sock.connect(&addr).context("connect UDP voz")?;
                        sock.set_read_timeout(Some(Duration::from_secs(3)))?;
                        let (lip, lport) = ip_discovery(&sock, ssrc)?;
                        tracing::info!("IP discovery: {lip}:{lport}, modo {mode}");
                        local_addr = Some((lip, lport));
                        server_addr = Some(addr);
                        udp = Some(sock);

                        // SELECT PROTOCOL
                        let (lip, lport) = local_addr.clone().unwrap();
                        let select = serde_json::json!({
                            "op": 1,
                            "d": {
                                "protocol": "udp",
                                "data": { "address": lip, "port": lport, "mode": mode }
                            }
                        });
                        sink.send(WsMessage::Text(select.to_string())).await?;
                    }
                    4 => { // SESSION DESCRIPTION (select_protocol_ack)
                        if let Some(v) = d.get("dave_protocol_version").and_then(|x| x.as_u64()) {
                            tracing::info!("DAVE: versión de protocolo negociada = {v}");
                            if v >= 1 && mls.is_none() {
                                let uid: u64 = info.user_id.parse().unwrap_or(0);
                                match dave::MlsSession::new(uid) {
                                    Ok(s) => {
                                        mls = Some(s);
                                        tracing::info!("DAVE: sesión MLS creada (E2EE activo)");
                                    }
                                    Err(e) => tracing::warn!("DAVE: no se pudo crear MLS: {e}"),
                                }
                                // La spec: enviar KeyPackage tras op 4 con versión != 0.
                                if let Some(m) = mls.as_ref() {
                                    match m.key_package_bytes() {
                                        Ok(kp) => {
                                            let msg = dave::client_binary(dave::op::KEY_PACKAGE, &kp);
                                            sink.send(WsMessage::Binary(msg)).await?;
                                            tracing::info!("DAVE: KeyPackage enviado ({} bytes)", kp.len());
                                        }
                                        Err(e) => tracing::warn!("DAVE key package: {e}"),
                                    }
                                }
                            }
                        }
                        let key: Vec<u8> = d.get("secret_key")
                            .and_then(|k| k.as_array())
                            .map(|a| a.iter().filter_map(|x| x.as_u64().map(|n| n as u8)).collect())
                            .unwrap_or_default();
                        if key.len() != 32 {
                            bail!("secret_key inválida ({} bytes)", key.len());
                        }
                        let sock = udp.take().ok_or_else(|| anyhow!("UDP no listo"))?;
                        let saddr = server_addr.clone().ok_or_else(|| anyhow!("addr no lista"))?;
                        let _ = saddr; // el socket ya está 'connect'-ado

                        // SPEAKING (necesario antes de transmitir).
                        let speaking = serde_json::json!({
                            "op": 5, "d": { "speaking": 1, "delay": 0, "ssrc": ssrc }
                        });
                        sink.send(WsMessage::Text(speaking.to_string())).await?;

                        start_audio(
                            sock,
                            key,
                            ssrc,
                            chosen_mode.clone().unwrap_or_else(|| PREFERRED_MODE.into()),
                            &shared,
                        )?;
                        audio_started = true;
                        let _ = events.send(AppEvent::VoiceUpdate {
                            connected: true,
                            text: "voz: conectado".into(),
                        });
                    }
                    5 => { // SPEAKING (de otro participante): mapea ssrc↔user_id
                        let uid = d.get("user_id")
                            .and_then(|x| x.as_str())
                            .and_then(|s| s.parse::<u64>().ok());
                        let rssrc = d.get("ssrc").and_then(|x| x.as_u64()).map(|n| n as u32);
                        if let (Some(uid), Some(rssrc)) = (uid, rssrc) {
                            if rssrc != ssrc {
                                ssrc_uid.insert(rssrc, uid);
                                // Si ya estamos en el grupo MLS, deriva su clave RX ahora.
                                if let Some(m) = mls.as_ref() {
                                    if m.has_group() && !shared.rx_e2ee.lock().unwrap().contains_key(&rssrc) {
                                        match m.media_base_secret_for(uid) {
                                            Ok(s) => {
                                                shared.rx_e2ee.lock().unwrap()
                                                    .insert(rssrc, dave::FrameCryptor::from_base_secret(&s));
                                                tracing::info!("DAVE RX: clave para ssrc={rssrc} uid={uid}");
                                            }
                                            Err(e) => tracing::warn!("DAVE RX secret uid={uid}: {e}"),
                                        }
                                    }
                                }
                            }
                        }
                    }
                    6 => { got_ack = true; } // HEARTBEAT ACK
                    21 => { // PREPARE_TRANSITION: anuncia una transición próxima
                        tracing::info!("DAVE op 21 PREPARE_TRANSITION: {}", d);
                        let pv = d.get("protocol_version").and_then(|x| x.as_u64()).unwrap_or(1);
                        let tid = d.get("transition_id").and_then(|x| x.as_u64()).map(|n| n as u16);
                        // protocol_version 0 = downgrade a transporte sin E2EE.
                        if pv == 0 {
                            if let Some(tid) = tid {
                                downgrade_transition = Some(tid);
                                tracing::info!("DAVE: downgrade a no-E2EE anunciado (transition_id={tid})");
                            }
                        }
                    }
                    22 => { // EXECUTE_TRANSITION: aplica la transición anunciada
                        tracing::info!("DAVE op 22 EXECUTE_TRANSITION: {}", d);
                        let tid = d.get("transition_id").and_then(|x| x.as_u64()).map(|n| n as u16);
                        if let Some(tid) = tid {
                            if downgrade_transition == Some(tid) {
                                // Downgrade: desactiva E2EE; el transporte sigue cifrado
                                // (XChaCha20). TX manda Opus en claro y RX decodifica directo.
                                shared.e2ee_active.store(false, Ordering::Relaxed);
                                *shared.e2ee.lock().unwrap() = None;
                                shared.rx_e2ee.lock().unwrap().clear();
                                downgrade_transition = None;
                                tracing::info!("DAVE: transición {tid} ejecutada — E2EE DESACTIVADO (no-E2EE)");
                            } else if pending_transition == Some(tid) {
                                if let Some(m) = mls.as_ref() {
                                    install_media_keys(m, &shared, &ssrc_uid);
                                    tracing::info!("DAVE: transición {tid} ejecutada — claves del nuevo epoch activas");
                                }
                                pending_transition = None;
                            }
                        }
                    }
                    // Resto de opcodes DAVE en JSON (epoch/invalid): se loguean.
                    23 | 24 | 31 => {
                        tracing::info!("DAVE op {op} (JSON): {}", d);
                    }
                    other => { tracing::debug!("voice op {other} no manejado: {d}"); }
                }
            }
        }

        if shared.stop.load(Ordering::Relaxed) {
            break;
        }
    }

    let _ = audio_started;
    shared.stop.store(true, Ordering::Relaxed);
    Ok(())
}

/// Lanza los hilos de audio (captura/codifica/cifra/envía y recibe/descifra/
/// decodifica/reproduce). cpal corre en sus propios hilos; nada cruza `await`.
fn start_audio(
    sock: UdpSocket,
    key: Vec<u8>,
    ssrc: u32,
    mode: String,
    shared: &Shared,
) -> Result<()> {
    let stop = shared.stop.clone();
    let mute = shared.mute.clone();
    let deaf = shared.deaf.clone();
    let e2ee = shared.e2ee.clone();
    let rx_e2ee = shared.rx_e2ee.clone();
    let e2ee_active = shared.e2ee_active.clone();

    // Buffers compartidos con los callbacks de cpal (i16 interleaved estéreo).
    let mic_buf: Arc<Mutex<VecDeque<i16>>> = Arc::new(Mutex::new(VecDeque::new()));
    let play_buf: Arc<Mutex<VecDeque<i16>>> = Arc::new(Mutex::new(VecDeque::new()));

    // Hilo dueño de cpal + UDP. cpal::Stream no es Send, así que vive aquí.
    std::thread::spawn({
        let mic_buf = mic_buf.clone();
        let play_buf = play_buf.clone();
        move || {
            if let Err(e) = audio_thread(
                sock, key, ssrc, mode, stop, mute, deaf, e2ee, rx_e2ee, e2ee_active, mic_buf,
                play_buf,
            ) {
                tracing::warn!("hilo de audio terminó: {e}");
            }
        }
    });
    Ok(())
}

/// Puerta de ruido (noise gate) adaptativa para suprimir el **ruido blanco/hiss**
/// constante del micrófono cuando no se habla. Estima el piso de ruido a partir
/// de las tramas silenciosas y abre la puerta solo cuando la energía (RMS) supera
/// ese piso por un margen; con histéresis (abrir/cerrar distintos) y una
/// envolvente de ganancia (ataque rápido, liberación lenta) para no producir
/// clics ni cortar el final de las palabras. Durante el habla la puerta queda
/// abierta y el audio pasa íntegro (el hiss queda enmascarado por la voz).
struct NoiseGate {
    gain: f32,        // ganancia actual aplicada (0..1), suavizada
    hold: u32,        // tramas restantes manteniendo la puerta abierta
    noise_floor: f64, // estimación del piso de ruido (RMS) aprendida
}

impl NoiseGate {
    fn new() -> Self {
        Self { gain: 0.0, hold: 0, noise_floor: 200.0 }
    }

    /// Procesa una trama PCM in situ, atenuándola si se considera ruido de fondo.
    fn process(&mut self, pcm: &mut [i16]) {
        // ~0.5 s de "mantener abierto" tras detectar voz (a 50 tramas/s).
        const HOLD_FRAMES: u32 = 25;

        // Energía RMS de la trama.
        let mut sum = 0.0f64;
        for &s in pcm.iter() {
            let v = s as f64;
            sum += v * v;
        }
        let rms = (sum / pcm.len().max(1) as f64).sqrt();

        // Umbrales relativos al piso de ruido aprendido, con suelo mínimo absoluto
        // (evita abrir con micrófonos muy silenciosos). Histéresis: una vez abierta
        // basta menos energía para mantenerla, así no parpadea en pausas breves.
        let open = (self.noise_floor * 4.0).max(300.0);
        let close = (self.noise_floor * 2.5).max(180.0);
        let want_open = if self.gain > 0.5 { rms > close } else { rms > open };

        if want_open {
            self.hold = HOLD_FRAMES;
        } else {
            if self.hold > 0 {
                self.hold -= 1;
            }
            // Aprende el piso de ruido SOLO en silencio (puerta cerrada).
            self.noise_floor = self.noise_floor * 0.99 + rms * 0.01;
        }

        let target = if self.hold > 0 { 1.0 } else { 0.0 };
        let coeff = if target > self.gain { 0.6 } else { 0.04 }; // ataque > liberación
        self.gain += (target - self.gain) * coeff;
        if self.gain < 0.001 {
            self.gain = 0.0;
        }
        if self.gain < 0.999 {
            for s in pcm.iter_mut() {
                *s = (*s as f32 * self.gain) as i16;
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn audio_thread(
    sock: UdpSocket,
    key: Vec<u8>,
    ssrc: u32,
    _mode: String,
    stop: Arc<AtomicBool>,
    mute: Arc<AtomicBool>,
    deaf: Arc<AtomicBool>,
    e2ee: Arc<Mutex<Option<dave::FrameCryptor>>>,
    rx_e2ee: Arc<Mutex<HashMap<u32, dave::FrameCryptor>>>,
    e2ee_active: Arc<AtomicBool>,
    mic_buf: Arc<Mutex<VecDeque<i16>>>,
    play_buf: Arc<Mutex<VecDeque<i16>>>,
) -> Result<()> {
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

    let host = cpal::default_host();
    let in_dev = host
        .default_input_device()
        .ok_or_else(|| anyhow!("sin micrófono por defecto"))?;
    let out_dev = host
        .default_output_device()
        .ok_or_else(|| anyhow!("sin altavoz por defecto"))?;

    let in_cfg = in_dev.default_input_config()?;
    let out_cfg = out_dev.default_output_config()?;
    let in_ch = in_cfg.channels() as usize;
    let out_ch = out_cfg.channels() as usize;
    tracing::info!(
        "audio in: '{}' {} Hz, {} canales, {:?}",
        in_dev.name().unwrap_or_else(|_| "?".into()),
        in_cfg.sample_rate().0,
        in_ch,
        in_cfg.sample_format()
    );
    tracing::info!(
        "audio out: '{}' {} Hz, {} canales, {:?}",
        out_dev.name().unwrap_or_else(|_| "?".into()),
        out_cfg.sample_rate().0,
        out_ch,
        out_cfg.sample_format()
    );
    if in_cfg.sample_rate().0 != SAMPLE_RATE || out_cfg.sample_rate().0 != SAMPLE_RATE {
        tracing::warn!(
            "⚠️ dispositivos NO están a 48 kHz (in={}, out={}) y NO hay remuestreo: \
             el audio sonará a velocidad/tono incorrecto o no se entenderá",
            in_cfg.sample_rate().0,
            out_cfg.sample_rate().0
        );
    }

    // --- Stream de captura (micrófono) → mic_buf (estéreo i16) -------------
    let in_stream = {
        let mic_buf = mic_buf.clone();
        let err_fn = |e| tracing::warn!("error stream entrada: {e}");
        match in_cfg.sample_format() {
            cpal::SampleFormat::F32 => in_dev.build_input_stream(
                &in_cfg.clone().into(),
                move |data: &[f32], _: &_| {
                    let mut b = mic_buf.lock().unwrap();
                    push_input_i16(&mut b, data, in_ch, |s| (s.clamp(-1.0, 1.0) * 32767.0) as i16);
                    cap(&mut b, SAMPLE_RATE as usize * CHANNELS); // ~1 s máx
                },
                err_fn,
                None,
            )?,
            cpal::SampleFormat::I16 => in_dev.build_input_stream(
                &in_cfg.clone().into(),
                move |data: &[i16], _: &_| {
                    let mut b = mic_buf.lock().unwrap();
                    push_input_i16(&mut b, data, in_ch, |s| s);
                    cap(&mut b, SAMPLE_RATE as usize * CHANNELS);
                },
                err_fn,
                None,
            )?,
            other => bail!("formato de entrada no soportado: {other:?}"),
        }
    };

    // --- Stream de reproducción ← play_buf --------------------------------
    let out_stream = {
        let play_buf = play_buf.clone();
        let deaf = deaf.clone();
        let err_fn = |e| tracing::warn!("error stream salida: {e}");
        match out_cfg.sample_format() {
            cpal::SampleFormat::F32 => out_dev.build_output_stream(
                &out_cfg.clone().into(),
                move |data: &mut [f32], _: &_| {
                    let mut b = play_buf.lock().unwrap();
                    let silent = deaf.load(Ordering::Relaxed);
                    fill_output(data, &mut b, out_ch, silent, |s| s as f32 / 32767.0);
                },
                err_fn,
                None,
            )?,
            cpal::SampleFormat::I16 => out_dev.build_output_stream(
                &out_cfg.clone().into(),
                move |data: &mut [i16], _: &_| {
                    let mut b = play_buf.lock().unwrap();
                    let silent = deaf.load(Ordering::Relaxed);
                    fill_output(data, &mut b, out_ch, silent, |s| s);
                },
                err_fn,
                None,
            )?,
            other => bail!("formato de salida no soportado: {other:?}"),
        }
    };

    in_stream.play()?;
    out_stream.play()?;

    // Cifrador y códecs.
    let cipher = XChaCha20Poly1305::new(Key::from_slice(&key));
    let mut encoder = Encoder::new(SampleRate::Hz48000, Channels::Stereo, Application::Voip)?;
    let mut decoders: HashMap<u32, Decoder> = HashMap::new();

    // Hilo de recepción (RX): UDP → descifrar → Opus → play_buf.
    let rx = {
        let sock = sock.try_clone()?;
        let key = key.clone();
        let stop = stop.clone();
        let deaf = deaf.clone();
        let play_buf = play_buf.clone();
        std::thread::spawn(move || {
            rx_loop(sock, key, stop, deaf, rx_e2ee, e2ee_active, play_buf)
        })
    };

    // TX en este hilo: mic_buf → Opus → cifrar → UDP.
    let mut seq: u16 = rand::random();
    let mut timestamp: u32 = rand::random();
    let mut nonce_ctr: u32 = 0;
    let mut pcm = vec![0i16; FRAME_LEN];
    let mut opus_out = [0u8; 4000];
    let mut gate = NoiseGate::new();
    // Diagnóstico TX.
    let mut frames_sent: u64 = 0;
    let mut empty_waits: u64 = 0;
    let mut warned_no_mic = false;

    while !stop.load(Ordering::Relaxed) {
        // Espera a tener un frame completo de micrófono.
        let ready = {
            let b = mic_buf.lock().unwrap();
            b.len() >= FRAME_LEN
        };
        if !ready {
            empty_waits += 1;
            // Si tras ~3 s el micro nunca llenó un frame, el stream de captura no
            // está produciendo audio (dispositivo/permiso). Avisar una vez.
            if !warned_no_mic && empty_waits == 600 {
                tracing::warn!("TX: el micrófono no produce audio (mic_buf vacío ~3s) — ¿permiso/micro?");
                warned_no_mic = true;
            }
            std::thread::sleep(Duration::from_millis(5));
            continue;
        }
        {
            let mut b = mic_buf.lock().unwrap();
            for s in pcm.iter_mut() {
                *s = b.pop_front().unwrap_or(0);
            }
        }

        if mute.load(Ordering::Relaxed) {
            // Avanza marcas de tiempo aunque no se transmita.
            seq = seq.wrapping_add(1);
            timestamp = timestamp.wrapping_add(FRAME_SAMPLES as u32);
            continue;
        }

        // Supresor de ruido blanco: atenúa la trama si es ruido de fondo del micro.
        gate.process(&mut pcm);

        let n = encoder.encode(&pcm, &mut opus_out)?;

        // E2EE (DAVE): si el grupo MLS ya está listo, envuelve el Opus en el
        // formato de frame cifrado antes del cifrado de transporte. Mientras el
        // handshake no termine (primeros ms), se envía el Opus en claro.
        let e2ee_frame: Option<Vec<u8>> = match e2ee.lock().unwrap().as_mut() {
            Some(cryptor) => Some(cryptor.encrypt(&opus_out[..n])?),
            None => None,
        };
        let opus: &[u8] = match &e2ee_frame {
            Some(f) => f,
            None => &opus_out[..n],
        };

        let header = rtp_header(seq, timestamp, ssrc);
        let mut nonce = [0u8; 24];
        nonce[..4].copy_from_slice(&nonce_ctr.to_be_bytes());
        let ct = cipher
            .encrypt(XNonce::from_slice(&nonce), Payload { msg: opus, aad: &header })
            .map_err(|_| anyhow!("fallo cifrando voz"))?;

        let mut packet = Vec::with_capacity(12 + ct.len() + 4);
        packet.extend_from_slice(&header);
        packet.extend_from_slice(&ct);
        packet.extend_from_slice(&nonce_ctr.to_be_bytes());
        let sent = sock.send(&packet);

        frames_sent += 1;
        if frames_sent == 1 || frames_sent % 250 == 0 {
            tracing::info!(
                "TX: {} frames enviados (e2ee={}, opus={}B, paquete={}B, send={:?})",
                frames_sent,
                e2ee_frame.is_some(),
                n,
                packet.len(),
                sent
            );
        }

        seq = seq.wrapping_add(1);
        timestamp = timestamp.wrapping_add(FRAME_SAMPLES as u32);
        nonce_ctr = nonce_ctr.wrapping_add(1);
    }

    let _ = rx.join();
    let _ = &mut decoders; // (los decoders viven en rx_loop)
    drop(in_stream);
    drop(out_stream);
    Ok(())
}

/// Bucle de recepción de audio.
fn rx_loop(
    sock: UdpSocket,
    key: Vec<u8>,
    stop: Arc<AtomicBool>,
    deaf: Arc<AtomicBool>,
    rx_e2ee: Arc<Mutex<HashMap<u32, dave::FrameCryptor>>>,
    e2ee_active: Arc<AtomicBool>,
    play_buf: Arc<Mutex<VecDeque<i16>>>,
) {
    let cipher = XChaCha20Poly1305::new(Key::from_slice(&key));
    let mut decoders: HashMap<u32, Decoder> = HashMap::new();
    let mut buf = [0u8; 4096];
    let mut pcm = vec![0i16; FRAME_LEN];
    // Contadores RX. `relleno` = frames de silencio/marcadores del emisor (sin
    // magic DAVE), que NO son audio y no deben contarse como fallo real.
    let (mut recv, mut transport_fail, mut dave_fail, mut relleno, mut no_key, mut decoded) =
        (0u64, 0u64, 0u64, 0u64, 0u64, 0u64);
    let report = |recv, transport_fail, dave_fail, relleno, no_key, decoded| {
        tracing::info!(
            "RX: recibidos={recv}, fallo_transporte={transport_fail}, fallo_dave={dave_fail}, \
             relleno={relleno}, sin_clave={no_key}, decodificados={decoded}"
        );
    };

    while !stop.load(Ordering::Relaxed) {
        let n = match sock.recv(&mut buf) {
            Ok(n) => n,
            Err(_) => continue, // timeout u otro: reintentar
        };
        if n < 12 + 4 + 16 || deaf.load(Ordering::Relaxed) {
            continue;
        }
        recv += 1;
        if recv == 1 {
            tracing::info!("RX: primer paquete recibido ({n}B)");
        }
        // Reporte periódico ANTES de cualquier `continue`: así vemos paquetes que
        // llegan pero fallan en transporte/DAVE/sin_clave (antes quedaban mudos).
        if recv % 250 == 0 {
            report(recv, transport_fail, dave_fail, relleno, no_key, decoded);
        }
        let packet = &buf[..n];
        let hlen = match unencrypted_prefix_len(packet, n) {
            Some(h) => h,
            None => continue,
        };
        let ssrc = u32::from_be_bytes([packet[8], packet[9], packet[10], packet[11]]);
        let aad = &packet[..hlen];
        let ct = &packet[hlen..n - 4];
        let mut nonce = [0u8; 24];
        nonce[..4].copy_from_slice(&packet[n - 4..n]);

        let plain = match cipher.decrypt(XNonce::from_slice(&nonce), Payload { msg: ct, aad }) {
            Ok(p) => p,
            Err(_) => {
                transport_fail += 1;
                continue;
            }
        };
        if plain.is_empty() {
            continue;
        }

        // Capa DAVE (E2EE): si tenemos la clave de este emisor, desenvuelve el
        // frame para obtener el Opus. Con E2EE activo pero sin clave todavía
        // (op5 SPEAKING aún no llegó o Welcome no procesado), descarta el frame:
        // su contenido es ciphertext DAVE, no Opus en claro.
        let opus: Vec<u8> = if let Some(cryptor) = rx_e2ee.lock().unwrap().get(&ssrc) {
            match cryptor.decrypt(&plain) {
                Ok(o) => o,
                Err(_) => {
                    // Sin magic DAVE = frame de relleno/silencio del emisor (no
                    // audio); no es un fallo real. Solo avisamos de fallos de tag.
                    if dave::has_dave_magic(&plain) {
                        dave_fail += 1;
                        if dave_fail == 1 || dave_fail % 250 == 0 {
                            tracing::warn!("RX: fallo descifrado DAVE (ssrc={ssrc}, n={dave_fail})");
                        }
                    } else {
                        relleno += 1;
                    }
                    continue;
                }
            }
        } else if e2ee_active.load(Ordering::Relaxed) {
            no_key += 1;
            continue;
        } else {
            plain
        };

        let dec = decoders
            .entry(ssrc)
            .or_insert_with(|| Decoder::new(SampleRate::Hz48000, Channels::Stereo).unwrap());
        match dec.decode(Some(&opus), &mut pcm, false) {
            Ok(samples) => {
                decoded += 1;
                if decoded == 1 || decoded % 250 == 0 {
                    report(recv, transport_fail, dave_fail, relleno, no_key, decoded);
                }
                let mut b = play_buf.lock().unwrap();
                for &s in &pcm[..samples * CHANNELS] {
                    b.push_back(s);
                }
                cap(&mut b, SAMPLE_RATE as usize * CHANNELS);
            }
            Err(_) => continue,
        }
    }
    report(recv, transport_fail, dave_fail, relleno, no_key, decoded);
}

// --- Helpers ---------------------------------------------------------------

fn rtp_header(seq: u16, timestamp: u32, ssrc: u32) -> [u8; 12] {
    let mut h = [0u8; 12];
    h[0] = 0x80;
    h[1] = 0x78;
    h[2..4].copy_from_slice(&seq.to_be_bytes());
    h[4..8].copy_from_slice(&timestamp.to_be_bytes());
    h[8..12].copy_from_slice(&ssrc.to_be_bytes());
    h
}

/// Longitud del prefijo **no cifrado** en modo `aead_xchacha20_poly1305_rtpsize`:
/// header RTP (12B) + CSRC (4·cc) + —si el bit X está activo— el encabezado de
/// extensión de 4B (`profile`+`length`). El **cuerpo** de la extensión y el
/// payload van *cifrados* (forman el ciphertext junto con el tag), y la cola de
/// 4B es el nonce. Confirmado empíricamente contra paquetes reales de Discord:
/// `ct=[16..n-4]`, `aad=[..16]` para un paquete con extensión y cc=0.
/// Devuelve `None` si no hay espacio para el prefijo + nonce(4) + tag(16).
fn unencrypted_prefix_len(packet: &[u8], n: usize) -> Option<usize> {
    if n < 12 {
        return None;
    }
    let first = packet[0];
    let cc = (first & 0x0F) as usize;
    let mut len = 12 + 4 * cc;
    if first & 0x10 != 0 {
        len += 4; // profile(2) + length(2); el cuerpo de la extensión va cifrado
    }
    if n < len + 4 + 16 {
        return None; // sin sitio para nonce(4) + tag Poly1305(16)
    }
    Some(len)
}

/// IP discovery: envía un paquete de 74 bytes y lee la IP/puerto externos.
fn ip_discovery(sock: &UdpSocket, ssrc: u32) -> Result<(String, u16)> {
    let mut req = [0u8; 74];
    req[0..2].copy_from_slice(&0x1u16.to_be_bytes()); // type = 0x0001 (request)
    req[2..4].copy_from_slice(&70u16.to_be_bytes()); // length = 70
    req[4..8].copy_from_slice(&ssrc.to_be_bytes());
    sock.send(&req).context("enviando IP discovery")?;

    let mut resp = [0u8; 74];
    let n = sock.recv(&mut resp).context("recibiendo IP discovery")?;
    if n < 74 {
        bail!("respuesta de IP discovery corta ({n})");
    }
    // address: bytes 8..72 (string terminada en NUL); port: 72..74 (BE).
    let end = resp[8..72].iter().position(|&b| b == 0).unwrap_or(64) + 8;
    let ip = String::from_utf8_lossy(&resp[8..end]).to_string();
    let port = u16::from_be_bytes([resp[72], resp[73]]);
    if ip.is_empty() || port == 0 {
        bail!("IP discovery inválido");
    }
    Ok((ip, port))
}

fn pick_mode(modes: &[String]) -> Result<String> {
    if modes.iter().any(|m| m == PREFERRED_MODE) {
        Ok(PREFERRED_MODE.to_string())
    } else {
        bail!(
            "el servidor no ofrece {PREFERRED_MODE} (modos: {:?})",
            modes
        )
    }
}

/// Parsea y loguea un mensaje binario DAVE (seq u16 BE + opcode u8 + payload).
fn log_dave_binary(buf: &[u8]) {
    if buf.len() < 3 {
        tracing::warn!("DAVE binario demasiado corto ({} bytes)", buf.len());
        return;
    }
    let seq = u16::from_be_bytes([buf[0], buf[1]]);
    let op = buf[2];
    let payload = &buf[3..];
    let name = match op {
        dave::op::EXTERNAL_SENDER => "EXTERNAL_SENDER(25)",
        dave::op::PROPOSALS => "PROPOSALS(27)",
        dave::op::ANNOUNCE_COMMIT => "ANNOUNCE_COMMIT(29)",
        dave::op::WELCOME => "WELCOME(30)",
        _ => "DESCONOCIDO",
    };
    tracing::info!(
        "DAVE binario: seq={seq} op={op} {name} payload={} bytes",
        payload.len()
    );
}

fn endpoint_host(endpoint: &str) -> String {
    endpoint
        .trim_start_matches("wss://")
        .split('/')
        .next()
        .unwrap_or(endpoint)
        .to_string()
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Acota un buffer a `max` elementos descartando los más antiguos.
fn cap(b: &mut VecDeque<i16>, max: usize) {
    while b.len() > max {
        b.pop_front();
    }
}

/// Inserta muestras del micrófono normalizadas a estéreo i16.
fn push_input_i16<T: Copy>(
    out: &mut VecDeque<i16>,
    data: &[T],
    in_ch: usize,
    conv: impl Fn(T) -> i16,
) {
    if in_ch == 0 {
        return;
    }
    for frame in data.chunks(in_ch) {
        let l = conv(frame[0]);
        let r = if in_ch >= 2 { conv(frame[1]) } else { l };
        out.push_back(l);
        out.push_back(r);
    }
}

/// Rellena el buffer de salida desde el de reproducción (estéreo i16 fuente).
fn fill_output<T: Copy + Default>(
    data: &mut [T],
    src: &mut VecDeque<i16>,
    out_ch: usize,
    silent: bool,
    conv: impl Fn(i16) -> T,
) {
    if out_ch == 0 {
        return;
    }
    for frame in data.chunks_mut(out_ch) {
        let (l, r) = if silent {
            (0i16, 0i16)
        } else {
            (src.pop_front().unwrap_or(0), src.pop_front().unwrap_or(0))
        };
        for (i, slot) in frame.iter_mut().enumerate() {
            *slot = match i {
                0 => conv(l),
                1 => conv(r),
                _ => T::default(),
            };
        }
    }
}
