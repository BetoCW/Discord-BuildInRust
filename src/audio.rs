//! Audio compartido entre la UI y el subsistema de voz: opciones EN VIVO
//! (volúmenes, dispositivos, procesado), cadena de procesamiento del micrófono
//! (ganancia → AGC → anti-eco → puerta de ruido) y la prueba de micrófono de
//! «Ajustes de voz» (estilo Discord: hablas y te lo reproducimos).
//!
//! Las opciones viven en un singleton con atómicos: la UI las escribe desde sus
//! callbacks y los hilos de audio en tiempo real las leen en cada trama sin
//! bloquear. Cambiar de dispositivo incrementa `device_generation`; los hilos
//! de audio lo detectan y reconstruyen sus streams sin cortar la llamada.

use anyhow::{anyhow, bail, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

pub const SAMPLE_RATE: u32 = 48_000;
pub const CHANNELS: usize = 2;
/// Muestras por canal en un frame de 20 ms a 48 kHz.
pub const FRAME_SAMPLES: usize = 960;
/// Muestras interleaved (estéreo) por frame.
pub const FRAME_LEN: usize = FRAME_SAMPLES * CHANNELS;

// --- Opciones en vivo --------------------------------------------------------

/// Ajustes de voz aplicados en caliente. La UI escribe; el audio lee por trama.
pub struct AudioOptions {
    /// Volumen de entrada en % (0–200; 100 = sin cambio), como en Discord.
    input_volume: AtomicU32,
    /// Volumen de salida en % (0–200).
    output_volume: AtomicU32,
    /// Cancelación de eco (ducking del micro mientras suena la voz remota).
    pub echo_suppress: AtomicBool,
    /// Supresión de ruido (puerta de ruido adaptativa).
    pub noise_suppress: AtomicBool,
    /// Control automático de ganancia (sube los micros que se oyen bajos).
    pub agc: AtomicBool,
    /// Sensibilidad de entrada automática (la puerta aprende el piso de ruido).
    pub auto_sensitivity: AtomicBool,
    /// Umbral manual de sensibilidad en dBFS (-100..0), si no es automática.
    sensitivity_db: AtomicI32,
    /// Nivel del micrófono YA procesado (0–100) para el medidor de la UI.
    mic_level: AtomicU32,
    /// Envolvente RMS de lo que se está reproduciendo (bits de f32); alimenta
    /// el anti-eco: si suena la voz de otros, el micro se atenúa.
    out_env: AtomicU32,
    /// Nombre del dispositivo elegido (`None` = predeterminado del sistema).
    input_device: Mutex<Option<String>>,
    output_device: Mutex<Option<String>>,
    /// Se incrementa al cambiar de dispositivo; los hilos de audio reabren streams.
    device_generation: AtomicU64,
}

impl AudioOptions {
    fn new() -> Self {
        Self {
            input_volume: AtomicU32::new(100),
            output_volume: AtomicU32::new(100),
            echo_suppress: AtomicBool::new(true),
            noise_suppress: AtomicBool::new(true),
            agc: AtomicBool::new(true),
            auto_sensitivity: AtomicBool::new(true),
            sensitivity_db: AtomicI32::new(-60),
            mic_level: AtomicU32::new(0),
            out_env: AtomicU32::new(0f32.to_bits()),
            input_device: Mutex::new(None),
            output_device: Mutex::new(None),
            device_generation: AtomicU64::new(0),
        }
    }

    pub fn input_gain(&self) -> f32 {
        self.input_volume.load(Ordering::Relaxed) as f32 / 100.0
    }
    pub fn set_input_volume(&self, pct: u32) {
        self.input_volume.store(pct.min(200), Ordering::Relaxed);
    }
    pub fn output_gain(&self) -> f32 {
        self.output_volume.load(Ordering::Relaxed) as f32 / 100.0
    }
    pub fn set_output_volume(&self, pct: u32) {
        self.output_volume.store(pct.min(200), Ordering::Relaxed);
    }

    pub fn sensitivity_db(&self) -> f32 {
        self.sensitivity_db.load(Ordering::Relaxed) as f32
    }
    pub fn set_sensitivity_db(&self, db: i32) {
        self.sensitivity_db.store(db.clamp(-100, 0), Ordering::Relaxed);
    }

    pub fn mic_level(&self) -> u32 {
        self.mic_level.load(Ordering::Relaxed)
    }
    pub fn set_mic_level(&self, pct: u32) {
        self.mic_level.store(pct.min(100), Ordering::Relaxed);
    }

    pub fn out_env(&self) -> f32 {
        f32::from_bits(self.out_env.load(Ordering::Relaxed))
    }
    /// Actualiza la envolvente (suavizada) de lo que se reproduce. Se suaviza en
    /// AMBOS sentidos para que el anti-eco vea un nivel estable y no module el
    /// micro trama a trama (esa modulación es lo que «robotiza» la voz).
    pub fn update_out_env(&self, rms: f32) {
        let prev = self.out_env();
        let env = if rms > prev {
            prev * 0.7 + rms * 0.3
        } else {
            prev * 0.95 + rms * 0.05
        };
        self.out_env.store(env.to_bits(), Ordering::Relaxed);
    }

    pub fn input_device(&self) -> Option<String> {
        self.input_device.lock().unwrap().clone()
    }
    pub fn set_input_device(&self, name: Option<String>) {
        *self.input_device.lock().unwrap() = name;
        self.device_generation.fetch_add(1, Ordering::Relaxed);
    }
    pub fn output_device(&self) -> Option<String> {
        self.output_device.lock().unwrap().clone()
    }
    pub fn set_output_device(&self, name: Option<String>) {
        *self.output_device.lock().unwrap() = name;
        self.device_generation.fetch_add(1, Ordering::Relaxed);
    }
    pub fn generation(&self) -> u64 {
        self.device_generation.load(Ordering::Relaxed)
    }
}

/// Singleton global de opciones de audio.
pub fn options() -> &'static AudioOptions {
    static OPTS: OnceLock<AudioOptions> = OnceLock::new();
    OPTS.get_or_init(AudioOptions::new)
}

