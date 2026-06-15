# Continuar mañana — discord-lite

## 🔬 PRIMERA PRUEBA EN VIVO (2026-06-15) — Metas 1–5 contra la sala real

Log con 6 personas en el canal del amigo (uid 678086459789541386). Resultados:
- ✅ **RX FUNCIONA EN VIVO**: `decodificados` sube sin parar (→1332 en ~20s),
  `fallo_transporte`~2.5% (RTCP benigno), `fallo_dave=59` **se estanca** (fallos
  puntuales del arranque antes de sincronizar claves/epoch 28; NO crece). DAVE/E2EE
  perfecto (Welcome, 6 claves RX, TX E2EE listo). **Las Metas 1–2 quedan validadas
  en vivo.**
- 🐛 **BUG ENCONTRADO Y ARREGLADO — concealment inflado (~18%)**: `relleno=275` ≈
  `concealados=235` (correlación delatora). Causa: los frames de comfort-noise (sin
  magic DAVE) se descartaban con `continue` ANTES del jitter buffer, pero **sí
  avanzan el seq RTP**; al volver audio real, el jitter buffer veía esos seq como
  "perdidos" y los rellenaba con **PLC fabricado** (audio inventado en los silencios
  del emisor). *Fix:* `JitterBuffer` ahora distingue `Some(opus)`=audio vs
  `None`=silencio; `rx_loop` pasa los relleno como `note_silence(seq)` → solo
  mantienen la continuidad de seq, sin PLC. (compila + 11 tests OK).
- ⚠️ **TX a verificar**: `WARN mic_buf vacío ~3s` al arrancar + `opus=33B` (≈silencio)
  → o el usuario no hablaba (probable, con supresión de ruido), o problema de
  captura/permiso. Necesita que el amigo confirme si SE LE OYÓ. Si no: revisar permiso
  de micrófono de Windows y que el dispositivo capte voz (no silencio/comfort-noise).
- 📌 build nuevo (con el fix) en `dist\discord-lite-new.exe` (el .exe estaba bloqueado
  por la app abierta; renombrar tras cerrarla).

## 🎯 ROADMAP — Optimización de sonido (abierto 2026-06-14)

El objetivo de RAM YA está cumplido (22 MB vs 300–800 MB de Discord). La nueva
frontera es **calidad de sonido y resiliencia de paquetes**. Metas en orden de
impacto. Marcar `[x]` al cerrar cada una.

- [x] **Meta 1 — Jitter buffer + PLC + FEC (RX).** ✅ HECHO 2026-06-14 (compila +
  4 tests OK; pendiente verificación EN VIVO con 2ª persona). Era la causa real de
  "fallan paquetes"/audio entrecortado: `rx_loop` decodificaba en orden de llegada.
  - Nuevo `struct JitterBuffer` por SSRC en `voice.rs` (antes de `rx_loop`):
    `HashMap<u16,Vec<u8>>` seq→Opus + cursor `next` + `Decoder` propio. Reordena por
    seq RTP, mantiene cojín `DEPTH=3` (~60 ms), resync si el hueco supera `MAX=24`.
  - PLC: en un hueco sin paquete siguiente, `decode(None::<&[u8]>, …)`.
  - FEC: si el paquete N+1 llegó, `decode(Some(N+1), fec=true)` reconstruye el N
    perdido antes de decodificar N+1 normal.
  - `rx_loop`: `decoders` → `jitters`; extrae `seq` del header RTP (`packet[2..4]`);
    nuevo contador `concealados` en el reporte RX (frames recuperados por PLC/FEC).
- [x] **Meta 2 — Afinar Opus (TX).** ✅ HECHO 2026-06-14. Tras crear el `Encoder`
  en `voice.rs`: `set_inband_fec(true)` + `set_packet_loss_perc(10)` +
  `set_bitrate(BitsPerSecond(64_000))`. Cada uno con warn no-fatal si el backend
  lo rechaza. Es lo que da el FEC que el RX (Meta 1) aprovecha.
- [x] **Meta 3 — Remuestreo.** ✅ HECHO 2026-06-14 (compila + 8 tests OK; pendiente
  probar en un dispositivo NO-48 kHz real). Decisión: en vez de `rubato` se hizo un
  **resampler cúbico (Hermite/Catmull-Rom) propio en `audio.rs`** — CERO dependencias
  nuevas, encaja con los callbacks de tamaño variable de cpal y mantiene el binario
  diminuto. Detalles:
  - `struct Resampler` (estéreo, streaming): 3 muestras de historia por canal +
    posición fraccionaria entre llamadas; `process(input, &mut out)`. Con `step==1.0`
    es passthrough EXACTO, pero solo se instancia si las frecuencias difieren.
  - Entrada: `feed_input` remuestrea nativo→48 kHz si el micro no es 48 kHz; si lo es,
    es la ruta probada `push_input_i16` sin cambios.
  - Salida: `drain_output` remuestrea 48 kHz→nativo (buffer `native` + chunks); si la
    salida es 48 kHz, delega en `fill_output` (ruta probada). La envolvente anti-eco se
    mide sobre el far-end a 48 kHz (antes de remuestrear).
  - ⚠️ La ruta de 48 kHz (la del PC del usuario, WASAPI compartido) queda IDÉNTICA:
    el remuestreo es puramente aditivo para dispositivos raros. 4 tests nuevos
    (passthrough/DC, down/upsample, streaming en trozos).
- [x] **Meta 4 — RNNoise mono + modos en UI.** ✅ HECHO 2026-06-14 (compila + 8
  tests OK). Dos partes:
  - **CPU a la mitad**: `Denoiser` ahora mezcla a MONO y corre UN solo `DenoiseState`
    (antes uno por canal sobre señal idéntica), escribiendo el mono a ambos canales.
  - **Modos estilo Discord**: nuevo enum `config::NoiseMode { Off, Light,
    VoiceIsolation }` (serde snake_case + `from_u8`/`as_u8`). Reemplaza el bool
    `noise_suppression`. `AudioOptions` guarda `noise_mode: AtomicU8` con
    `noise_mode()`/`set_noise_mode()`. `VoiceProcessor::process` hace `match`:
    Ligero→`NoiseGate` (revivido, ya no `dead_code`), Aislamiento→`Denoiser`, Off→nada.
  - **UI**: en «Ajustes de voz», el checkbox de ruido pasó a un `menu::Choice` de 3
    opciones (Desactivada / Ligera / Aislamiento de voz IA), aplicado en vivo + persistido.
  - **Migración**: configs viejas sin `noise_mode` caen en `VoiceIsolation` (= el viejo
    `noise_suppression=true`); el campo `noise_suppression` se ignora si aparece.
