# plan.md — Arquitectura y plan técnico

> Documento del **Arquitecto**. Define **cómo** se construye, basándose en
> `spec.md`. Cada requisito (RF/RNF/CA) de la spec se mapea a decisiones de
> arquitectura, módulos y dependencias.

> **🟢 IMPLEMENTADO (2026-06-09).** Este plan ya se ejecutó: la app está construida
> y verificada en vivo (texto + voz **E2EE**). La arquitectura propuesta se mantuvo
> fiel; el cambio mayor respecto al plan fue añadir el módulo **`dave.rs` (E2EE/DAVE,
> MLS/RFC 9420)**, no previsto aquí pero **obligatorio** en los canales reales.
> Decisiones validadas: **fltk** confirmado (~22 MB RAM), **rustls→native-tls** (ver
> §10), modo de cifrado `aead_xchacha20_poly1305_rtpsize`. Detalle de lo construido
> en la **§10** al final y en [REPORTE.md](REPORTE.md).

---

## 1. Visión general de la arquitectura

La app se estructura en **dos mundos que cooperan por paso de mensajes**:

1. **Mundo async (tokio):** todo el I/O de red — cliente REST (reqwest) y cliente
   Gateway (tokio-tungstenite). Vive en un runtime tokio en hilo(s) de fondo.
2. **Mundo GUI (hilo principal):** el bucle de la interfaz. La mayoría de
   toolkits nativos exigen correr en el **hilo principal**, así que la GUI **no**
   se bloquea con `await`; se comunica con el mundo async mediante **canales**.

```
            +---------------------------------------------------+
            |                  Hilo principal (GUI)             |
            |   render del estado + captura de input del usuario|
            +------------------^------------------+-------------+
                               |                  |
                 eventos (Ui<-)|                  |comandos (->Net)
                               |                  v
   +---------------------------+------------------+--------------------+
   |                       Runtime tokio (fondo)                        |
   |                                                                    |
   |   +----------------+         +------------------------------+      |
   |   |  Gateway task  |  recv   |        REST client           |      |
   |   |  (WebSocket)   |-------->|  (reqwest: send, history,    |      |
   |   |  heartbeat +   | eventos |   validate token, list)      |      |
   |   |  reconnect     |         +------------------------------+      |
   |   +----------------+                                               |
   +-------------------------------------------------------------------+
                               |
                       Discord API (REST v10 + Gateway WSS)
```

**Flujo de datos clave (RF-3 → CA-2):**
`Gateway (WebSocket)` → deserializa evento → **canal `tokio::mpsc`** hacia la capa
de estado → la GUI lee el estado actualizado y repinta.

**Flujo de comandos (RF-4, RF-5 → CA-4, CA-5):**
`GUI` (usuario pulsa enviar) → **canal** hacia la tarea de red → `REST POST` →
resultado/echo de vuelta al estado → GUI repinta.

**Flujo de voz (RF-6 → CA-6):** es un tercer subsistema, independiente del REST y
con su propio transporte:
```
 GUI "unirse a voz" ─► Gateway principal: Voice State Update (op 4)
        ◄── eventos VOICE_STATE_UPDATE + VOICE_SERVER_UPDATE (endpoint, token, sesión)
                              │
                              ▼
        Voice Gateway (WebSocket aparte): IDENTIFY voz → SELECT PROTOCOL → heartbeat
                              │
                              ▼
        Conexión UDP  ── IP discovery ── claves de cifrado
          ▲  (RX: descifrar → decodificar Opus → reproducir por altavoz)
          │
        Captura micrófono → codificar Opus → cifrar → (TX) UDP
```
El audio corre en su(s) **propio(s) hilo(s)** (tiempo real), separados de la GUI y
del resto de la red, comunicándose por canales y por el callback de audio del SO.

---

## 2. Arquitectura en módulos

Crate binario único, organizado en módulos internos (no se decide aún la
estructura exacta de archivos; eso es de implementación):