/// Vuelca la config persistida a las opciones en vivo (al arrancar la app).
pub fn apply_settings(v: &crate::config::VoiceSettings) {
    let o = options();
    o.set_input_volume(v.input_volume);
    o.set_output_volume(v.output_volume);
    o.echo_suppress.store(v.echo_suppression, Ordering::Relaxed);
    o.noise_suppress.store(v.noise_suppression, Ordering::Relaxed);
    o.agc.store(v.auto_gain, Ordering::Relaxed);
    o.auto_sensitivity.store(v.auto_sensitivity, Ordering::Relaxed);
    o.set_sensitivity_db(v.sensitivity_db);
    o.set_input_device(v.input_device.clone());
    o.set_output_device(v.output_device.clone());
}

// --- Utilidades PCM ----------------------------------------------------------

/// Energía RMS de una trama PCM i16.
pub fn rms(pcm: &[i16]) -> f32 {
    if pcm.is_empty() {
        return 0.0;
    }
    let sum: f64 = pcm.iter().map(|&s| (s as f64) * (s as f64)).sum();
    ((sum / pcm.len() as f64) as f32).sqrt()
}

/// Aplica una ganancia con saturación (sin wrap-around al pasarse de i16).
pub fn apply_gain(pcm: &mut [i16], gain: f32) {
    for s in pcm.iter_mut() {
        *s = ((*s as f32) * gain).clamp(-32768.0, 32767.0) as i16;
    }
}

fn db_to_rms(db: f32) -> f32 {
    32767.0 * 10f32.powf(db / 20.0)
}

/// Mapea un RMS a 0–100 para el medidor (escala dB, -50 dBFS..0 dBFS).
fn rms_to_pct(rms: f32) -> u32 {
    if rms < 1.0 {
        return 0;
    }
    let db = 20.0 * (rms / 32767.0).log10();
    (((db + 50.0) / 50.0).clamp(0.0, 1.0) * 100.0) as u32
}

/// Acota un buffer a `max` elementos descartando los más antiguos.
pub fn cap(b: &mut VecDeque<i16>, max: usize) {
    while b.len() > max {
        b.pop_front();
    }
}

// --- Procesamiento de voz (TX) ----------------------------------------------

/// Puerta de ruido para suprimir el hiss constante del micrófono cuando no se
/// habla. En modo automático estima el piso de ruido de las tramas silenciosas
/// y abre solo cuando el RMS lo supera por margen; en manual usa el umbral del
/// slider de sensibilidad. Con histéresis y envolvente (ataque rápido,
/// liberación lenta) para no producir clics ni cortar el final de las palabras.
pub struct NoiseGate {
    gain: f32,        // ganancia actual aplicada (0..1), suavizada
    hold: u32,        // tramas restantes manteniendo la puerta abierta
    noise_floor: f64, // estimación del piso de ruido (RMS) aprendida
}

