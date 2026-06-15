//! Cancelación de eco acústico (AEC) por filtro adaptativo NLMS — Meta 5.
//!
//! A diferencia del `EchoDucker` (que solo ATENÚA el micro mientras suena la voz
//! remota, cortando el doble-habla), un AEC real **estima y resta** el eco: el
//! altavoz reproduce la señal lejana `x` (far-end); el micro capta la voz cercana
//! `s` MÁS una copia retardada y filtrada de `x` por el camino acústico
//! altavoz→sala→micro. El filtro adaptativo `w` aprende ese camino y produce una
//! estimación del eco `y = w · x`; la salida `e = d − y` es la voz cercana ya
//! limpia, **sin recortar el doble-habla** (puedes hablar a la vez que el otro).
//!
//! Implementación: NLMS (Normalized Least Mean Squares) en el dominio del tiempo,
//! mono, con:
//! - Línea de retardo circular de la referencia (sin memmove por muestra).
//! - Detector de doble-habla (Geigel): congela la adaptación cuando la voz
//!   cercana domina, para que el filtro no diverja aprendiendo la voz propia.
//! - Regularización `delta` y paso `mu` normalizado.
//!
//! ⚠️ EXPERIMENTAL. El punto crítico para que funcione en vivo es la **alineación
//! temporal** entre la referencia que capturamos (lo que mandamos al altavoz) y lo
//! que el micro realmente capta: hay un retardo de pipeline (buffers de software +
//! del dispositivo) que el filtro solo cubre si su longitud `taps` lo abarca. La
//! estimación/compensación fina de ese retardo y la versión en frecuencia (FDAF,
//! más barata para filtros largos) quedan como trabajo siguiente. El núcleo de
//! aquí está verificado con tests de ERLE (reducción de eco) offline.

/// Filtro adaptativo NLMS para cancelación de eco (mono).
pub struct Aec {
    w: Vec<f32>,     // coeficientes del filtro (estimación del camino de eco)
    xring: Vec<f32>, // línea de retardo circular de la referencia (far-end)
    pos: usize,      // índice de escritura en `xring` (la muestra más nueva)
    mu: f32,         // paso de adaptación normalizado (0<mu<2; ~0.3–0.7 estable)
    delta: f32,      // regularización (evita dividir por ~0 en silencios)
    max_abs_x: f32,  // máximo decadente de |x| reciente, para el DTD de Geigel
}

impl Aec {
    /// `taps` = longitud del filtro (en muestras a 48 kHz). Define el "presupuesto
    /// de retardo + reverberación" que puede cancelar: p. ej. 1024 ≈ 21 ms.
    pub fn new(taps: usize) -> Self {
        let taps = taps.max(1);
        Self {
            w: vec![0.0; taps],
            xring: vec![0.0; taps],
            pos: 0,
            mu: 0.5,
            delta: 1e-3,
            max_abs_x: 0.0,
        }
    }

    /// Ajusta el paso de adaptación (por defecto 0.5).
    #[allow(dead_code)]
    pub fn with_mu(mut self, mu: f32) -> Self {
        self.mu = mu;
        self
    }

    /// Procesa UNA muestra: micro `d` y referencia `x` (alineadas en el tiempo).
    /// Devuelve `e` = micro con el eco estimado restado (voz cercana limpia).
    #[inline]
    pub fn step(&mut self, d: f32, x: f32) -> f32 {
        let l = self.w.len();
        // Inserta la referencia más nueva.
        self.xring[self.pos] = x;

        // Estimación de eco y = Σ w[k]·x[n−k], y energía de la referencia (norma).
        let mut y = 0.0f32;
        let mut norm = self.delta;
        let mut idx = self.pos;
        for k in 0..l {
            let xv = self.xring[idx];
            y += self.w[k] * xv;
            norm += xv * xv;
            idx = if idx == 0 { l - 1 } else { idx - 1 };
        }
        let e = d - y;

        // Detector de doble-habla (Geigel): si el micro supera con margen el mayor
        // valor reciente de la referencia, hay voz cercana (no solo eco) → congela
        // la adaptación para no aprender (y luego cancelar) la voz propia.
        let ax = x.abs();
        if ax > self.max_abs_x {
            self.max_abs_x = ax;
        } else {
            self.max_abs_x *= 0.9995; // decaimiento lento (~ varios segundos)
        }
        let near_speech = d.abs() > 2.0 * self.max_abs_x.max(1.0);

        if !near_speech {
            // Actualización NLMS: w += mu · e · x / (||x||² + delta).
            let g = self.mu * e / norm;
            let mut idx = self.pos;
            for k in 0..l {
                self.w[k] += g * self.xring[idx];
                idx = if idx == 0 { l - 1 } else { idx - 1 };
            }
        }

        self.pos = if self.pos + 1 == l { 0 } else { self.pos + 1 };
        e
    }