### `config` — preferencias persistentes
- Carga/guarda preferencias no secretas: lista de canales seguidos, último canal
  abierto, ajustes de UI.
- Formato: archivo de texto (TOML/JSON) en el directorio de config del usuario
  (`%APPDATA%` en Windows, `~/.config` en Linux) vía un crate de rutas estándar.
- **No** guarda el token (eso lo hace `auth`).
- Cubre: RF-2, RF-8.

### `auth` — token y almacenamiento seguro
- Guarda/recupera/borra el **token de usuario** (RF-1, RNF-8, R-2).
- Estrategia primaria: **keychain/credential manager del SO** (Windows Credential
  Manager / Secret Service / macOS Keychain) mediante el crate `keyring`.
- Estrategia de respaldo (fallback): **archivo con permisos restringidos**
  (`0600` en Linux; ACL de solo-usuario en Windows) si el keychain no está
  disponible.
- Valida el token contra `GET /users/@me` antes de aceptarlo.
- Garantiza que el token **nunca** se loguea ni se serializa en `config`.
- Cubre: RF-1, RNF-8, R-2, CA-1, CA-11.

### `rest` — cliente REST de Discord
- Envuelve `reqwest` con la base `https://discord.com/api/v10`.
- Operaciones mínimas:
  - `validate_token` → `GET /users/@me`.
  - `get_channel_messages` → `GET /channels/{id}/messages?limit=N` (historial).
  - `create_message` → `POST /channels/{id}/messages` (enviar a canal o DM).
  - `list_guilds` / `list_guild_channels` → para que el usuario elija canales.
  - `list_dms` / `open_dm` → `GET /users/@me/channels`, `POST /users/@me/channels`.
- **Manejo de rate limits (RNF-5, R-1):** respeta cabeceras `X-RateLimit-*` y
  responde a `429` esperando `retry_after`. Cliente HTTP único reutilizable
  (keep-alive) para minimizar coste.
- Cabecera `Authorization: <token>` (token de usuario: **sin** prefijo `Bot`).
- Cubre: RF-3 (historial), RF-4, RF-5, CA-3, CA-4, CA-5.

### `gateway` — cliente WebSocket de tiempo real
- Conexión persistente vía `tokio-tungstenite` (WSS) al Gateway de Discord.
- Máquina de estados del protocolo:
  1. Conectar y recibir **HELLO** (opcode 10) → leer `heartbeat_interval`.
  2. Enviar **IDENTIFY** (opcode 2) con el token y los `intents`/propiedades
     mínimas.
  3. Lanzar la **tarea de heartbeat** (opcode 1) en bucle cada
     `heartbeat_interval` ms; vigilar el **ACK** (opcode 11).
  4. Recibir **READY** → guardar `session_id` y `resume_gateway_url` para RESUME.
  5. Recibir despachos (opcode 0); de interés inmediato: **`MESSAGE_CREATE`**
     (también `MESSAGE_UPDATE`/`MESSAGE_DELETE` opcionales más adelante) y, para
     voz, **`VOICE_STATE_UPDATE`** y **`VOICE_SERVER_UPDATE`**.
  6. **Señalización de voz:** enviar **Voice State Update (op 4)** al unirse/salir
     de un canal de voz y reenviar los dos eventos de voz al módulo `voice`.
- **Heartbeat (restricción técnica):** un temporizador independiente del bucle de
  recepción (usando `tokio::select!` entre "llegó un frame" y "toca latir"). Si no
  llega ACK antes del siguiente latido → conexión zombie → forzar reconexión.
- **Reconexión automática (RF-7, CA-7):**
  - Backoff exponencial con tope (p. ej. 1s, 2s, 4s… máx ~30s) y jitter.
  - Si hay `session_id` + secuencia válida → intentar **RESUME** (opcode 6);
    si Discord responde **Invalid Session** → re-**IDENTIFY** limpio.
  - Reabrir el WebSocket usando `resume_gateway_url` cuando aplique.