impl NoiseGate {
    pub fn new() -> Self {
        Self { gain: 0.0, hold: 0, noise_floor: 200.0 }
    }

    /// Procesa una trama PCM in situ, atenuándola si se considera ruido de fondo.
    pub fn process(&mut self, pcm: &mut [i16]) {
        // ~0.5 s de "mantener abierto" tras detectar voz (a 50 tramas/s).
        const HOLD_FRAMES: u32 = 25;

        let rms = rms(pcm) as f64;
        let (open, close) = if options().auto_sensitivity.load(Ordering::Relaxed) {
            // Umbrales relativos al piso aprendido, con suelo mínimo absoluto.
            ((self.noise_floor * 4.0).max(300.0), (self.noise_floor * 2.5).max(180.0))
        } else {
            let t = db_to_rms(options().sensitivity_db()) as f64;
            (t.max(1.0), (t * 0.7).max(1.0))
        };
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
            apply_gain(pcm, self.gain);
        }
    }
}

/// Control automático de ganancia: acerca el nivel de la voz a un objetivo
/// (~ -15 dBFS) con cambios suaves. Solo se adapta cuando hay señal apreciable
/// para no amplificar el ruido de fondo. Arregla los micros que «se oyen bajo».
pub struct Agc {
    gain: f32,
}

impl Agc {
    pub fn new() -> Self {
        Self { gain: 1.0 }
    }

    pub fn process(&mut self, pcm: &mut [i16]) {
        const TARGET_RMS: f32 = 4500.0; // ~ -17 dBFS
        let r = rms(pcm);
        // Adaptación LENTA (2 %/trama ≈ 1 s de constante de tiempo) y solo con
        // voz clara: si la ganancia cambia rápido entre tramas, modula la voz y
        // suena robótica. Tope 4× para no amplificar hasta saturar/clipping.
        if r > 700.0 {
            let desired = (TARGET_RMS / r).clamp(0.3, 4.0);
            self.gain += (desired - self.gain) * 0.02;
        }
        if (self.gain - 1.0).abs() > 0.01 {
            apply_gain(pcm, self.gain);
        }
    }
}

/// Cancelación de eco por atenuación (ducking): mientras suena la voz de otros
/// por los altavoces, atenúa el micrófono salvo que el usuario esté hablando por
/// encima del eco esperado. Aprende el acople acústico altavoz→micro (ratio RMS)
/// para no atenuar durante el doble-habla.
///
/// CLAVE para no «robotizar»: el factor de atenuación se mueve MUY despacio entre
/// tramas (constantes de tiempo de ~100–300 ms) y la atenuación mínima es modesta
/// (-9 dB). Una atenuación fuerte que conmuta trama a trama modula la amplitud de
/// la voz y produce el efecto robótico/entrecortado; aquí se evita a propósito.
pub struct EchoDucker {
    coupling: f32, // eco_en_micro ≈ coupling · rms_reproducido (aprendido)
    duck: f32,     // atenuación actual aplicada (suavizada)
}

impl EchoDucker {
    pub fn new() -> Self {
        Self { coupling: 0.5, duck: 1.0 }
    }

    /// `far_rms` debe ser la envolvente YA suavizada de la reproducción
    /// (`options().out_env()`), no el RMS instantáneo.
    pub fn process(&mut self, pcm: &mut [i16], mic_rms: f32, far_rms: f32) {
        let far_active = far_rms > 250.0;
        if far_active {
            let ratio = (mic_rms / far_rms.max(1.0)).min(4.0);
            // Aprende el acople despacio (persigue el mínimo para quedarse con el
            // eco, no con la voz del usuario superpuesta).
            let a = if ratio < self.coupling { 0.05 } else { 0.003 };
            self.coupling += (ratio - self.coupling) * a;
        }
        let expected_echo = self.coupling * far_rms;
        let speaking = mic_rms > (expected_echo * 2.0).max(800.0);
        // Atenuación máxima moderada (0.4 ≈ -8 dB) y SIEMPRE suavizada despacio,
        // para reducir el eco sin recortar la voz a trozos.
        let target = if far_active && !speaking { 0.4 } else { 1.0 };
        let coeff = if target < self.duck { 0.08 } else { 0.04 };
        self.duck += (target - self.duck) * coeff;
        if self.duck < 0.999 {
            apply_gain(pcm, self.duck);
        }
    }
}