    /// Procesa una trama PCM estéreo intercalada in situ, restando el eco. `far`
    /// son las muestras MONO de referencia (far-end) alineadas: una por frame
    /// estéreo del micro. La voz es mono, así que mezclamos el micro a mono,
    /// cancelamos y escribimos el resultado a ambos canales (igual que el RNNoise).
    pub fn process(&mut self, pcm: &mut [i16], far: &[f32]) {
        let frames = pcm.len() / crate::audio::CHANNELS;
        let m = frames.min(far.len());
        let ch = crate::audio::CHANNELS;
        for i in 0..m {
            let lft = pcm[i * ch] as f32;
            let rgt = pcm[i * ch + 1] as f32;
            let d = 0.5 * (lft + rgt);
            let e = self.step(d, far[i]).clamp(-32768.0, 32767.0) as i16;
            pcm[i * ch] = e;
            pcm[i * ch + 1] = e;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Generador de ruido determinista (xorshift) en [-amp, amp].
    struct Noise(u64);
    impl Noise {
        fn next(&mut self, amp: f32) -> f32 {
            let mut z = self.0;
            z ^= z << 13;
            z ^= z >> 7;
            z ^= z << 17;
            self.0 = z;
            // u64 → [-1,1)
            let u = (z >> 11) as f32 / (1u64 << 53) as f32; // [0,1)
            (u * 2.0 - 1.0) * amp
        }
    }

    /// Camino de eco sintético (FIR corto con unas pocas muestras de retardo).
    fn echo_path() -> Vec<f32> {
        // Retardo de 3 muestras, luego una cola decreciente.
        vec![0.0, 0.0, 0.0, 0.6, -0.35, 0.2, -0.12, 0.07, -0.04, 0.02]
    }

    /// Convoluciona en streaming `x` con `h` manteniendo la historia.
    fn convolve(h: &[f32], x: &[f32]) -> Vec<f32> {
        let mut out = vec![0.0f32; x.len()];
        for n in 0..x.len() {
            let mut acc = 0.0;
            for (k, &hk) in h.iter().enumerate() {
                if n >= k {
                    acc += hk * x[n - k];
                }
            }
            out[n] = acc;
        }
        out
    }

    fn erle_db(d: &[f32], e: &[f32]) -> f32 {
        let pd: f64 = d.iter().map(|&v| (v as f64) * (v as f64)).sum();
        let pe: f64 = e.iter().map(|&v| (v as f64) * (v as f64)).sum();
        10.0 * ((pd + 1e-9) / (pe + 1e-9)).log10() as f32
    }

    #[test]
    fn aec_converges_single_talk() {
        // Solo eco (sin voz cercana): el AEC debe aprender el camino y reducir el
        // eco fuertemente (ERLE alto) tras converger.
        let h = echo_path();
        let mut rng = Noise(0x1234_5678_9abc_def0);
        let n = 40_000;
        let x: Vec<f32> = (0..n).map(|_| rng.next(1000.0)).collect();
        let echo = convolve(&h, &x);

        let mut aec = Aec::new(64).with_mu(0.7);
        let mut e = vec![0.0f32; n];
        for i in 0..n {
            e[i] = aec.step(echo[i], x[i]);
        }
        // Mide ERLE solo en la cola (ya convergido).
        let tail = n - 8_000..n;
        let erle = erle_db(&echo[tail.clone()], &e[tail]);
        assert!(erle > 20.0, "ERLE={erle} dB (esperado >20)");
    }

    #[test]
    fn aec_passthrough_without_reference() {
        // Sin referencia (nada suena por el altavoz), el AEC no debe tocar la voz.
        let mut rng = Noise(0xdead_beef_cafe_babe);
        let n = 5_000;
        let mut aec = Aec::new(64);
        let mut max_diff = 0.0f32;
        for _ in 0..n {
            let near = rng.next(800.0);
            let e = aec.step(near, 0.0); // x=0 → y=0 → e=d
            max_diff = max_diff.max((e - near).abs());
        }
        assert!(max_diff < 1e-3, "max_diff={max_diff} (debería ser ~0)");
    }

    #[test]
    fn aec_protects_near_end_during_double_talk() {
        // Con voz cercana fuerte presente, el DTD congela la adaptación y el filtro
        // NO debe diverger ni "comerse" la voz cercana: la energía de salida debe
        // seguir siendo comparable a la de la voz cercana (no colapsar a ~0).
        let h = echo_path();
        let mut rx = Noise(0x1111_2222_3333_4444);
        let mut rn = Noise(0x5555_6666_7777_8888);
        let n = 20_000;
        let x: Vec<f32> = (0..n).map(|_| rx.next(1000.0)).collect();
        let echo = convolve(&h, &x);
        let near: Vec<f32> = (0..n).map(|_| rn.next(1200.0)).collect();

        let mut aec = Aec::new(64).with_mu(0.7);
        let mut out_pow = 0.0f64;
        let mut near_pow = 0.0f64;
        for i in 0..n {
            let e = aec.step(echo[i] + near[i], x[i]);
            if i >= n - 8_000 {
                out_pow += (e as f64) * (e as f64);
                near_pow += (near[i] as f64) * (near[i] as f64);
            }
        }
        // La salida conserva al menos ~la mitad de la energía de la voz cercana
        // (no la cancela). Finito y no colapsado.
        assert!(out_pow.is_finite() && out_pow > 0.25 * near_pow,
            "out_pow={out_pow} near_pow={near_pow}");
    }
}