- Emite eventos ya deserializados hacia el estado por un `mpsc`.
- Cubre: RF-3 (tiempo real), RF-7 (reconexión), RF-6 (señalización de voz),
  restricciones de Gateway, CA-2, CA-7.

### `voice` — subsistema de voz (Voice Gateway + UDP + audio)
Subsistema **aislado y desactivable** (R-5). Toma la info de
`VOICE_SERVER_UPDATE`/`VOICE_STATE_UPDATE` y establece la sesión de audio:
- **Voice Gateway (WebSocket aparte):**
  - IDENTIFY de voz (server/guild id, user id, session_id, token).
  - **Descubrimiento de IP** (IP discovery sobre UDP) y **SELECT PROTOCOL**.
  - **Heartbeat propio**, independiente del Gateway principal.
  - Recibe la **clave secreta** y el **modo de cifrado** a usar.
- **Transporte UDP (audio):**
  - Socket UDP de tokio; encabezado **RTP** con secuencia/timestamp/SSRC.
  - **Cifrado/descifrado** del payload con el modo vigente
    (`aead_xchacha20_poly1305_rtpsize` preferido; soportar variantes
    `xsalsa20_poly1305*` como fallback). El modo se mantiene como punto fácil de
    actualizar (R-3).
- **Códec Opus:** codificar el micrófono (TX) y decodificar a los demás (RX);
  frames de ~20 ms a 48 kHz.
- **Audio I/O del SO:** captura y reproducción con `cpal` (WASAPI en Windows;
  ALSA/Pulse en Linux), con un **jitter buffer** pequeño para suavizar la red.
- **Controles:** mute (enviar silencio o nada), deafen (no reproducir), volumen de
  salida; enviar el flag **speaking** cuando corresponda.
- **Hilos:** los callbacks de audio de `cpal` corren en hilos de tiempo real
  propios; se comunican con la tarea UDP por colas acotadas (ring buffers) sin
  bloquear ni añadir latencia. Nada de esto toca el hilo de la GUI.
- **Reconexión** propia del Voice Gateway/UDP si la llamada se cae.
- Emite a `state`/`ui`: estado de la llamada, participantes, quién habla.
- Cubre: RF-6, RNF-2 (con voz), RNF-2b, R-5, CA-6.

### `model` — tipos de dominio (serde)
- Structs `serde` para: usuario (`@me`), canal, guild, mensaje, y los payloads del
  Gateway (HELLO, READY, dispatch genérico, MESSAGE_CREATE), más los de **voz**:
  Voice State Update (op 4), `VOICE_STATE_UPDATE`, `VOICE_SERVER_UPDATE`, y los
  payloads del **Voice Gateway** (IDENTIFY/READY/SESSION DESCRIPTION/SPEAKING).
- **Deserialización tolerante** (`#[serde(default)]`, `Option<>`, ignorar campos
  desconocidos) para resistir cambios de la API (R-3).
- Cubre: restricción serde, R-3.

### `state` — estado de la aplicación (modelo compartido)
- Estructura central: canales seguidos, mensajes por canal (buffer acotado, p. ej.
  últimos N por canal para controlar RAM — RNF-2), DMs, canal activo, estado de
  conexión (conectado / reconectando / offline), y **estado de voz** (canal de voz
  actual, participantes, mute/deafen, quién habla, estado de la llamada).
- Aplica los eventos entrantes del Gateway y los resultados del REST.
- **Cómo conviven tokio y la GUI (decisión central):**
  - **Canales `tokio::mpsc`** en ambos sentidos:
    - `net → ui`: la tarea de red envía `AppEvent` (mensaje nuevo, estado de
      conexión, resultado de envío, historial cargado).
    - `ui → net`: la GUI envía `Command` (enviar mensaje, abrir canal, cargar
      historial, logout).
  - El estado compartido se mantiene de forma que la GUI lo lea sin bloquear: o
    bien el estado **vive en el hilo GUI** y se muta drenando el receptor de
    `AppEvent` en cada frame (preferido con egui), o bien un `Arc<Mutex<State>>`
    con bloqueos muy cortos (alternativa con toolkits de callbacks como fltk/slint).
  - **Sin `await` en el hilo de la GUI**; el runtime tokio corre en hilos de
    fondo (`std::thread` + `Runtime`, o `runtime` multi-hilo lanzado al inicio).