/// Cadena completa de procesamiento del micrófono, en el orden:
/// volumen de entrada → AGC → anti-eco → puerta de ruido. Publica el nivel
/// resultante para el medidor de la UI. Cada paso respeta su toggle en vivo.
pub struct VoiceProcessor {
    gate: NoiseGate,
    agc: Agc,
    ducker: EchoDucker,
}

impl VoiceProcessor {
    pub fn new() -> Self {
        Self { gate: NoiseGate::new(), agc: Agc::new(), ducker: EchoDucker::new() }
    }

    pub fn process(&mut self, pcm: &mut [i16]) {
        let o = options();
        let g = o.input_gain();
        if (g - 1.0).abs() > 0.001 {
            apply_gain(pcm, g);
        }
        if o.agc.load(Ordering::Relaxed) {
            self.agc.process(pcm);
        }
        if o.echo_suppress.load(Ordering::Relaxed) {
            let r = rms(pcm);
            self.ducker.process(pcm, r, o.out_env());
        }
        if o.noise_suppress.load(Ordering::Relaxed) {
            self.gate.process(pcm);
        }
        o.set_mic_level(rms_to_pct(rms(pcm)));
    }
}

// --- Dispositivos y streams ---------------------------------------------------

/// Nombres de los dispositivos de entrada disponibles (para el desplegable).
pub fn input_device_names() -> Vec<String> {
    let host = cpal::default_host();
    host.input_devices()
        .map(|it| it.filter_map(|d| d.name().ok()).collect())
        .unwrap_or_default()
}

/// Nombres de los dispositivos de salida disponibles.
pub fn output_device_names() -> Vec<String> {
    let host = cpal::default_host();
    host.output_devices()
        .map(|it| it.filter_map(|d| d.name().ok()).collect())
        .unwrap_or_default()
}

/// Dispositivo de entrada según los Ajustes de voz (o el predeterminado).
pub fn find_input_device() -> Result<cpal::Device> {
    let host = cpal::default_host();
    if let Some(want) = options().input_device() {
        if let Ok(mut devs) = host.input_devices() {
            if let Some(d) = devs.find(|d| d.name().map(|n| n == want).unwrap_or(false)) {
                return Ok(d);
            }
        }
        tracing::warn!("audio: micrófono '{want}' no disponible; usando el predeterminado");
    }
    host.default_input_device()
        .ok_or_else(|| anyhow!("sin micrófono por defecto"))
}

/// Dispositivo de salida según los Ajustes de voz (o el predeterminado).
pub fn find_output_device() -> Result<cpal::Device> {
    let host = cpal::default_host();
    if let Some(want) = options().output_device() {
        if let Ok(mut devs) = host.output_devices() {
            if let Some(d) = devs.find(|d| d.name().map(|n| n == want).unwrap_or(false)) {
                return Ok(d);
            }
        }
        tracing::warn!("audio: salida '{want}' no disponible; usando la predeterminada");
    }
    host.default_output_device()
        .ok_or_else(|| anyhow!("sin altavoz por defecto"))
}

/// Config del dispositivo a usar. Prioriza la **config predeterminada** del
/// dispositivo (la del modo compartido de WASAPI), que es la ruta probada que
/// funciona: forzar un formato distinto del de la mezcla compartida puede dar
/// audio entrecortado/robótico. Solo si el predeterminado NO es 48 kHz se busca
/// un rango que sí lo soporte con el MISMO nº de canales (para evitar el audio a
/// tono incorrecto cuando no hay remuestreo).
fn config_48k(dev: &cpal::Device, input: bool) -> Result<cpal::SupportedStreamConfig> {
    let default = if input {
        dev.default_input_config()?
    } else {
        dev.default_output_config()?
    };
    if default.sample_rate().0 == SAMPLE_RATE {
        return Ok(default); // ruta conocida-buena: no tocar el formato compartido
    }
    let want_ch = default.channels();
    let pick = |ranges: Vec<cpal::SupportedStreamConfigRange>, ch_exact: bool| {
        ranges.into_iter().find(|r| {
            r.min_sample_rate().0 <= SAMPLE_RATE
                && r.max_sample_rate().0 >= SAMPLE_RATE
                && (!ch_exact || r.channels() == want_ch)
                && matches!(
                    r.sample_format(),
                    cpal::SampleFormat::F32 | cpal::SampleFormat::I16
                )
        })
    };
    let ranges: Vec<_> = if input {
        dev.supported_input_configs().map(|i| i.collect()).unwrap_or_default()
    } else {
        dev.supported_output_configs().map(|i| i.collect()).unwrap_or_default()
    };
    // Primero un rango de 48 kHz con el mismo nº de canales que el predeterminado;
    // si no, cualquiera a 48 kHz; si tampoco, el predeterminado (con aviso).
    if let Some(r) = pick(ranges.clone(), true).or_else(|| pick(ranges, false)) {
        return Ok(r.with_sample_rate(cpal::SampleRate(SAMPLE_RATE)));
    }
    Ok(default)
}