- [~] **Meta 5 — AEC real (NLMS).** 🔨 INICIADA 2026-06-14: núcleo + fontanería +
  opt-in HECHOS (compila + 11 tests OK, 3 nuevos del AEC). FALTA el ajuste fino en
  vivo. NO reemplaza al ducker por defecto (sigue siendo el default; el AEC es opt-in).
  - ✅ **Núcleo** `src/aec.rs`: filtro adaptativo **NLMS** mono (`struct Aec`) con
    línea de retardo circular (sin memmove por muestra), **detector de doble-habla
    (Geigel)** que congela la adaptación cuando domina la voz cercana, y `process()`
    estéreo (mezcla a mono, resta eco, escribe a ambos canales). Verificado con tests
    offline: convergencia **ERLE >20 dB** con eco sintético, passthrough sin referencia,
    y protección de la voz cercana en doble-habla.
  - ✅ **Fontanería referencia far-end**: `AudioOptions` guarda `far_ref`
    (ring mono 48 kHz, ~500 ms) + `aec_enabled`. La ruta de reproducción REAL
    (`fill_output`/`drain_output`, `track_env`) la alimenta solo con AEC activo; el TX
    la consume en lockstep (`take_far_ref(960)` por trama).
  - ✅ **Opt-in**: `config.voice.aec` (default false) + checkbox «Cancelación de eco
    avanzada (experimental)» en Ajustes de voz. `VoiceProcessor`: si `aec_enabled`
    corre el AEC (en vez del ducker), misma posición en la cadena. `AEC_TAPS=2048`
    (~43 ms de presupuesto de retardo).
  - ⬜ **PENDIENTE (lo difícil, necesita 2ª persona en vivo):**
    1. **Estimación/compensación de retardo**: el eco en el micro llega con un retardo
       de pipeline (buffers SW+dispositivo) que puede SUPERAR los 43 ms del filtro. Si
       el retardo cae fuera de `AEC_TAPS`, el AEC no cancela. Hace falta estimar D por
       correlación cruzada far↔mic y alinear (o subir taps, con coste). ESTE es el
       make-or-break.
    2. **FDAF/PBFDAF** (con `rustfft`): filtros largos baratos en frecuencia (el
       NLMS por muestra a 2048+ taps es costoso en CPU).
    3. **Ajuste de `mu`/DTD/`AEC_TAPS`** con eco acústico real (sin auriculares).
    4. Tras validar: evaluar hacerlo el default y jubilar el `EchoDucker`.

### Notas de diseño Meta 1 (jitter buffer)
- Estado por SSRC: `HashMap<u16,Vec<u8>>` (seq→Opus) + cursor `next: Option<u16>` +
  `Decoder` + scratch PCM. Comparar seq con `seq_diff(a,b)=a.wrapping_sub(b) as i16`
  (maneja el wrap de u16 en ventanas <32768).
- DEPTH≈3 frames de cojín antes de empezar a emitir/concealar; MAX de seguridad
  para no crecer sin límite. Descartar paquetes más viejos que `next` (tardíos).
- `play_buf` (cpal output) sigue dando el reloj de reproducción y suaviza extra; el
  jitter buffer SOLO reordena + rellena huecos. No tocar el código DAVE/E2EE.

---

## ⭐ ÚLTIMA SESIÓN (2026-06-10, parte 4) — ECO recursivo + RNNoise ("Krisp")

**Síntomas reportados:** eco recursivo que afectaba a TODA la sala (no solo a una
máquina). En el log: la sesión de voz cayó con `close 4006 "Session is no longer
valid"` a las 23:46:23 pero el **TX siguió enviando frames 90 s más** (hasta el
final del log).

**Causa raíz 1 — hilo de audio zombi (bug claro):** en `voice.rs::session`, al
terminar la sesión POR ERROR (p. ej. 4006) se retornaba el `Err` pero **nunca se
ponía `shared.stop = true`**. Los hilos TX/RX (que tienen un clon del `Arc` stop)
seguían vivos capturando micro y enviando UDP indefinidamente. Si el usuario se
reconectaba, había **doble captura del micrófono** → audio duplicado para los
demás = eco. *Fix:* guard `StopOnDrop` que pone `stop=true` en su `Drop` al salir
de `session` por cualquier vía.

**Causa raíz 2 — no había cancelación de eco real, y estaba apagada:** el
"anti-eco" era un *ducker* de solo **-8 dB** y `echo_suppression` venía
**desactivado por defecto** (se desactivó en la parte 3 porque robotizaba). Sin
auriculares, el micro recapta la voz de la sala y la reenvía a -8 dB → audible →
eco recursivo. *Fix:* `EchoDucker` reescrito (ver abajo) y `echo_suppression=true`
por defecto.

**RNNoise = el "Krisp" abierto (lo que pidió el usuario):** Krisp es propietario
(Discord lo licencia, no se puede empaquetar). WebRTC APM tampoco: es C++ con
abseil, **inviable con el toolchain GNU** (ya costó precompilar libopus.a a mano).
El equivalente abierto es **RNNoise**, y `nnnoiseless` es un port en **Rust PURO**
que compila sin libs C. Integrado como `audio::Denoiser` (supresión de ruido /
aislamiento de voz por red neuronal).

**Cambios concretos:**
- `Cargo.toml`: + `nnnoiseless = { version = "0.5", default-features = false }`.
- `voice.rs`: guard `StopOnDrop` mata los hilos de audio al caer la sesión.
- `audio.rs`:
  - `Denoiser` (RNNoise): procesa estéreo en bloques mono de 480 (10 ms), un
    `DenoiseState` por canal; expone `vad()` (prob. de voz).
  - `EchoDucker` reescrito: floor **0.05 (-26 dB)**, ataque 0.3 / liberación 0.08,
    y **hangover de voz `SPEAK_HOLD=15` (~300 ms)** → solo atenúa ECO PURO (usuario
    callado), nunca a mitad de palabra. Esto evita la robotización de la parte 3
    mientras sí mata el eco. `coupling` arranca bajo (0.15) para no tocar a quien
    usa auriculares.
  - `VoiceProcessor`: cadena nueva = gain → anti-eco → RNNoise → AGC (medidor al
    final). `NoiseGate` queda como alternativa ligera (`#[allow(dead_code)]`).
