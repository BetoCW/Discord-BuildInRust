//! Audio compartido entre la UI y el subsistema de voz: opciones EN VIVO
//! (volúmenes, dispositivos, procesado), cadena de procesamiento del micrófono
//! (ganancia → AGC → anti-eco → puerta de ruido) y la prueba de micrófono de
//! «Ajustes de voz» (estilo Discord: hablas y te lo reproducimos).
//!
//! Las opciones viven en un singleton con atómicos: la UI las escribe desde sus
//! callbacks y los hilos de audio en tiempo real las leen en cada trama sin
//! bloquear. Cambiar de dispositivo incrementa `device_generation`; los hilos
//! de audio lo detectan y reconstruyen sus streams sin cortar la llamada.

use crate::config::NoiseMode;
use anyhow::{anyhow, bail, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use nnnoiseless::DenoiseState;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, AtomicU64, AtomicU8, Ordering};
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
    /// Modo de supresión de ruido (0=Off, 1=Ligero/puerta, 2=Aislamiento/RNNoise).
    /// Privado: se accede con `noise_mode()`/`set_noise_mode()`.
    noise_mode: AtomicU8,
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
    /// Cancelación de eco avanzada (AEC NLMS, Meta 5). EXPERIMENTAL/opt-in.
    aec_enabled: AtomicBool,
    /// Referencia far-end (mono, 48 kHz) para el AEC: lo que se reproduce por el
    /// altavoz. La ruta de reproducción REAL la alimenta; el TX la consume en
    /// lockstep con el micro. Solo se acumula si el AEC está activo.
    far_ref: Mutex<VecDeque<f32>>,
}