- Cubre: RF-3, RF-8, RNF-2, CA-2, CA-7.

### `ui` — capa de interfaz
- Renderiza el estado y captura input. Pantallas mínimas:
  - **Login:** input del token (oculto) + validación + mensajes de error.
  - **Principal:** lista de canales/DMs seguidos a la izquierda; vista de mensajes
    del canal activo + caja de envío a la derecha; indicador de estado de conexión.
  - **Voz:** unirse/salir de un canal de voz, lista de participantes con indicador
    de quién habla, y controles de **mute / deafen / volumen**.
  - **Ajustes:** gestionar canales seguidos, cerrar sesión.
- Drena `AppEvent` del canal de red cada frame y emite `Command` al interactuar.
- Cubre: RF-1..RF-8 (incl. UI de voz), RNF-1, RNF-3, CA-8.

---

## 3. Decisión del toolkit de GUI (justificación vs objetivo de RAM)

Objetivo (RNF-2/RNF-4/CA-9): **20–50 MB de RAM**, **sin Electron/WebView**, un
**solo binario**, **doble clic** (RNF-1), arranque rápido (RNF-3).

### Comparativa

| Criterio | `eframe`/`egui` | `fltk-rs` | `slint` |
|---|---|---|---|
| Paradigma | GUI de **modo inmediato** (repinta cada frame) | Retenido, widgets nativos C++ (FLTK) | Retenido, lenguaje declarativo `.slint` |
| Backend de render | wgpu/glow (GPU) | Software/Xlib/GDI nativo | Software o GPU (Skia/femtovg) |
| RAM típica reposo | **~60–120 MB** (contexto GPU/glow) | **~10–30 MB** | **~20–50 MB** (backend software) |
| Single binary | Sí | Sí (linkea FLTK estático) | Sí |
| Facilidad / velocidad de desarrollo | **Alta** (muy ergonómico en Rust) | Media (API estilo C++) | Media (DSL propio + binding) |
| Encaje async (tokio) | Excelente: drenar `mpsc` por frame es natural en modo inmediato | Bueno: callbacks + `awake`/canales para empujar repintado | Bueno: modelo de eventos + `invoke_from_event_loop` |
| Riesgo para 20–50 MB | **Puede exceder** el objetivo por el contexto GPU | **Bajo** (el más liviano) | **Medio-bajo** (con backend software) |

### Decisión

**Toolkit primario: `fltk-rs`.** Es el que mejor encaja con el objetivo agresivo
de **20–50 MB** (RNF-2): render por software, sin contexto GPU pesado, binario
pequeño y arranque muy rápido. El coste es una API menos ergonómica que egui y un
look-and-feel más sobrio, lo cual es **aceptable** para una herramienta personal
centrada en texto.

> **Nota sobre la voz y la RAM:** el toolkit de GUI **no** es donde más impacta la
> voz; el coste de la voz es CPU (Opus + cifrado) y unos buffers de audio
> pequeños, no la GUI. La elección de `fltk-rs` por su bajo consumo deja margen
> para absorber el subsistema de voz y seguir muy por debajo de ~300 MB (RNF-2 con
> voz). La UI de voz es mínima (lista de participantes + botones mute/deafen/salir).

**Plan B documentado:** si el desarrollo con fltk-rs resulta demasiado lento o
limitante para la vista de chat, **`slint`** con backend software es la segunda
opción (también dentro o cerca de 20–50 MB). **`egui`/`eframe`** queda como
**prototipo rápido** y red de seguridad: por su ergonomía permite validar el flujo
async↔GUI en horas, pero **probablemente supera el techo de RAM** por el contexto
GPU, así que **no** es el candidato final salvo que fltk y slint fallen.