- `config.rs`: `echo_suppression=true` por defecto; `noise_suppression` ahora es
  RNNoise.

**PENDIENTE / a validar con 2+ personas reales en llamada:**
- Afinar `DUCK_FLOOR`/`SPEAK_HOLD`/umbral de `speaking` con eco real sin
  auriculares (no se pudo probar en vivo en esta sesión).
- AEC *real* (resta de referencia, no ducking) sería lo ideal para doble-habla
  limpio: requiere FDAF/NLMS en Rust (rustfft) con alineación de la referencia
  far-end, o cambiar a toolchain MSVC para usar `webrtc-audio-processing`.
- (Opcional) Exponer en la UI modos tipo Discord: Off / Ligero (NoiseGate) /
  Aislamiento de voz (RNNoise).
- (Opcional) Auto-reconexión de voz tras 4006 en `net.rs` (hoy es manual).

## ÚLTIMA SESIÓN (2026-06-10, parte 3) — ARREGLO: la voz salía ROBOTIZADA (v0.2.1)

Tras v0.2.0 la voz salía **robótica/entrecortada** y «saturaba» el canal (los
demás también se oían mal). **Causa raíz:** el procesado nuevo aplicaba al micro
una **ganancia que cambiaba trama a trama** (cada 20 ms):
- `EchoDucker` bajaba el micro a **0.15** y volvía a 1.0 con constantes muy
  rápidas (ataque 0.5/trama) según fluctuaba la voz remota → **modulación de
  amplitud** = efecto robótico. Iba en la ruta TX, así que los demás me oían así.
- `Agc` ajustaba la ganancia rápido (0.06/trama) con el RMS ruidoso por trama →
  más modulación.
- Ambos venían **activados por defecto** en v0.2.0 (la versión que SÍ funcionaba
  no tenía nada de esto), de ahí la regresión.
- Verificado que NO hay flood de paquetes: sigue 1 `sock.send` por trama, paced
  por el micro (50/s). La «saturación» era el artefacto de audio, no la red.

**Fix (v0.2.1):**
- `EchoDucker`: atenuación mínima moderada **0.4 (-8 dB)** y suavizado lento en
  ambos sentidos (coeff 0.04–0.08, ~100–300 ms); lee la envolvente **ya
  suavizada** `out_env` (no el RMS instantáneo). No recorta la voz a trozos.
- `Agc`: adaptación lenta **0.02/trama** (~1 s), tope **4×**, target -17 dBFS.
- `update_out_env`: suavizado en ambos sentidos (antes subía rápido 0.7).
- **Defaults seguros**: `echo_suppression=false` y `auto_gain=false` (la llamada
  sale igual que la versión buena: solo la puerta de ruido, que sí funcionaba).
  Son **opt-in** desde «⚙ Ajustes de voz». `noise_suppression=true` se mantiene.
- `config_48k`: ahora **prioriza la config predeterminada** del dispositivo (modo
  compartido WASAPI = ruta probada); solo busca otro rango de 48 kHz si el
  predeterminado no es 48 kHz, y con el mismo nº de canales. Evita forzar un
  formato que WASAPI compartido no honra (otra posible fuente de glitches).
- Versión **0.2.1** (Cargo.toml + instalador). Release publicado con installer+exe.

**Pendiente de probar EN VIVO:** confirmar que ya NO robotiza con defaults; si el
amigo tiene eco y NO usa auriculares, activar «Cancelación de eco» y comprobar que
ahora atenúa suave sin robotizar (si aún recorta, subir el floor 0.4 o bajar los
coeff del ducker).

---

## ÚLTIMA SESIÓN (2026-06-10) — AJUSTES DE VOZ ESTILO DISCORD (anti-eco + volumen)