impl AudioOptions {
    fn new() -> Self {
        Self {
            input_volume: AtomicU32::new(100),
            output_volume: AtomicU32::new(100),
            echo_suppress: AtomicBool::new(true),
            noise_mode: AtomicU8::new(NoiseMode::VoiceIsolation.as_u8()),
            agc: AtomicBool::new(true),
            auto_sensitivity: AtomicBool::new(true),
            sensitivity_db: AtomicI32::new(-60),
            mic_level: AtomicU32::new(0),
            out_env: AtomicU32::new(0f32.to_bits()),
            input_device: Mutex::new(None),
            output_device: Mutex::new(None),
            device_generation: AtomicU64::new(0),
            aec_enabled: AtomicBool::new(false),
            far_ref: Mutex::new(VecDeque::new()),
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

    pub fn noise_mode(&self) -> NoiseMode {
        NoiseMode::from_u8(self.noise_mode.load(Ordering::Relaxed))
    }
    pub fn set_noise_mode(&self, m: NoiseMode) {
        self.noise_mode.store(m.as_u8(), Ordering::Relaxed);
    }

    pub fn aec_enabled(&self) -> bool {
        self.aec_enabled.load(Ordering::Relaxed)
    }
    pub fn set_aec(&self, on: bool) {
        self.aec_enabled.store(on, Ordering::Relaxed);
        if !on {
            self.far_ref.lock().unwrap().clear(); // no acumular si está apagado
        }
    }
    /// La ruta de reproducción real empuja aquí el far-end (mono 48 kHz). Solo
    /// acumula con el AEC activo; acota a ~500 ms para no crecer sin límite.
    pub fn push_far_ref(&self, mono: &[f32]) {
        if !self.aec_enabled.load(Ordering::Relaxed) {
            return;
        }
        let mut q = self.far_ref.lock().unwrap();
        q.extend(mono.iter().copied());
        cap_f32(&mut q, SAMPLE_RATE as usize / 2);
    }
    /// El TX toma `n` muestras de referencia alineadas con la trama del micro
    /// (rellena con silencio si aún no hay suficientes).
    pub fn take_far_ref(&self, n: usize) -> Vec<f32> {
        let mut q = self.far_ref.lock().unwrap();
        (0..n).map(|_| q.pop_front().unwrap_or(0.0)).collect()
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
    o.set_noise_mode(v.noise_mode);
    o.agc.store(v.auto_gain, Ordering::Relaxed);
    o.set_aec(v.aec);
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

/// Igual que `cap` para la referencia far-end del AEC (muestras f32).
fn cap_f32(b: &mut VecDeque<f32>, max: usize) {
    while b.len() > max {
        b.pop_front();
    }
}

// --- Remuestreo (Meta 3) -----------------------------------------------------

/// Interpolación cúbica de 4 puntos (Catmull-Rom) entre `x0` y `x1` en `t∈[0,1)`,
/// usando los vecinos `xm1`/`x2`. Mejor que la lineal para voz (menos aliasing).
fn hermite4(xm1: f32, x0: f32, x1: f32, x2: f32, t: f32) -> f32 {
    let c1 = 0.5 * (x1 - xm1);
    let c2 = xm1 - 2.5 * x0 + 2.0 * x1 - 0.5 * x2;
    let c3 = 0.5 * (x2 - xm1) + 1.5 * (x0 - x1);
    ((c3 * t + c2) * t + c1) * t + x0
}

/// Remuestreador estéreo en streaming (cúbico). Convierte entre la frecuencia
/// nativa de un dispositivo y los 48 kHz del pipeline interno cuando el
/// dispositivo NO ofrece 48 kHz en modo compartido (antes la voz salía a tono
/// incorrecto). Es puro Rust, sin dependencias, y procesa chunks de tamaño
/// arbitrario (los callbacks de cpal varían): mantiene 3 muestras de historia por
/// canal y una posición fraccionaria entre llamadas. Con `step==1.0` es un
/// passthrough EXACTO, pero solo se instancia cuando las frecuencias difieren.
pub struct Resampler {
    step: f64,        // muestras de ENTRADA consumidas por muestra de SALIDA
    pos: f64,         // posición de lectura (coords del array [historia(3) ++ entrada])
    hist_l: [f32; 3], // últimas 3 muestras de entrada (canal izq) de la llamada previa
    hist_r: [f32; 3],
}

impl Resampler {
    pub fn new(src_rate: u32, dst_rate: u32) -> Self {
        Self {
            step: src_rate as f64 / dst_rate as f64,
            pos: 3.0, // arranca en la primera muestra real (índices 0..3 = historia)
            hist_l: [0.0; 3],
            hist_r: [0.0; 3],
        }
    }

    /// Lee la muestra del array virtual `historia(3) ++ entrada` (estéreo i16
    /// intercalado) en el índice `idx`, canal `ch` (0=izq, 1=der).
    #[inline]
    fn at(&self, input: &[i16], idx: usize, ch: usize) -> f32 {
        if idx < 3 {
            if ch == 0 { self.hist_l[idx] } else { self.hist_r[idx] }
        } else {
            input[(idx - 3) * CHANNELS + ch] as f32
        }
    }

    /// Procesa un chunk de entrada estéreo intercalado (a la frecuencia nativa) y
    /// añade a `out` las muestras estéreo i16 resultantes (a la de destino).
    pub fn process(&mut self, input: &[i16], out: &mut VecDeque<i16>) {
        let n = input.len() / CHANNELS;
        if n == 0 {
            return;
        }
        let last = 3 + n - 1; // último índice válido del array virtual
        loop {
            let i = self.pos.floor();
            let ii = i as usize;
            // Necesitamos los vecinos ii-1 .. ii+2 dentro de rango.
            if ii < 1 || ii + 2 > last {
                break;
            }
            let t = (self.pos - i) as f32;
            for ch in 0..CHANNELS {
                let y = hermite4(
                    self.at(input, ii - 1, ch),
                    self.at(input, ii, ch),
                    self.at(input, ii + 1, ch),
                    self.at(input, ii + 2, ch),
                    t,
                );
                out.push_back(y.clamp(-32768.0, 32767.0) as i16);
            }
            self.pos += self.step;
        }
        // Nueva historia = las últimas 3 muestras del array virtual; desplaza el
        // marco de coordenadas en `n` (las muestras de entrada ya consumidas).
        let (mut nl, mut nr) = ([0.0f32; 3], [0.0f32; 3]);
        for k in 0..3 {
            nl[k] = self.at(input, n + k, 0);
            nr[k] = self.at(input, n + k, 1);
        }
        self.hist_l = nl;
        self.hist_r = nr;
        self.pos -= n as f64;
    }
}

// --- Procesamiento de voz (TX) ----------------------------------------------

/// Puerta de ruido para suprimir el hiss constante del micrófono cuando no se
/// habla. En modo automático estima el piso de ruido de las tramas silenciosas
/// y abre solo cuando el RMS lo supera por margen; en manual usa el umbral del
/// slider de sensibilidad. Con histéresis y envolvente (ataque rápido,
/// liberación lenta) para no producir clics ni cortar el final de las palabras.
///
/// Es el modo **Ligero** del selector de supresión de ruido (sin red neuronal):
/// más barato en CPU que `Denoiser` (RNNoise), útil en equipos justos de recursos.
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

/// Supresión de eco por atenuación (ducking): mientras suena la voz de otros por
/// los altavoces, atenúa FUERTE el micrófono salvo que el usuario esté hablando
/// por encima del eco esperado. Aprende el acople acústico altavoz→micro (ratio
/// RMS) para distinguir el eco de la voz real (doble-habla).
///
/// No es una cancelación adaptativa real (AEC tipo WebRTC/Krisp, que resta la
/// señal de referencia muestra a muestra); es supresión: cuando el micro solo
/// contiene eco lo baja ~-26 dB para que NO se reenvíe a la sala. La supresión de
/// ruido por red neuronal posterior (RNNoise) limpia el residuo y suaviza la
/// transición, evitando el efecto «robótico» que tenía el ducking suave anterior.
///
/// Ataque rápido (corta el eco en ~2-3 tramas) y liberación más lenta (no recorta
/// el final de las palabras del usuario). `coupling` arranca bajo para no atenuar
/// a quien usa auriculares (sin eco real) y sube solo si detecta acople verdadero.
pub struct EchoDucker {
    coupling: f32,   // eco_en_micro ≈ coupling · rms_reproducido (aprendido)
    duck: f32,       // atenuación actual aplicada (suavizada)
    speak_hold: u32, // tramas restantes considerando "el usuario habla" (anti-flicker)
}

impl EchoDucker {
    /// Atenuación máxima del eco (≈ -26 dB). Suficiente para que el eco recaptado
    /// no sea audible al reenviarse, sin llegar al mute total (que cortaría al
    /// usuario si la detección de doble-habla falla un instante).
    const DUCK_FLOOR: f32 = 0.05;
    /// Hangover de voz: tras detectar que el usuario habla, se le considera
    /// hablando ~300 ms más (15 tramas a 50/s). CLAVE contra la «robotización»:
    /// evita que la atenuación entre a mitad de palabra si el RMS baja un instante
    /// (la regresión de v0.2.0 era el duck parpadeando DURANTE la voz del usuario).
    const SPEAK_HOLD: u32 = 15;

    pub fn new() -> Self {
        // `coupling` bajo de inicio: con auriculares no hay eco y así no se atenúa
        // la voz del usuario; sube solo si aparece acople acústico real.
        Self { coupling: 0.15, duck: 1.0, speak_hold: 0 }
    }

    /// `far_rms` debe ser la envolvente YA suavizada de la reproducción
    /// (`options().out_env()`), no el RMS instantáneo.
    pub fn process(&mut self, pcm: &mut [i16], mic_rms: f32, far_rms: f32) {
        let far_active = far_rms > 250.0;
        if far_active {
            let ratio = (mic_rms / far_rms.max(1.0)).min(4.0);
            // Aprende el acople persiguiendo el MÍNIMO (baja rápido, sube despacio):
            // así se queda con el nivel del eco puro, no con la voz superpuesta.
            let a = if ratio < self.coupling { 0.05 } else { 0.003 };
            self.coupling += (ratio - self.coupling) * a;
        }
        let expected_echo = self.coupling * far_rms;
        // El usuario habla si su nivel supera con margen el eco esperado (factor
        // 1.6) o un suelo absoluto (voz normal). Con hangover para no parpadear.
        if mic_rms > (expected_echo * 1.6).max(800.0) {
            self.speak_hold = Self::SPEAK_HOLD;
        } else if self.speak_hold > 0 {
            self.speak_hold -= 1;
        }
        // Solo se atenúa cuando suena la voz remota Y el usuario NO habla: es eco
        // puro, sin voz propia que modular → atenuar fuerte aquí no robotiza nada.
        let echo_only = far_active && self.speak_hold == 0;
        let target = if echo_only { Self::DUCK_FLOOR } else { 1.0 };
        // Ataque moderado (baja en ~200 ms) / liberación suave: la rampa cae sobre
        // eco (no sobre voz), así que no introduce modulación audible de la voz.
        let coeff = if target < self.duck { 0.3 } else { 0.08 };
        self.duck += (target - self.duck) * coeff;
        if self.duck < 0.999 {
            apply_gain(pcm, self.duck);
        }
    }
}

/// Supresión de ruido / «aislamiento de voz» por red neuronal (RNNoise, port en
/// Rust puro `nnnoiseless`). Es el equivalente abierto a la tecnología de Krisp
/// que usa Discord: separa la voz del ruido de fondo (teclado, ventilador, calle)
/// mucho mejor que una puerta de ruido por umbral, y sin cortar la voz.
///
/// RNNoise trabaja en tramas MONO de 480 muestras (10 ms) a 48 kHz. La voz es
/// mono (el micro se duplica a ambos canales en captura), así que **mezclamos a
/// mono**, procesamos con UN solo `DenoiseState` y escribimos el resultado a
/// ambos canales. Antes se corría una red por canal (doble CPU) sobre una señal
/// idéntica: a la mitad de coste, ideal para equipos justos de recursos. Devuelve
/// además una probabilidad de voz (VAD) por trama, útil como medidor/indicador.
pub struct Denoiser {
    state: Box<DenoiseState<'static>>,
    last_vad: f32,
}

impl Denoiser {
    pub fn new() -> Self {
        Self { state: DenoiseState::new(), last_vad: 0.0 }
    }

    /// Probabilidad de voz (0..1) estimada en la última trama procesada.
    #[allow(dead_code)]
    pub fn vad(&self) -> f32 {
        self.last_vad
    }

    /// Procesa una trama PCM estéreo intercalada in situ. La longitud debe ser
    /// múltiplo de `FRAME_SIZE * CHANNELS`; cualquier resto se deja sin tocar.
    pub fn process(&mut self, pcm: &mut [i16]) {
        const N: usize = DenoiseState::FRAME_SIZE; // 480 (10 ms a 48 kHz)
        let mut inb = [0f32; N];
        let mut outb = [0f32; N];
        let per_ch = pcm.len() / CHANNELS;
        let mut vad = 0.0f32;
        let mut off = 0;
        while off + N <= per_ch {
            for i in 0..N {
                let l = pcm[(off + i) * CHANNELS] as f32;
                let r = pcm[(off + i) * CHANNELS + 1] as f32;
                inb[i] = 0.5 * (l + r); // mezcla a mono
            }
            // RNNoise espera/devuelve f32 en rango i16 (no normalizado a ±1).
            vad = vad.max(self.state.process_frame(&mut outb, &inb));
            for i in 0..N {
                let s = outb[i].clamp(-32768.0, 32767.0) as i16;
                pcm[(off + i) * CHANNELS] = s; // mismo mono a ambos canales
                pcm[(off + i) * CHANNELS + 1] = s;
            }
            off += N;
        }
        self.last_vad = vad;
    }
}

/// Cadena completa de procesamiento del micrófono, en el orden recomendado para
/// telefonía: volumen de entrada → anti-eco → supresión de ruido (RNNoise) →
/// AGC. Hacer el anti-eco ANTES de la supresión de ruido evita reenviar la voz
/// de otros; hacer el AGC AL FINAL evita amplificar ruido ya suprimido. Publica
/// el nivel resultante para el medidor de la UI. Cada paso respeta su toggle.
pub struct VoiceProcessor {
    agc: Agc,
    ducker: EchoDucker,
    denoiser: Denoiser,
    gate: NoiseGate,
    aec: crate::aec::Aec,
}

impl VoiceProcessor {
    /// Longitud del filtro AEC (≈43 ms a 48 kHz): presupuesto de retardo+reverb
    /// que puede cancelar. Si el retardo del pipeline lo supera, hay que subirlo
    /// (o pasar a FDAF para que filtros más largos sean baratos) — pendiente Meta 5.
    const AEC_TAPS: usize = 2048;

    pub fn new() -> Self {
        Self {
            agc: Agc::new(),
            ducker: EchoDucker::new(),
            denoiser: Denoiser::new(),
            gate: NoiseGate::new(),
            aec: crate::aec::Aec::new(Self::AEC_TAPS),
        }
    }

    pub fn process(&mut self, pcm: &mut [i16]) {
        let o = options();
        let g = o.input_gain();
        if (g - 1.0).abs() > 0.001 {
            apply_gain(pcm, g);
        }
        // Etapa anti-eco. Con AEC avanzado (opt-in) RESTA el eco usando la
        // referencia far-end alineada (permite doble-habla); si no, el ducker
        // básico ATENÚA el micro mientras suena la voz remota. Misma posición en
        // la cadena (antes de la supresión de ruido).
        if o.aec_enabled() {
            let frames = pcm.len() / CHANNELS;
            let far = o.take_far_ref(frames);
            self.aec.process(pcm, &far);
        } else if o.echo_suppress.load(Ordering::Relaxed) {
            let r = rms(pcm);
            self.ducker.process(pcm, r, o.out_env());
        }
        // Supresión de ruido según el modo elegido (estilo Discord):
        //   Ligero = puerta de ruido por umbral; Aislamiento = RNNoise (IA).
        match o.noise_mode() {
            NoiseMode::Light => self.gate.process(pcm),
            NoiseMode::VoiceIsolation => self.denoiser.process(pcm),
            NoiseMode::Off => {}
        }
        if o.agc.load(Ordering::Relaxed) {
            self.agc.process(pcm);
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
    let src_rate = cfg.sample_rate().0;
    if src_rate != SAMPLE_RATE {
        tracing::info!("audio in: remuestreando {src_rate}→{SAMPLE_RATE} Hz (cúbico)");
    }
    // Resampler solo si el dispositivo NO da 48 kHz; en 48 kHz la ruta es idéntica
    // a la versión probada (sin tocar nada). `tmp` reusa memoria entre callbacks.
    let mut rs = (src_rate != SAMPLE_RATE).then(|| Resampler::new(src_rate, SAMPLE_RATE));
    let mut tmp: Vec<i16> = Vec::new();
    let err_fn = |e| tracing::warn!("error stream entrada: {e}");
    let stream = match cfg.sample_format() {
        cpal::SampleFormat::F32 => dev.build_input_stream(
            &cfg.config(),
            move |data: &[f32], _: &_| {
                let mut b = buf.lock().unwrap();
                feed_input(&mut b, data, ch, &mut rs, &mut tmp, |s| {
                    (s.clamp(-1.0, 1.0) * 32767.0) as i16
                });
                cap(&mut b, SAMPLE_RATE as usize * CHANNELS); // ~1 s máx
            },
            err_fn,
            None,
        )?,
        cpal::SampleFormat::I16 => dev.build_input_stream(
            &cfg.config(),
            move |data: &[i16], _: &_| {
                let mut b = buf.lock().unwrap();
                feed_input(&mut b, data, ch, &mut rs, &mut tmp, |s| s);
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
    let dst_rate = cfg.sample_rate().0;
    if dst_rate != SAMPLE_RATE {
        tracing::info!("audio out: remuestreando {SAMPLE_RATE}→{dst_rate} Hz (cúbico)");
    }
    // El pipeline interno es 48 kHz; si la salida no lo es, remuestrea 48 kHz→nativo.
    // En 48 kHz (`rs=None`) la ruta es idéntica a la versión probada. `native`
    // acumula muestras ya remuestreadas y `tmp` reusa memoria entre callbacks.
    let make_rs = move || (dst_rate != SAMPLE_RATE).then(|| Resampler::new(SAMPLE_RATE, dst_rate));
    let err_fn = |e| tracing::warn!("error stream salida: {e}");
    let stream = match cfg.sample_format() {
        cpal::SampleFormat::F32 => {
            let deaf = deaf.clone();
            let mut rs = make_rs();
            let mut native: VecDeque<i16> = VecDeque::new();
            let mut tmp: Vec<i16> = Vec::new();
            dev.build_output_stream(
                &cfg.config(),
                move |data: &mut [f32], _: &_| {
                    let silent = deaf.as_ref().map(|d| d.load(Ordering::Relaxed)).unwrap_or(false);
                    let gain = options().output_gain();
                    let mut b = buf.lock().unwrap();
                    drain_output(
                        data, &mut b, ch, silent, gain, track_env, &mut rs, &mut native,
                        &mut tmp, |s| s as f32 / 32767.0,
                    );
                },
                err_fn,
                None,
            )?
        }
        cpal::SampleFormat::I16 => {
            let deaf = deaf.clone();
            let mut rs = make_rs();
            let mut native: VecDeque<i16> = VecDeque::new();
            let mut tmp: Vec<i16> = Vec::new();
            dev.build_output_stream(
                &cfg.config(),
                move |data: &mut [i16], _: &_| {
                    let silent = deaf.as_ref().map(|d| d.load(Ordering::Relaxed)).unwrap_or(false);
                    let gain = options().output_gain();
                    let mut b = buf.lock().unwrap();
                    drain_output(
                        data, &mut b, ch, silent, gain, track_env, &mut rs, &mut native,
                        &mut tmp, |s| s,
                    );
                },
                err_fn,
                None,
            )?
        }
        other => bail!("formato de salida no soportado: {other:?}"),
    };
    Ok(stream)
}

/// Normaliza un chunk del micrófono a estéreo i16 48 kHz y lo vuelca en `out`.
/// Con `rs` activo (dispositivo no-48 kHz) remuestrea; si no, es exactamente la
/// ruta probada `push_input_i16` (passthrough). `tmp` reusa memoria entre llamadas.
fn feed_input<T: Copy>(
    out: &mut VecDeque<i16>,
    data: &[T],
    in_ch: usize,
    rs: &mut Option<Resampler>,
    tmp: &mut Vec<i16>,
    conv: impl Fn(T) -> i16,
) {
    if in_ch == 0 {
        return;
    }
    match rs {
        None => push_input_i16(out, data, in_ch, conv),
        Some(r) => {
            tmp.clear();
            for frame in data.chunks(in_ch) {
                let l = conv(frame[0]);
                let rr = if in_ch >= 2 { conv(frame[1]) } else { l };
                tmp.push(l);
                tmp.push(rr);
            }
            r.process(tmp, out);
        }
    }
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
    // Solo en la llamada real (track_env) y con AEC activo, recoge el far-end mono
    // para la cancelación de eco; en otro caso no hay coste.
    let feed_aec = track_env && options().aec_enabled();
    let mut far: Vec<f32> = Vec::new();
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
        if feed_aec {
            far.push(0.5 * (l as f32 + r as f32));
        }
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
    if feed_aec {
        options().push_far_ref(&far);
    }
}

/// Igual que `fill_output` pero remuestrea la fuente de 48 kHz a la frecuencia
/// nativa de la salida cuando `rs` está activo. Sin `rs` (salida a 48 kHz) delega
/// en `fill_output` (ruta idéntica a la probada). La envolvente del anti-eco se
/// mide sobre la señal 48 kHz del far-end (antes de remuestrear), que es la
/// correcta. `native` guarda lo ya remuestreado; `tmp` reusa un chunk de 48 kHz.
#[allow(clippy::too_many_arguments)]
pub fn drain_output<T: Copy + Default>(
    data: &mut [T],
    src: &mut VecDeque<i16>,
    out_ch: usize,
    silent: bool,
    gain: f32,
    track_env: bool,
    rs: &mut Option<Resampler>,
    native: &mut VecDeque<i16>,
    tmp: &mut Vec<i16>,
    conv: impl Fn(i16) -> T,
) {
    if out_ch == 0 {
        return;
    }
    let r = match rs {
        None => {
            fill_output(data, src, out_ch, silent, gain, track_env, conv);
            return;
        }
        Some(r) => r,
    };
    let frames_needed = data.len() / out_ch;
    let mut sumsq = 0.0f64;
    let mut count = 0usize;
    // Referencia far-end para el AEC: se mide a 48 kHz (la fuente), antes de
    // remuestrear, para que esté a la misma frecuencia que el micro.
    let feed_aec = track_env && options().aec_enabled();
    let mut far: Vec<f32> = Vec::new();
    // Rellena `native` desde la fuente 48 kHz (en chunks) hasta cubrir la salida.
    while native.len() < frames_needed * CHANNELS && src.len() >= CHANNELS {
        tmp.clear();
        for _ in 0..480 {
            if src.len() < CHANNELS {
                break;
            }
            let l = src.pop_front().unwrap();
            let rr = src.pop_front().unwrap();
            sumsq += (l as f64) * (l as f64) + (rr as f64) * (rr as f64);
            count += 2;
            if feed_aec {
                far.push(0.5 * (l as f32 + rr as f32));
            }
            tmp.push(l);
            tmp.push(rr);
        }
        r.process(tmp, native);
    }
    if track_env && count > 0 {
        options().update_out_env(((sumsq / count as f64) as f32).sqrt());
    }
    if feed_aec {
        options().push_far_ref(&far);
    }
    let scale = (gain - 1.0).abs() > 0.001;
    for frame in data.chunks_mut(out_ch) {
        let (mut l, mut rr) = if silent || native.len() < CHANNELS {
            (0i16, 0i16)
        } else {
            (native.pop_front().unwrap(), native.pop_front().unwrap())
        };
        if scale {
            l = ((l as f32) * gain).clamp(-32768.0, 32767.0) as i16;
            rr = ((rr as f32) * gain).clamp(-32768.0, 32767.0) as i16;
        }
        for (i, slot) in frame.iter_mut().enumerate() {
            *slot = match i {
                0 => conv(l),
                1 => conv(rr),
                _ => T::default(),
            };
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// `n` frames estéreo intercalados con valor constante `v`.
    fn stereo_const(n: usize, v: i16) -> Vec<i16> {
        let mut x = Vec::with_capacity(n * CHANNELS);
        for _ in 0..n {
            x.push(v);
            x.push(v);
        }
        x
    }

    #[test]
    fn resampler_passthrough_preserves_dc() {
        // step=1.0 (misma frecuencia): DC exacto y ~misma cantidad de frames.
        let mut r = Resampler::new(48_000, 48_000);
        let input = stereo_const(1000, 5000);
        let mut out = VecDeque::new();
        r.process(&input, &mut out);
        let frames = out.len() / CHANNELS;
        assert!((990..=1000).contains(&frames), "frames={frames}");
        // El cúbico de una señal constante es la misma constante (sin deriva).
        for &s in out.iter() {
            assert!((s as i32 - 5000).abs() <= 1, "s={s}");
        }
    }

    #[test]
    fn resampler_downsample_halves_frames() {
        let mut r = Resampler::new(48_000, 24_000); // step=2.0 → mitad de salida
        let input = stereo_const(2000, 1000);
        let mut out = VecDeque::new();
        r.process(&input, &mut out);
        let frames = out.len() / CHANNELS;
        assert!((994..=1000).contains(&frames), "frames={frames}");
    }

    #[test]
    fn resampler_upsample_doubles_frames() {
        let mut r = Resampler::new(24_000, 48_000); // step=0.5 → doble de salida
        let input = stereo_const(1000, 1000);
        let mut out = VecDeque::new();
        r.process(&input, &mut out);
        let frames = out.len() / CHANNELS;
        assert!((1992..=2000).contains(&frames), "frames={frames}");
    }

    #[test]
    fn resampler_streaming_matches_rate() {
        // 44.1→48 kHz procesado en muchos trozos pequeños: el total debe acercarse
        // a la razón teórica (valida que el estado entre callbacks es correcto).
        let mut r = Resampler::new(44_100, 48_000);
        let mut out = VecDeque::new();
        let chunk = stereo_const(441, 2000); // 10 ms a 44.1 kHz
        for _ in 0..10 {
            r.process(&chunk, &mut out);
        }
        let frames = out.len() / CHANNELS; // 4410 in → ~4800 out
        assert!((4790..=4805).contains(&frames), "frames={frames}");
    }
}