> **Estrategia práctica:** la lógica de red/estado (`rest`, `gateway`, `state`,
> `model`, `auth`, `config`) se diseña **independiente del toolkit**, comunicándose
> solo por `Command`/`AppEvent`. Así el toolkit es intercambiable y se puede
> **medir RAM real** de un prototipo en cada uno antes de comprometerse
> definitivamente (mitiga R-4).

### Convivencia tokio ↔ hilo GUI (resumen de la decisión)

- El runtime **tokio multi-hilo** se inicia al arrancar, en hilos de fondo.
- La **GUI corre en el hilo principal** (requisito de los toolkits nativos).
- Comunicación **solo por canales**:
  - `ui → net`: `mpsc<Command>`.
  - `net → ui`: `mpsc<AppEvent>`; con fltk se usa `app::awake()` (o equivalente)
    para que el hilo GUI repinte al llegar eventos; con egui se drena por frame.
- **Nunca** se llama `block_on`/`await` en el hilo de la GUI. La GUI no posee
  sockets; solo envía intenciones y consume eventos.

---

## 4. Manejo de heartbeat y reconexión (detalle)

- **Heartbeat:** al recibir HELLO se obtiene `heartbeat_interval`. Un bucle
  `tokio::select!` alterna entre:
  - recibir el siguiente frame del WebSocket, y
  - el disparo del temporizador de heartbeat (enviar opcode 1 con la última
    secuencia recibida).
  - Se rastrea el **ACK** (opcode 11). Sin ACK entre dos latidos → cerrar y
    reconectar (conexión zombie).
- **Secuencia (`s`):** se guarda el último número de secuencia recibido para
  heartbeats y para RESUME.
- **Reconexión:** ante cierre/EOF/error o zombie:
  1. Esperar backoff exponencial + jitter (tope ~30s).
  2. Si hay `session_id` + `s` → conectar a `resume_gateway_url` y enviar RESUME.
  3. Si Discord envía **Invalid Session (op 9)** → descartar sesión, IDENTIFY
     desde cero.
  4. Publicar el estado de conexión a la GUI en cada transición (CA-7).

> **Voz:** el **Voice Gateway** tiene su **propio** heartbeat y su **propia**
> reconexión, independientes de los del Gateway principal. Si una llamada se cae,
> se reintenta la sesión de voz (re-IDENTIFY de voz con los datos de
> `VOICE_SERVER_UPDATE`); si el Gateway principal se reconecta, Discord puede
> reenviar los eventos de voz para rehacer la sesión.

---

## 5. Almacenamiento seguro del token (detalle)

- **Primario:** crate `keyring` → Windows Credential Manager / Secret Service
  (Linux) / Keychain (macOS). Clave por servicio+usuario.
- **Fallback:** archivo en el directorio de config del usuario con permisos
  restringidos:
  - Linux: `chmod 0600`.
  - Windows: ACL que limita el acceso al usuario actual.
- **Reglas duras (RNF-8, R-2, CA-11):**
  - El token **no** entra en `config` (preferencias normales).
  - **Nunca** se imprime en logs ni en mensajes de error (redactar siempre).
  - `.gitignore` excluye cualquier archivo de credenciales/config local.
  - Opción de **logout** que borra la entrada del keychain/archivo.

---

## 6. Estrategia de empaquetado por SO (RNF-1, RNF-6, CA-12)

Objetivo: **doble clic, sin terminal**, un ejecutable por SO.

### Windows (objetivo principal)
- Compilar release: `cargo build --release` → `target/release/<app>.exe`.
- **Subsistema Windows (sin consola):** marcar la app como GUI
  (`#![windows_subsystem = "windows"]`) para que el doble clic **no abra una
  ventana de consola**.
- Distribución: el `.exe` suelto basta para doble clic. Opcional: instalador
  ligero (p. ej. con `cargo-wix`/Inno Setup) o simplemente una carpeta con el
  `.exe` y un icono (recurso de icono vía `winres`/`embed-resource`).