Motivo: los amigos oían **eco** y al usuario **muy bajo**. Añadido un panel
«⚙ Ajustes de voz» (botón en la sección Voz de la barra lateral) que replica el
panel Voz y vídeo de Discord, y la cadena de procesamiento del micro que arregla
ambos problemas. Release 2026-06-10 copiado a `dist\`. Compila + 4 tests OK.

### Qué hay nuevo
- **`src/audio.rs` (nuevo)**: opciones de audio EN VIVO (singleton `options()`
  con atómicos, leído por trama desde los hilos de audio), enumeración y
  selección de dispositivos cpal, preferencia de config a **48 kHz** (antes se
  usaba la default del dispositivo aunque no fuera 48k), helpers PCM compartidos
  (`push_input_i16`/`fill_output`/`cap`), y la cadena `VoiceProcessor`:
  1. **Volumen de entrada** (slider 0–200%, como Discord).
  2. **AGC** (`Agc`): acerca la voz a ~-15 dBFS (gain 0.25–6×, suave; solo se
     adapta con señal > umbral para no subir el ruido). ⇐ arregla el "se oye bajo".
  3. **Cancelación de eco** (`EchoDucker`): ducking del micro mientras suena la
     voz remota (envolvente `out_env` alimentada por `fill_output` de la llamada,
     NO por la prueba de mic). Aprende el acople altavoz→micro (ratio RMS,
     persigue el mínimo) para dejar pasar el doble-habla. ⇐ arregla el eco.
  4. **Puerta de ruido** (movida desde voice.rs): auto (piso aprendido) o
     **manual** con el slider de sensibilidad en dB.
  - **Prueba de micrófono** (`start_mic_test`): captura→misma cadena→loopback a
    la salida elegida + medidor (`mic_level` 0–100, escala dB) para la UI.
- **`src/ui.rs`**: ventana «Ajustes de voz» estilo Discord (#313338, headers en
  gris mayúsculas, blurple/verde): dispositivos entrada/salida (Choice,
  «Predeterminado» = default del sistema), volúmenes 0–200%, «PROBEMOS EL
  MICRÓFONO» con barra verde en vivo (timeout 60 ms), «SENSIBILIDAD DE ENTRADA»
  (check auto + slider dB que se desactiva en auto), «PROCESAMIENTO DE VOZ»
  (3 checks: eco/ruido/AGC). Todo se aplica al instante y persiste en config.
- **`src/config.rs`**: struct `VoiceSettings` (`cfg.voice`) con serde defaults;
  se vuelca a `audio::options()` al arrancar (`audio::apply_settings`).
- **`src/voice.rs`**: usa los dispositivos elegidos; el TX corre la cadena
  `VoiceProcessor` en vez del gate suelto; **hot-swap de dispositivos en plena
  llamada** (contador `device_generation`: al cambiar en la UI, el hilo de audio
  suelta los streams y reabre los nuevos sin cortar la sesión); volumen de
  salida aplicado en el callback de reproducción (cambio en vivo).

### También en esta sesión (2026-06-10, parte 2)
- **Icono nuevo**: `icono nuevo.png` (fuente 1024px) → regenerados `icon.ico`
  (multi-tamaño 16–256, exe/instalador), `icon.png` e `icon_window.png` con
  Pillow (recorte del margen transparente). El exe release ya lo lleva.
- **Instalador**: `installer\discord-lite.iss` (Inno Setup 6, instalado vía
  winget). Compilar: `ISCC.exe installer\discord-lite.iss` → genera
  `dist\discord-lite-setup-<versión>.exe`. Per-user (sin admin), accesos
  directos, desinstalador, y al final ofrece abrir
  `ms-settings:privacy-microphone`: la causa más probable de que al compañero
  no se le oyera es el permiso de micrófono de Windows para apps de escritorio
  (la app captura silencio → "TX: el micrófono no produce audio" en el log).
- Versión subida a **0.2.0** (Cargo.toml + instalador).

### Pendiente de verificar EN VIVO (con los amigos)
- Que ya **no haya eco** con la cancelación activada (y que el doble-habla no
  corte demasiado; si corta, bajar el factor 3.0 de `EchoDucker::process` o el
  duck 0.15).
- Que con AGC el volumen llegue bien (si satura, bajar TARGET_RMS 5500).
- El medidor/prueba de mic en la máquina real (latencia del loopback aceptable).
- NOTA: sigue SIN remuestreo; ahora se fuerza 48 kHz si el dispositivo lo
  soporta (WASAPI compartido normalmente sí). Si algún dispositivo no lo
  soporta, se loguea warn y sonará mal (igual que antes).

---

## ÚLTIMA SESIÓN (2026-06-07) — ARREGLADO EL HASHRATCHET DAVE (context de generación)

**Causa raíz del `fallo_dave≈96%` (y de que el amigo nunca confirmara oírnos):** el
`HashRatchet` de DAVE derivaba la clave con **context vacío**. RFC 9420 §9 y libdave/mlspp
usan el **índice de generación como context (u32 BIG-ENDIAN)**:
`DeriveTreeSecret(secret, label, gen, len) = ExpandWithLabel(secret, label, encode_uint32(gen), len)`.
- Verificado contra fuente autoritativa (NO re-investigar):
  - `discord/libdave cpp/src/mls_key_ratchet.cpp` → `hashRatchet_.get(generation)`.
  - `cisco/mlspp src/key_schedule.cpp` `HashRatchet::next()` →
    `derive_tree_secret(next_secret, "key"/"secret", generation, len)`.
  - `secret_{i+1} = DeriveTreeSecret(secret_i, "secret", i, Nh=32)`;
    `key_g = DeriveTreeSecret(secret_g, "key", g, Nk=16)`.
- **Bug clave:** incluso en **generación 0** (caso típico, la inmensa mayoría de frames
  con counter < 2^24) el context real es `[0,0,0,0]`, NO vacío. Por eso fallaba ~100%
  en AMBAS direcciones (RX y TX). Nuestro roundtrip propio pasaba porque era
  auto-consistente, pero NO interoperaba con libdave.
- **Fix:** `dave.rs hash_ratchet_key` ahora pasa `i.to_be_bytes()` / `generation.to_be_bytes()`
  como context. Compila, 4 tests dave OK, release copiado a `dist\`.

**Descartado este ciclo (confirmado contra libdave, NO re-investigar):**
- Exporter context del userId = **LITTLE-endian** (`session.cpp` hace `memcpy(&u64userId)` en
  x86). Nuestro `media_base_secret_for` con `to_le_bytes()` ya era CORRECTO.
- Nonce de 12B: el `truncatedNonce` u32 se copia con `memcpy` → **LITTLE-endian** en offset 8
  (`encryptor.cpp`). Nuestro `build_nonce` con `to_le_bytes()` ya era CORRECTO.
- Trailer Opus: orden libdave = `tag(8) | nonce(leb128) | rangos(vacío) | supp_size(1) |
  magic(2)`. Para Opus los rangos serializan a 0 bytes → nuestro parseo ya coincide.

### ✅ A VERIFICAR EN VIVO (lo único que falta para cerrar voz)
Requiere al amigo (uid `678086459789541386`) en el canal. Flujo abajo. Esperado en el log:
- RX: `fallo_dave` debe **desplomarse** y `decodificados` debe **subir** (>0) → se oye al amigo.
- TX: el amigo debe **confirmar que nos oye**; nuestro icono debe dejar de verse opaco.
- Si `fallo_dave` sigue alto: volcar hex del `plain` (frame DAVE destransporteado) de los
  primeros 2-3 paquetes y comparar contra un frame de referencia; revisar generación
  (`counter>>24`) y boundaries del trailer byte a byte.

---

## ÚLTIMA SESIÓN (2026-06-06) — TRANSPORTE RX ARREGLADO; falta DAVE RX

Depuración de voz EN VIVO con un amigo real (uid `678086459789541386`) en Discord
oficial. Añadida instrumentación y resuelto el primer bug grande. **Resumen:**

### ✅ ARREGLADO: framing del descifrado de transporte RX (modo `aead_xchacha20_poly1305_rtpsize`)
- **Síntoma:** no se oía a nadie; el log mostraba `fallo_transporte ≈ 100%` (todos los
  paquetes entrantes fallaban el descifrado XChaCha20-Poly1305), `decodificados=0`.
- **Causa raíz (confirmada EMPÍRICAMENTE con barrido de combinaciones sobre un paquete
  real):** el prefijo NO cifrado se calculaba mal. El bug estaba en la antigua
  `rtp_header_len`, que sumaba el *cuerpo* de la extensión RTP al prefijo en claro.
- **Framing REAL de Discord (NO re-investigar):**
  - Prefijo en claro (= AAD) = `12 + 4·cc + (4 si bit X)`. Discord SIEMPRE manda el bit X
    con el marcador `BE DE 00 02`, así que el prefijo típico = **16 bytes** (12 header +
    4 de encabezado de extensión). El **cuerpo** de la extensión va **cifrado**.
  - Ciphertext = `packet[prefijo .. n-4]` (incluye lo que RTP declara como "cuerpo de
    extensión" + payload + tag Poly1305 de 16B).
  - Nonce = 24B: los **últimos 4 bytes** del paquete copiados en `nonce[0..4]`, resto cero.
  - Ejemplo real: `ct=[16..43] aad=[..16] nonce=front` → autenticó (tag Poly1305 OK).
- **Fix:** nueva `unencrypted_prefix_len(packet, n)` en `voice.rs` (reemplaza a
  `rtp_header_len`). **Resultado en vivo: `fallo_transporte` cayó de ~100% a ~4%.**
  El ~4% residual son paquetes no-media (keepalive/RTCP), inofensivos.
- **TX ya estaba bien:** mandamos header de 12B sin extensión (prefijo en claro = 12),
  que es válido y descifrable por Discord. No tocar TX por esto.

### ❌ BLOQUEADOR ACTUAL (CICLO 8): el descifrado DAVE/E2EE RX falla ~96%
Tras arreglar el transporte, el log muestra:
`RX: recibidos=1000, fallo_transporte=38, fallo_dave=961, sin_clave=0, decodificados=0`
- O sea: el transporte **ya descifra** (llega al frame DAVE interno), pero
  `FrameCryptor::decrypt()` falla en casi todos los frames del amigo. `decodificados=0`
  → **aún no se oye nada**. Tenemos la clave RX del emisor (`DAVE RX: clave para
  ssrc=12948 uid=678086459789541386`), así que NO es `sin_clave`.
- **Próximo paso concreto (mañana):** volcar en hex el `plain` (frame DAVE ya
  destransporteado) de los primeros 2-3 paquetes entrantes y rastrear DÓNDE falla
  `dave.rs FrameCryptor::decrypt` (¿parseo del trailer vs. mismatch del tag de 8B?).
  Sospechas, por orden:
  1. **Derivación de la clave del emisor**: `media_base_secret_for(uid)` usa el userId del
     emisor como context del exporter. Verificar **endianness** (el KeyPackage usa
     credencial userId BIG-endian, pero el exporter usa userId **LE** según notas —
     esa asimetría es un clásico foco de bug; confirmar contra un frame real).
  2. **Generación / HashRatchet**: `generation = counter >> 24`; asegurar que el ratchet
     del emisor se avanza a la generación correcta del frame entrante.
  3. **Parseo del trailer entrante** debe ser exacto: `tag(8) | nonce_leb128 | rangos |
     supp_size(1) | magic(2 = 0xFAFA LE)`; reconstrucción del nonce de 12B (contador u32
     LE en offset 8). El cifrado (TX) ya está testeado; el DESCIFRADO (RX) de frames
     ajenos es lo que falta validar byte a byte.

### ⚠️ TX (que nos oigan) — SIN CONFIRMAR todavía
El amigo nunca confirmó de viva voz si nos oía. Además aparece cada sesión
`TX: el micrófono no produce audio (mic_buf vacío ~3s)` al arrancar, aunque luego
salen frames Opus de ~248B. Verificar mañana: (a) que el amigo confirme si nos oye;
(b) si el micro realmente capta voz o estamos mandando silencio/comfort-noise.

### Instrumentación añadida (queda en el código, útil para mañana)
- **Logging a archivo automático:** `src/applog.rs` escribe `discord-lite.log` **junto al
  .exe** (en `dist\`), aunque se abra con doble clic. Leer ese archivo tras cada prueba.
- **Contadores periódicos** cada 250 frames: TX (`frames enviados, e2ee, opus B, paquete B,
  send`) y RX (`recibidos, fallo_transporte, fallo_dave, sin_clave, decodificados`). El
  reporte RX se emite ANTES de cualquier `continue` (si no, los fallos quedaban mudos).
- El barrido de diagnóstico de framing (brute-force sobre el primer paquete) ya se **quitó**
  tras encontrar la combinación ganadora.

### Flujo de prueba (la app bloquea el .exe mientras corre)
`cargo build --release` → cerrar la app → `cp target/release/discord-lite.exe dist/` →
abrir `dist\discord-lite.exe`, pulsar 🔊 Reunirse al último, con el amigo en el canal →
cerrar → leer `dist\discord-lite.log`.

---

## Estado (resumen rápido)

Cliente Discord ligero en **Rust**, GUI **FLTK**. Reemplazo del oficial (~300 MB)
→ release **4.7 MB**, ~22 MB RAM. Carpeta: `C:\Projectos\Discord`.

### Funciona y verificado
- **Texto**: login, importar token del Discord local (DPAPI+AES-GCM), REST
  (historial/enviar/DMs, rate limits), Gateway en vivo (`MESSAGE_CREATE`,
  heartbeat, reconexión+RESUME), GUI con lista de canales, envío, estado.
- **Validación de IDs** numéricos + resolución de nombres + botón quitar canal +
  purga de entradas inválidas.
- **Lanzador en el escritorio** con icono (`icon.ico` incrustado vía `build.rs`+
  windres). Exe autónomo en `dist\discord-lite.exe`.

### Voz: handshake E2EE COMPLETO Y ESTABLE ✅ (ciclo 5)
- `voice.rs`: Voice Gateway (ahora **v8**), UDP+IP discovery, XChaCha20-Poly1305
  rtpsize, Opus (audiopus + libopus.a precompilada), audio dúplex cpal.
- Arreglado: heartbeat prematuro y orden de IDENTIFY.
- **MURO 4017 SUPERADO**: con `max_dave_protocol_version: 1` entramos al handshake DAVE.
- **DAVE E2EE RESUELTO** (ver CICLO 5 abajo): entramos al grupo MLS, derivamos la
  clave de medios, y la sesión es **estable** (>70s sin cierre). TX E2EE cableado.

## DAVE / E2EE — PROGRESO

### Hecho y verificado (`src/dave.rs`, 4 tests pasan)
- **Cifrado de frames** completo y testeado: AES-128-GCM **tag truncado a 64 bits**
  (implementado a mano con `aes`+`ghash` porque `aes-gcm` no admite tag<12B),
  `HashRatchet` (ExpandWithLabel "key"/"secret"), HKDF-Expand manual (PRK corto),
  LEB128, y el **formato de trailer de libdave**.
- **openmls 0.6 + openmls_rust_crypto 0.2** añadidos y compilando (de-risk hecho).
- Opcodes DAVE (21–31) y helper `user_id_context` definidos.

### Constantes/protocolo confirmados desde discord/libdave (NO re-investigar)
- Ciphersuite MLS **2** = P256_AES128GCM_SHA256_P256. DAVE protocol version **1**.
- key=16B, nonce=12B, contador u32 LE en **offset 8**, tag truncado **8B**,
  magic **0xFAFA** (LE), supplemental_size = u8, generation = `counter >> 24`.
- Trailer tras el ciphertext: `tag(8) | nonce_leb128 | rangos(vacío Opus) | supp_size(1) | magic(2)`.
  AAD vacío para Opus. Opus va **completamente cifrado**.
- Exporter: `base_secret = MLS-Exporter("Discord Secure Frames v0", userId_u64_LE, 16)`.
- Binarios del servidor (op 25/27/29/30) llevan prefijo **seq u16 (BE)** antes del opcode.
- IDENTIFY debe declarar `max_dave_protocol_version: 1`; el `select_protocol_ack`
  (op 4) responde `dave_protocol_version`.

### PROGRESO HANDSHAKE (probado en vivo contra canal 1142745485099159622)
- ✅ Superado **4017**: IDENTIFY con `max_dave_protocol_version: 1` → entra al handshake.
- ✅ `op 4` negocia `dave_protocol_version=1`; creamos `dave::MlsSession` (P256).
- ✅ Recibimos `op 25 EXTERNAL_SENDER` (71B), lo parseamos (`ExternalSender::tls_deserialize_exact`).
- ✅ Enviamos `op 26 KeyPackage` (418B) con `client_binary(26, kp)` = `[opcode][payload]`.
- ✅ `process_welcome` + `media_base_secret` + `FrameCryptor` implementados (listos).
- ✅ Versiones alineadas: openmls 0.6 + rust_crypto/traits/basic_credential **0.3**.
- ✅ Arreglado race 4003 en gateway principal (op4 Voice State solo tras READY).
- ✅ Autojoin de prueba: env `DISCORD_LITE_AUTOJOIN="guild:channel"`. Binario `--import`.

### CICLO 2 — correcciones de interop aplicadas (desde spec oficial dave-protocol)
- ✅ KeyPackage capabilities EXACTAS: `cipher_suites=[P256]`, `credentials=[basic]`,
  versions=[mls10], extensiones/proposals vacías (libdave parameters.cpp).
- ✅ Credencial = `Credential::basic(userId u64 BIG-ENDIAN)` (libdave user_credential.cpp). Coincide.
- ✅ op 26 envía **MLSMessage(KeyPackage)**, no bare (spec: `{uint8 op=26; MLSMessage}`).
  Framing cliente = `[opcode][payload]` (sin seq). KeyPackage se envía tras op 4 (versión!=0).
- ✅ op 30 welcome = `[seq u16][op 30][transition_id u16][Welcome BARE]` → process_welcome salta transition_id y parsea Welcome bare.
- ✅ Constantes spec: server binarios `[seq u16][opcode][...]`; op 28 commit = `[op 28][MLSMessage commit][Welcome?]`.

### CICLO 3 — datos nuevos del usuario (importante)
- En el canal hay 2 personas REALES activas horas (dennis, arath) en Discord oficial,
  hablándose entre ellas (su E2EE funciona). Nuestro icono sale **opaco/diluido** en
  el cliente web = conectados a voz pero NO en el grupo E2EE. → **DESCARTA causa
  "nadie commitea"**; sus clientes deberían commitear nuestro Add y no pasa.
  ⇒ El problema es nuestro **KeyPackage** (serialización openmls vs mlspp de Discord).
- ✅ AÑADIDO: **panel de logs dentro de la app** (`src/applog.rs` + TextDisplay inferior
  en ui.rs, refresco con add_timeout3). `tracing` con `with_ansi(false)` → stderr + GUI.
- SIGUIENTE (ciclo 4): comparar BYTES de nuestro KeyPackage MLSMessage con una impl de
  referencia **github.com/Snazzah/davey** (JS) o libdave; sospechas: firma ECDSA P256
  (DER vs raw), orden de extensiones del leaf, lifetime, o que openmls añada algo.
  Para volcar bytes: loguear hex del kp en `key_package_bytes` y comparar.

### CICLO 5 — ¡E2EE RESUELTO! Entramos al grupo MLS y la sesión es estable ✅
Dos piezas faltaban (ambas confirmadas contra el comportamiento de **davey**):
1. **Lifetime de RANGO MÁXIMO en el KeyPackage**: `not_before=0`, `not_after=u64::MAX`
   (`MAX_TIMESPAN_LIFETIME` en `dave.rs:403`). openmls ponía por defecto `ahora..+90d`,
   que el gateway de Discord rechazaba por desfase de reloj → por eso NO proponía
   nuestro Add (sin op27/op30). Con el lifetime máximo, **el gateway sí propone
   nuestro Add** y manda Welcome. ⇐ ESTA era la causa real del muro de ciclos 1–4.
2. **Config del join idéntica a davey**: `use_ratchet_tree_extension(true)` +
   `PURE_PLAINTEXT_WIRE_FORMAT_POLICY` en `process_welcome` (`dave.rs:441`).
Flujo verificado EN VIVO (canal 1142745485099159622):
  `op4 dave_v=1` → KeyPackage (~391B) → `op25 EXTERNAL_SENDER` → `op27 PROPOSALS` →
  `op30 WELCOME(~959B)` → grupo MLS → `media_base_secret` → `FrameCryptor` listo.
**Arreglado el cierre 4006** ("Session is no longer valid"): tras el Welcome hay que
enviar **op 23 READY_FOR_TRANSITION** con el `transition_id` del Welcome (`voice.rs:208`).
Sin él, el gateway invalidaba la sesión a ~35s. Con él: **>70s estable**.
**TX E2EE cableado** (`voice.rs`): `Shared.e2ee: Arc<Mutex<Option<FrameCryptor>>>`,
poblado al procesar el Welcome; el bucle TX envuelve cada Opus con `cryptor.encrypt()`
antes del cifrado de transporte rtpsize. Compila y corre sin errores de cifrado.

### CICLO 6 — RX E2EE por-emisor IMPLEMENTADO (pendiente verificación en vivo)
Cableada la ruta de recepción E2EE (compila + 4 tests dave OK):
- `dave.rs`: nuevo `MlsSession::media_base_secret_for(uid)` (deriva el base_secret con
  el userId del emisor como context); `media_base_secret()` ahora llama a este con el
  nuestro. Cada participante deriva SU clave con SU userId (LE) — confirmado en spec.
- `voice.rs Shared`: + `rx_e2ee: Arc<Mutex<HashMap<u32, FrameCryptor>>>` (SSRC→cryptor)
  y `e2ee_active: AtomicBool`.
- **op5 SPEAKING ahora se maneja**: aprende el mapeo `ssrc↔user_id` de los remotos y,
  si ya estamos en el grupo MLS, deriva su `FrameCryptor` RX al vuelo. Mapeo guardado en
  `ssrc_uid` por si op5 llega ANTES del Welcome; al procesar el Welcome se derivan todos.
- `rx_loop`: tras el descifrado de transporte, si hay clave para ese SSRC desenvuelve el
  frame DAVE (`cryptor.decrypt`) antes de Opus-decode; con `e2ee_active` pero sin clave
  aún, **descarta** el frame (es ciphertext DAVE, no Opus en claro) en vez de meter basura
  al decoder. Sin DAVE (no-E2EE) decodifica directo como antes.

### CICLO 7 — Transiciones de epoch IMPLEMENTADAS (pendiente verificación en vivo)
Rotación de claves cuando entran/salen miembros (compila + 4 tests dave OK):
- `dave.rs`: `MlsSession::process_commit(mls_message)` procesa un commit MLS entrante
  (`process_message` → `merge_staged_commit`) y **avanza el epoch**; `epoch()` para logs.
- `voice.rs`: helper `install_media_keys(m, shared, ssrc_uid)` que re-deriva TX
  (`shared.e2ee`) **y TODAS las RX** (`shared.rx_e2ee`, con `clear()` previo) desde el
  epoch actual. Lo usan el Welcome y las transiciones (evita la lógica duplicada).
- **op29 ANNOUNCE_COMMIT** (`[transition_id u16][MLSMessage commit]`): procesa el commit,
  manda op23 TRANSITION_READY. Si `transition_id==0` instala claves de inmediato; si no,
  guarda `pending_transition` y espera op22.
- **op22 EXECUTE_TRANSITION**: cuando el `transition_id` coincide con el pendiente,
  ejecuta `install_media_keys` (claves del nuevo epoch activas).
- op21/op24/op31 siguen solo logueados.

SIGUIENTE (ciclo 8):
1. **Verificación humana EN VIVO** (lo único que falta para cerrar voz, no se puede
   automatizar — requiere a dennis/arath conectados al canal 1142745485099159622):
   - TX: que confirmen que **nos oyen** (valida TX E2EE byte a byte vs mlspp; nuestro
     icono debería dejar de verse opaco/diluido en el cliente web).
   - RX: que **nosotros los oigamos** (valida `media_base_secret_for`/decrypt RX).
   - Epoch: que el audio **sobreviva a que alguien entre/salga** (valida op29/op22).
   - Lanzar con `DISCORD_LITE_AUTOJOIN="guild:channel"` + `RUST_LOG=debug` → run.log.
   - ⚠️ Una instancia de discord-lite estaba CORRIENDO y bloqueó `dist\discord-lite.exe`;
     el build nuevo quedó en `target\release\`. Cerrar la instancia y copiar a `dist\`.
2. **Asunciones a confirmar en vivo (ajustar si el log las desmiente)**:
   - Framing de op29 = `[transition_id u16][MLSMessage commit]` (asumido por analogía con
     op30 Welcome; si `process_commit` falla, volcar hex y revisar como se hizo con op30).
   - Que op22 llega DESPUÉS del op29 con el mismo transition_id.
3. ✅ Downgrade a no-E2EE (op21 `protocol_version: 0`) IMPLEMENTADO: op21 registra el
   `downgrade_transition`; op22 con ese id desactiva `e2ee_active`, pone `shared.e2ee=None`
   y limpia `rx_e2ee` → TX manda Opus en claro y RX decodifica directo (transporte sigue
   cifrado con XChaCha20). Pendiente (edge case): **re-upgrade** v0→v1 en vivo (re-IDENTIFY
   del handshake MLS: nuevo op25/KeyPackage/Welcome); hoy `mls` solo se crea una vez en op4.

### Estado del código de voz (resumen)
**Todo lo implementable está hecho y compila** (release OK, 4 tests dave OK). La máquina
DAVE cubre: handshake inicial (op25/26/30), TX E2EE, RX E2EE por-emisor (op5+decrypt),
rotación de epoch (op29/op22) y downgrade (op21 pv=0). **Lo único que falta es
verificación humana EN VIVO** — no automatizable, requiere a dennis/arath en el canal.

### CICLO 4 — KeyPackage verificado válido, sigue rechazado (límite blind)
- Descartado: firma ECDSA P256 es **DER** en openmls (basic_credential/src/lib.rs `to_der()`), correcto.
- Flujo confirmado contra **davey** (impl que funciona): key package tras op4; `setExternalSender(m[3..])`;
  welcome en `m[5..]`; al recibir op27 un MIEMBRO hace processProposals→op28 commit. Joiner espera op30.
- Volcado hex de NUESTRO KeyPackage (395B, en run.log con RUST_LOG=debug, fn to_hex en dave.rs):
  decodificado = RFC9420 válido: mls10/wire=5/cs=2/init65/enc65/sig65/basic(idBE)/caps[mls10][P256][][][basic]/lifetime ok/firmas DER. round-trip OK.
- ⇒ Estructura correcta y coincide con libdave params, PERO el gateway no propone nuestro Add
  (no op27 con nuestro add, no op30). dennis+arath ACTIVOS (descarta entorno).
- **Límite alcanzado sin referencia**: hace falta capturar un KeyPackage REAL que interopere
  (Wireshark sobre cliente oficial, o construir davey/libdave) y diffear byte a byte vs el nuestro.
  Sospecha residual: diferencia sutil de serialización openmls↔mlspp que el parser de Discord
  rechaza aunque openmls la acepte (orden/encoding de algún campo o vector vacío).
- Herramientas listas para el diff: hex en log, panel de logs in-app, autojoin.
- PROBADO bare (390B, como libdave) Y MLSMessage (395B): MISMO resultado (sin op27/op30).
  ⇒ El framing no es; es el CONTENIDO/serialización del leaf que mlspp del gateway rechaza.
  Hex bare de ejemplo guardado en run.log (00010002404104955695...). KeyPackage actual = bare.
- CAMINO REAL para cerrar esto: construir davey (Node addon de libdave) o un binario libdave,
  generar un KeyPackage de referencia y diffear campo a campo vs el nuestro. Sin esa referencia
  (o sin los logs de validación de mlspp) es búsqueda a ciegas.

### SIGUE ATASCADO (ciclo 2): enviamos KeyPackage (396B) pero NO llega op27/op30
Tras KeyPackage correcto, el gateway no propone nuestro Add ni manda Welcome (sin error).
Causas restantes a investigar (CICLO 3):
1. **Ningún miembro activo commitea** nuestro Add (ENTORNO): hace falta un cliente
   Discord OFICIAL activo en el canal que ejecute el commit. Verificar con 2ª persona real.
2. **Firma/serialización del KeyPackage** openmls vs mlspp: ECDSA P256 ¿DER vs raw?,
   orden de campos. Comparar bytes de nuestro KeyPackage con uno de libdave/davey (Snazzah).
3. Referencia JS para comparar: github.com/Snazzah/davey (impl DAVE en JS).
Método: instancia viva + RUST_LOG=debug → run.log. Autojoin: DISCORD_LITE_AUTOJOIN.

### (Histórico ciclo 1) — Discord no completa nuestro Add
Tras enviar el KeyPackage, NO llega `op 27` (Add nuestro) ni `op 30 Welcome`; sin error
del gateway. Causas probables a investigar:
1. **KeyPackage no cumple requisitos de Discord**: revisar capabilities/extensiones del
   leaf node que exige DAVE (comparar con libdave `mls/session.cpp` / `mls/util`).
   En openmls: `KeyPackage::builder().leaf_node_capabilities(...).key_package_extensions(...)`.
2. **Framing cliente→servidor**: confirmar si op 26/28 van `[opcode][payload]` o llevan
   prefijo (seq u16). Verificar en libdave la capa de websocket / `dave.cpp`.
3. Quizá falta responder a `op 24 prepare_epoch` o enviar `op 23 ready_for_transition`.
Método: instancia en vivo + `RUST_LOG=discord_lite=debug`, logs en `run.log`.

### (Histórico) PENDIENTE original — el handshake MLS
1. **MlsSession con openmls** (módulo `dave_mls.rs` o ampliar `dave.rs`):
   credencial básica (identity = userId big-endian), SignatureKeyPair, KeyPackage,
   external sender (op 25) en las extensiones del grupo, procesar Proposals (27),
   crear Commit+Welcome (28), procesar Welcome (30)/Announce (29), avanzar epoch,
   y `group.export_secret(...)` → conectar a `FrameCryptor::from_base_secret`.
2. **Wire en `voice.rs`**: parsear frames **binarios** del WS (seq u16 + opcode),
   despachar opcodes DAVE a la MlsSession, enviar KeyPackage (26) tras op 24,
   `ready_for_transition` (23), manejar transición (21/22).
3. **Integrar `FrameCryptor`** en el audio TX/RX cuando E2EE esté activo (envolver
   el frame Opus DAVE antes del cifrado de transporte rtpsize del UDP).
4. Riesgos de interop (validar con canal real): que `openmls.export_secret`
   coincida con mlspp `do_export`; label exacto del exporter; HashRatchet exacto.

## (Referencia) Notas previas de integración

DAVE = E2EE de voz de Discord, basado en **MLS (RFC 9420)**. Para canales que lo
exigen hay que negociarlo. Plan de ataque:

1. Confirmar primero si **algún** canal permite degradar (probar varios canales);
   si sí, el audio no-E2EE ya debería funcionar y DAVE solo hace falta para los
   canales que lo fuerzan.
2. Para DAVE real: implementar el handshake MLS sobre el Voice Gateway v8
   (opcodes DAVE ~21–31: prepare transition, prepare epoch, MLS key package,
   commits/welcome, etc.) y cifrar el audio con la clave de grupo derivada.
   - Evaluar crate MLS en Rust (p. ej. `openmls`) en vez de portar `libdave` (C++).
   - Es un módulo grande y nuevo: `dave.rs` (estado MLS) integrado en `voice.rs`.
3. Alternativa de bajo esfuerzo: limitar la voz a canales sin E2EE obligatorio.

### Puntos de entrada en el código
- `src/voice.rs` — handshake voz, IDENTIFY (añadir negociación DAVE aquí), audio.
- Cierre/diagnóstico ya muestra el código (ej. `voice ws close (4017: ...)`).

## Build (recordatorio)
Toolchain: Rust **GNU** + mingw-w64 (WinLibs) + CMake + libopus.a en
`thirdparty/opus-lib/` (env en `.cargo/config.toml`). Detalles en `README.md`.
`cargo build --release`; copiar exe a `dist\` para el lanzador.