/// Abre el stream de captura del micrófono elegido → `buf` (estéreo i16 48 kHz).
pub fn build_input_stream(buf: Arc<Mutex<VecDeque<i16>>>) -> Result<cpal::Stream> {
    let dev = find_input_device()?;
    let cfg = config_48k(&dev, true)?;
    let ch = cfg.channels() as usize;
    tracing::info!(
        "audio in: '{}' {} Hz, {} canales, {:?}",
        dev.name().unwrap_or_else(|_| "?".into()),
        cfg.sample_rate().0,
        ch,
        cfg.sample_format()
    );
    if cfg.sample_rate().0 != SAMPLE_RATE {
        tracing::warn!(
            "⚠️ el micrófono no soporta 48 kHz (usa {}) y NO hay remuestreo: \
             la voz sonará a velocidad/tono incorrecto",
            cfg.sample_rate().0
        );
    }
    let err_fn = |e| tracing::warn!("error stream entrada: {e}");
    let stream = match cfg.sample_format() {
        cpal::SampleFormat::F32 => dev.build_input_stream(
            &cfg.config(),
            move |data: &[f32], _: &_| {
                let mut b = buf.lock().unwrap();
                push_input_i16(&mut b, data, ch, |s| (s.clamp(-1.0, 1.0) * 32767.0) as i16);
                cap(&mut b, SAMPLE_RATE as usize * CHANNELS); // ~1 s máx
            },
            err_fn,
            None,
        )?,
        cpal::SampleFormat::I16 => dev.build_input_stream(
            &cfg.config(),
            move |data: &[i16], _: &_| {
                let mut b = buf.lock().unwrap();
                push_input_i16(&mut b, data, ch, |s| s);
                cap(&mut b, SAMPLE_RATE as usize * CHANNELS);
            },
            err_fn,
            None,
        )?,
        other => bail!("formato de entrada no soportado: {other:?}"),
    };
    Ok(stream)
}

/// Abre el stream de reproducción hacia la salida elegida ← `buf`. Aplica el
/// volumen de salida en vivo; si `track_env`, alimenta la envolvente del
/// anti-eco (solo la llamada real, NO la prueba de micrófono).
pub fn build_output_stream(
    buf: Arc<Mutex<VecDeque<i16>>>,
    deaf: Option<Arc<AtomicBool>>,
    track_env: bool,
) -> Result<cpal::Stream> {
    let dev = find_output_device()?;
    let cfg = config_48k(&dev, false)?;
    let ch = cfg.channels() as usize;
    tracing::info!(
        "audio out: '{}' {} Hz, {} canales, {:?}",
        dev.name().unwrap_or_else(|_| "?".into()),
        cfg.sample_rate().0,
        ch,
        cfg.sample_format()
    );
    let err_fn = |e| tracing::warn!("error stream salida: {e}");
    let stream = match cfg.sample_format() {
        cpal::SampleFormat::F32 => {
            let deaf = deaf.clone();
            dev.build_output_stream(
                &cfg.config(),
                move |data: &mut [f32], _: &_| {
                    let silent = deaf.as_ref().map(|d| d.load(Ordering::Relaxed)).unwrap_or(false);
                    let gain = options().output_gain();
                    let mut b = buf.lock().unwrap();
                    fill_output(data, &mut b, ch, silent, gain, track_env, |s| s as f32 / 32767.0);
                },
                err_fn,
                None,
            )?
        }
        cpal::SampleFormat::I16 => {
            let deaf = deaf.clone();
            dev.build_output_stream(
                &cfg.config(),
                move |data: &mut [i16], _: &_| {
                    let silent = deaf.as_ref().map(|d| d.load(Ordering::Relaxed)).unwrap_or(false);
                    let gain = options().output_gain();
                    let mut b = buf.lock().unwrap();
                    fill_output(data, &mut b, ch, silent, gain, track_env, |s| s);
                },
                err_fn,
                None,
            )?
        }
        other => bail!("formato de salida no soportado: {other:?}"),
    };
    Ok(stream)
}