- Verificar **RAM y arranque** en la PC objetivo (CA-9, CA-10).
- **Voz:** captura/reproducción vía WASAPI (incluida en Windows, sin instalar
  nada). Enlazar **Opus** de forma estática si es posible (p. ej. crate que compile
  libopus) para no exigir DLLs externas al usuario; si se enlaza dinámico, **incluir
  la DLL** junto al `.exe` (CA-13).

### Linux (deseable / best-effort)
- Compilar release → binario ELF.
- Distribución recomendada: **AppImage** (auto-contenido, doble clic en la mayoría
  de entornos) generado con `linuxdeploy`/`cargo-appimage`, o simplemente el ELF
  con permiso de ejecución.
- Crear un `.desktop` para integración con el menú (opcional).
- Considerar `musl` para un binario más portable si hace falta.
- **Voz:** audio vía ALSA/PulseAudio (`cpal`); el **AppImage debe empaquetar
  libopus** (y, si aplica, las libs de audio) para no depender de que estén
  instaladas. Con `musl` cuidar las libs nativas de audio (puede ser más fácil con
  glibc) (CA-13).

### Notas comunes
- Build en modo **release** con optimizaciones de tamaño razonables
  (`opt-level`, `lto`, `strip`) para reducir binario y memoria.
- El usuario final **no** ejecuta `cargo` ni la terminal: recibe el artefacto ya
  construido.

---

## 7. Dependencias propuestas (crates) con versión

> Versiones orientativas (líneas estables a la fecha del proyecto, 2026). Se
> fijarán exactas en `Cargo.toml` durante la implementación; usar las últimas
> compatibles dentro de cada línea major.

| Crate | Versión (línea) | Para qué | Requisito |
|---|---|---|---|
| `tokio` | `1.x` (features `rt-multi-thread`, `macros`, `sync`, `time`, `net` para UDP) | Runtime async, canales mpsc, timers, UDP de voz | RF-3/6/7, arquitectura |
| `tokio-tungstenite` | `0.24+` | WebSocket del Gateway principal **y del Voice Gateway** | Gateway, RF-3/6/7 |
| `reqwest` | `0.12.x` (`json`, `rustls-tls`) | Cliente REST | RF-3/4/5 |
| `serde` | `1.x` (`derive`) | Modelos de dominio | restricción serde |
| `serde_json` | `1.x` | (De)serialización JSON | restricción serde |
| `keyring` | `3.x` | Token en keychain del SO | RNF-8, R-2 |
| `directories` | `5.x` | Rutas de config por SO | `config` |
| `toml` (o `serde_json`) | `0.8.x` | Persistir preferencias | RF-8 |
| **GUI primaria** `fltk` (`fltk-rs`) | `1.4.x` | Interfaz nativa ligera | RNF-1/2/4, UI |
| **GUI plan B** `slint` | `1.x` | Alternativa ligera retenida | R-4 |
| **GUI prototipo** `eframe`/`egui` | `0.29+` | Validación rápida del flujo (no final) | R-4 |
| `futures-util` | `0.3.x` | Utilidades de streams para el WS | Gateway |
| `tracing` + `tracing-subscriber` | `0.1`/`0.3` | Logs (con token siempre redactado) | observabilidad |
| `anyhow` / `thiserror` | `1.x` | Manejo de errores ergonómico | calidad |
| `tokio-util` | `0.7.x` | Helpers (p. ej. backoff/cancelación) | Gateway reconexión |
| `winres` / `embed-resource` (Windows) | — | Icono y metadatos del `.exe` | empaquetado |
| `rand` | `0.8.x` | Jitter del backoff | reconexión |
| **Voz** `cpal` | `0.15.x` | Captura de micrófono y reproducción (WASAPI/ALSA/Pulse) | RF-6 |
| **Voz** `audiopus` (o `opus`/`magnum-opus`) | `0.3.x` | Códec **Opus** (encode/decode) | RF-6 |
| **Voz** `xsalsa20poly1305` + `chacha20poly1305`/`aead` (RustCrypto) | `0.x` | Cifrado del paquete de voz (modos vigentes) | RF-6, R-3 |
| **Voz** `byteorder` / `bytes` | `1.x` | Construir/parsear encabezado RTP y payloads | RF-6 |
| **Voz** `ringbuf` | `0.4.x` | Buffer/jitter entre callbacks de audio y la red | RF-6, RNF-2b |

> Nota: en lugar de implementar la voz a mano se evaluará reutilizar parte de la
> lógica de **`songbird`** (de la familia serenity); como está pensado para *bots*,
> hay que validar su encaje con **token de usuario** y su consumo. Si no encaja, se
> implementa el subsistema `voice` directamente con los crates de arriba.

> TLS: usar **rustls** (`rustls-tls`) en reqwest y tungstenite para evitar
> dependencia de OpenSSL del sistema y mejorar portabilidad (Windows/Linux).

---

## 8. Riesgos técnicos y mitigaciones

| ID | Riesgo técnico | Mitigación |
|---|---|---|
| RT-1 | El toolkit GUI excede el objetivo de RAM (R-4) | Lógica desacoplada del toolkit; **medir** RAM de prototipos en fltk/slint/egui antes de fijar; elegir fltk como primario. |
| RT-2 | Detección de self-bot / baneo (R-1) | Ritmo humano, sin polling agresivo, respetar rate limits, IDENTIFY/heartbeat estándar; bajo volumen. |
| RT-3 | Fuga del token (R-2) | keychain + fallback con permisos, redacción en logs, `.gitignore`, logout. |
| RT-4 | Cambios de API/Gateway (R-3) | Modelos `serde` tolerantes; aislar protocolo; degradar sin crashear. |
| RT-5 | Conexión zombie / pérdidas silenciosas | Vigilancia de ACK de heartbeat + reconexión con backoff y RESUME. |
| RT-6 | Bloqueo de la GUI por I/O | Toda la red en tokio de fondo; GUI solo canales; sin `await` en hilo UI. |
| RT-7 | Rate limit 429 inesperado | Respetar `retry_after`, cliente HTTP único, sin ráfagas; cola de envío si hace falta. |
| RT-8 | Crecimiento de memoria por historial | Buffers acotados por canal (últimos N mensajes). |
| RT-9 | Cambio del **esquema de cifrado** de voz (R-3) | Modo de cifrado aislado y configurable; soportar el modo vigente + fallback; fácil de actualizar. |
| RT-10 | **Cortes/latencia de audio** (xruns) por bloqueo en callbacks (RNF-2b) | Callbacks de audio sin asignaciones ni I/O; comunicación por ring buffers; jitter buffer pequeño. |
| RT-11 | **Dependencias nativas** de audio/Opus rompen el "doble clic" | Enlazar libopus estático o empaquetar la lib junto al binario/AppImage (CA-13). |
| RT-12 | Voz con token de usuario **más vigilada** (R-5/R-1) | Un solo flujo de audio, comportamiento humano; subsistema de voz desactivable. |

---

## 9. Trazabilidad spec → plan (resumen)

- RF-1 → `auth` + UI login + `rest.validate_token` (CA-1, CA-12).
- RF-2 → `config` + UI ajustes (selección de canales).
- RF-3 → `gateway` (tiempo real) + `rest.get_channel_messages` (historial) + `state` (CA-2, CA-3).
- RF-4 → `rest.create_message` + UI envío (CA-4).
- RF-5 → `rest` DMs + `gateway` + UI (CA-5).
- **RF-6 (voz) → `gateway` (señalización op 4) + `voice` (Voice Gateway + UDP + cifrado + Opus + `cpal`) + `state`/`ui` (CA-6).**
- RF-7 → `gateway` reconexión/heartbeat (CA-7).
- RF-8 → `config` + `auth` persistencia.
- RNF-1/3 → toolkit nativo + empaquetado doble clic (CA-8, CA-10).
- RNF-2/2b/4 → elección fltk + buffers acotados + audio eficiente + sin WebView (CA-9, CA-11).
- RNF-6 → empaquetado Windows/Linux con deps de audio (CA-13).
- RNF-8 → almacenamiento seguro del token (CA-12).