/// Inserta muestras del micrófono normalizadas a estéreo i16.
pub fn push_input_i16<T: Copy>(
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

/// Rellena el buffer de salida desde el de reproducción (estéreo i16 fuente),
/// aplicando el volumen de salida y midiendo la envolvente para el anti-eco.
pub fn fill_output<T: Copy + Default>(
    data: &mut [T],
    src: &mut VecDeque<i16>,
    out_ch: usize,
    silent: bool,
    gain: f32,
    track_env: bool,
    conv: impl Fn(i16) -> T,
) {
    if out_ch == 0 {
        return;
    }
    let scale = (gain - 1.0).abs() > 0.001;
    let mut sumsq = 0.0f64;
    let mut count = 0usize;
    for frame in data.chunks_mut(out_ch) {
        let (mut l, mut r) = if silent {
            (0i16, 0i16)
        } else {
            (src.pop_front().unwrap_or(0), src.pop_front().unwrap_or(0))
        };
        if scale {
            l = ((l as f32) * gain).clamp(-32768.0, 32767.0) as i16;
            r = ((r as f32) * gain).clamp(-32768.0, 32767.0) as i16;
        }
        sumsq += (l as f64) * (l as f64) + (r as f64) * (r as f64);
        count += 2;
        for (i, slot) in frame.iter_mut().enumerate() {
            *slot = match i {
                0 => conv(l),
                1 => conv(r),
                _ => T::default(),
            };
        }
    }
    if track_env && count > 0 {
        options().update_out_env(((sumsq / count as f64) as f32).sqrt());
    }
}

// --- Prueba de micrófono ("Probemos el micrófono") ---------------------------

/// Prueba en marcha: captura → procesa (misma cadena que la llamada) →
/// reproduce, y publica el nivel en `mic_level` para el medidor. Se detiene al
/// soltar el handle (Drop) o llamando a `stop()`.
pub struct MicTest {
    stop: Arc<AtomicBool>,
}

impl MicTest {
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

impl Drop for MicTest {
    fn drop(&mut self) {
        self.stop();
    }
}

pub fn start_mic_test() -> MicTest {
    let stop = Arc::new(AtomicBool::new(false));
    let s2 = stop.clone();
    std::thread::spawn(move || {
        if let Err(e) = mic_test_thread(s2) {
            tracing::warn!("prueba de micrófono: {e}");
        }
        options().set_mic_level(0);
    });
    MicTest { stop }
}

fn mic_test_thread(stop: Arc<AtomicBool>) -> Result<()> {
    let in_buf: Arc<Mutex<VecDeque<i16>>> = Arc::new(Mutex::new(VecDeque::new()));
    let play_buf: Arc<Mutex<VecDeque<i16>>> = Arc::new(Mutex::new(VecDeque::new()));
    let mut chain = VoiceProcessor::new();
    let mut pcm = vec![0i16; FRAME_LEN];

    'rebuild: loop {
        // La prueba NO alimenta la envolvente anti-eco (track_env=false): si no,
        // el propio loopback haría que el ducking se atenuara a sí mismo.
        let in_stream = build_input_stream(in_buf.clone())?;
        let out_stream = build_output_stream(play_buf.clone(), None, false)?;
        in_stream.play()?;
        out_stream.play()?;
        let gen = options().generation();
        tracing::info!("prueba de micrófono iniciada");
        loop {
            if stop.load(Ordering::Relaxed) {
                break 'rebuild;
            }
            if options().generation() != gen {
                continue 'rebuild; // cambió el dispositivo: reabrir streams
            }
            let ready = { in_buf.lock().unwrap().len() >= FRAME_LEN };
            if !ready {
                std::thread::sleep(std::time::Duration::from_millis(5));
                continue;
            }
            {
                let mut b = in_buf.lock().unwrap();
                for s in pcm.iter_mut() {
                    *s = b.pop_front().unwrap_or(0);
                }
            }
            chain.process(&mut pcm);
            let mut b = play_buf.lock().unwrap();
            for &s in pcm.iter() {
                b.push_back(s);
            }
            cap(&mut b, SAMPLE_RATE as usize * CHANNELS);
        }
    }
    tracing::info!("prueba de micrófono detenida");
    Ok(())
}