---

## 10. Estado de implementación (lo realmente construido)

> Añadido tras la implementación (2026-06-09). Documenta cómo quedó el código frente
> a este plan. Métricas y criterios de aceptación en [REPORTE.md](REPORTE.md).

### 10.1 Módulos reales (crate binario único, ~4 470 líneas)

La estructura sigue el plan, con tres módulos **adicionales** no anticipados:

| Módulo | En el plan | Notas |
|---|---|---|
| `config`, `auth`, `rest`, `gateway`, `voice`, `model`, `state`, `ui` | Sí | Tal como en §2. |
| `net` | implícito (orquestador) | Traduce `Command`→REST/voz y puentea eventos a la UI. |
| **`dave`** | **No** | **E2EE/DAVE (MLS/RFC 9420)** — ver §10.3. El mayor añadido. |
| **`applog`** | No | Log a archivo junto al `.exe` + panel in-app (diagnóstico con doble clic). |
| **`token_import`** | No | Importa el token del Discord oficial local (LevelDB + AES-256-GCM + DPAPI). |

### 10.2 Desviaciones respecto al plan

- **TLS: `native-tls`, no `rustls`.** El plan proponía rustls; en la práctica se usó
  **native-tls** (SChannel en Windows) por encajar mejor con el toolchain **GNU +
  mingw-w64** elegido (ver [README.md](README.md) y la memoria de toolchain). reqwest
  y tokio-tungstenite van con `native-tls`.
- **GUI confirmada: fltk.** La decisión del §3 se validó: ~22 MB de RAM en reposo,
  muy dentro del objetivo 20–50 MB. No hizo falta el Plan B (slint) ni el prototipo
  (egui).
- **Voz: implementación propia, no `songbird`.** Como se anticipó en §7, songbird
  (orientado a bots) no encajaba con token de usuario + DAVE; se implementó `voice`
  directamente con `cpal` + `audiopus` + RustCrypto.
- **Modo de cifrado de transporte:** se usa `aead_xchacha20_poly1305_rtpsize`
  (XChaCha20-Poly1305) como único modo; el prefijo no cifrado (AAD) se calcula con
  `unencrypted_prefix_len` en `voice.rs`.

### 10.3 Subsistema E2EE/DAVE (no estaba en el plan)

El plan asumía que bastaba el **cifrado de transporte** por paquete. La realidad: los
canales del usuario **exigen E2EE** y cierran con `4017` sin **DAVE** (E2EE de voz de
Discord sobre **MLS / RFC 9420**). Se añadió `src/dave.rs`:

- Handshake **MLS** con `openmls` (ciphersuite P256): KeyPackage, external sender,
  Proposals, Welcome, transiciones de epoch.
- Clave por-emisor vía **MLS-Exporter** (`"Discord Secure Frames v0"`, context =
  userId LE) + **HashRatchet** (RFC 9420 §9).
- **AES-128-GCM con tag truncado a 64 bits** (a mano con `aes`+`ghash`) y formato de
  trailer de **libdave**. Causa raíz final del RX: **prefijo de 8 B no cifrado** en
  los frames entrantes del cliente oficial.

Crates añadidos no listados en §7: `openmls` + `openmls_rust_crypto` +
`openmls_basic_credential` + `openmls_traits`, `aes`, `ghash`, `hmac`, `sha2`, `hkdf`.

### 10.4 Pulido de UX posterior

Sobre la base funcional (no en el plan original): tema oscuro, DMs separados de los
canales con **buscador**, panel de info y de log **colapsables**, y **supresor de
ruido** (noise gate) en el TX. Ver [REPORTE.md](REPORTE.md) §5.

### 10.5 Pendiente

- **Empaquetado Linux/AppImage** (CA-13 parcial; Windows ✅).
- **Reconexión propia de la voz** (T-97) y **re-upgrade v0→v1** de DAVE (edge case).
