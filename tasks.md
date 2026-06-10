# tasks.md — Desglose accionable

> Documento del **Tech Lead**. Desglosa `plan.md` en tareas pequeñas, ordenadas y
> verificables. Cada tarea indica **dependencias** y **criterio de "hecho"
> (DoD)**. El orden permite tener **algo funcional cuanto antes**: primero **leer**
> un canal, luego **enviar**, luego **DMs**.

**Leyenda:** `[T-n]` id de tarea · `dep:` dependencias · `DoD:` definición de
hecho. **Estado:** ✅ hecho y verificado · 🟡 parcial · ⬜ pendiente.

> **Actualizado 2026-06-09.** El proyecto está **implementado y verificado en vivo**
> (texto + voz E2EE). Ver [REPORTE.md](REPORTE.md) para el detalle. Resumen de avance:

| Fase | Estado |
|---|---|
| Fase 0 — Setup | ✅ Completa (T-01…T-04) |
| Fase 1 — Modelos + REST | ✅ Completa (T-10…T-14) |
| Fase 2 — Token + config | ✅ Completa (T-20, T-21) |
| Fase 3 — Estado + async↔UI | ✅ Completa (T-30…T-32) |
| Fase 4 — Gateway | ✅ Completa (T-40…T-45) |
| Fase 5 — UI texto | ✅ Completa (T-50…T-57) |
| Fase 5-bis — Voz | ✅ Completa salvo T-97 (reconexión de voz, ⬜) |
| **Fase 5-ter — E2EE/DAVE (no planeada)** | ✅ Completa (T-D1…T-D4) — ver §nueva |
| Fase 6 — Toolkit (RAM) | ✅ fltk confirmado (T-60) |
| Fase 7 — Empaquetado | 🟡 Windows ✅ (T-70) · Linux ⬜ (T-71) · doc 🟡 (T-72) |
| Fase 8 — Pruebas/aceptación | ✅ 12/13 CA (T-80…T-85) |
| **Fase 9 — Pulido de UX (no planeada)** | ✅ Completa (T-U1…T-U5) — ver §nueva |

---

## Fase 0 — Setup del proyecto ✅

- **[T-01] Inicializar crate Rust + repositorio**
  - dep: —
  - DoD: `cargo new` compila; repo git con `.gitignore` que excluye config local,
    credenciales y `target/`. `cargo run` arranca un binario vacío.

- **[T-02] Fijar dependencias base en `Cargo.toml`**
  - dep: T-01
  - DoD: tokio, reqwest (rustls), serde, serde_json, tokio-tungstenite,
    tracing/anyhow añadidos (versiones de `plan.md` §7); `cargo build` ok.

- **[T-03] Esqueleto de módulos y perfiles de build**
  - dep: T-02
  - DoD: módulos vacíos `config`, `auth`, `rest`, `gateway`, `model`, `state`,
    `ui` declarados; perfil `release` con `lto`/`strip`/`opt-level` configurado;
    compila.

- **[T-04] Logging con redacción de secretos**
  - dep: T-03
  - DoD: `tracing` inicializado; existe un util que **garantiza** que el token
    nunca se imprime (helper de redacción); prueba manual de que un log con token
    aparece censurado.

---

## Fase 1 — Modelos y cliente REST mínimo ✅

- **[T-10] Tipos de dominio `serde` (tolerantes)**
  - dep: T-03
  - DoD: structs para `@me`, canal, guild, mensaje, y payloads Gateway (HELLO,
    READY, dispatch, MESSAGE_CREATE) con `Option`/`default`; tests de
    deserialización con JSON de ejemplo pasan.

- **[T-11] Cliente REST base + auth header**
  - dep: T-10
  - DoD: cliente `reqwest` único reutilizable con base `…/api/v10` y header
    `Authorization` (token usuario, sin `Bot`).

- **[T-12] `validate_token` (`GET /users/@me`)**
  - dep: T-11
  - DoD: con un token válido devuelve el usuario; con uno inválido devuelve error
    tipado claro. (Soporta CA-1.)

- **[T-13] Manejo de rate limits (429 + cabeceras)**
  - dep: T-11
  - DoD: ante `429` espera `retry_after` y reintenta; respeta `X-RateLimit-*`;
    cubierto por test/simulación. (Soporta R-1/RNF-5.)

- **[T-14] `get_channel_messages` (historial)**
  - dep: T-11, T-10
  - DoD: devuelve los últimos N mensajes de un canal por ID, deserializados.
    (Soporta CA-3.)

---

## Fase 2 — Token seguro y config ✅

- **[T-20] Almacenamiento seguro del token (keyring + fallback)**
  - dep: T-04
  - DoD: guardar/recuperar/borrar token vía `keyring`; si no hay keychain, archivo
    con permisos restringidos (0600 / ACL usuario). Token nunca en `config` ni en
    logs. (Soporta RNF-8, R-2, CA-11.)

- **[T-21] Persistencia de preferencias (`config`)**
  - dep: T-03
  - DoD: cargar/guardar TOML/JSON en el dir de config del SO (`directories`):
    lista de canales seguidos, último canal abierto. Sobrevive a reinicio.
    (Soporta RF-2, RF-7.)

---

## Fase 3 — Estado y canalización async↔UI ✅

- **[T-30] Definir `Command` (ui→net) y `AppEvent` (net→ui)**
  - dep: T-03
  - DoD: enums con: comandos (enviar, abrir canal, cargar historial, logout) y
    eventos (mensaje nuevo, historial cargado, estado de conexión, resultado de
    envío, error). Documentados.

- **[T-31] Estructura de estado central (`state`)**
  - dep: T-30, T-10
  - DoD: estado con canales seguidos, **buffer acotado** de mensajes por canal,
    DMs, canal activo, estado de conexión; función para aplicar `AppEvent`.
    (Soporta RNF-2, RT-8.)

- **[T-32] Arranque del runtime tokio en hilos de fondo + wiring de canales**
  - dep: T-30
  - DoD: runtime multi-hilo iniciado al lanzar; canales `mpsc` conectados entre el
    futuro hilo GUI y las tareas de red; sin `await` en el hilo principal.
    (Soporta RT-6.)

---

## Fase 4 — Gateway / tiempo real ✅

- **[T-40] Conexión WebSocket + HELLO**
  - dep: T-32, T-10
  - DoD: conecta al Gateway (WSS) con tokio-tungstenite y recibe HELLO con
    `heartbeat_interval`.

- **[T-41] IDENTIFY + READY**
  - dep: T-40, T-20
  - DoD: envía IDENTIFY con el token; recibe READY; guarda `session_id`,
    secuencia y `resume_gateway_url`.

- **[T-42] Tarea de heartbeat + vigilancia de ACK**
  - dep: T-41
  - DoD: late cada `heartbeat_interval` con la última secuencia; detecta ausencia
    de ACK (zombie) y la señala. (Soporta restricción Gateway.)

- **[T-43] Recepción y despacho de `MESSAGE_CREATE` → `AppEvent`**
  - dep: T-41, T-31
  - DoD: al llegar `MESSAGE_CREATE` de un canal seguido, se emite `AppEvent` y el
    estado se actualiza. (Soporta CA-2.)

- **[T-44] Reconexión automática (backoff + RESUME / re-IDENTIFY)**
  - dep: T-42
  - DoD: ante caída reconecta con backoff exponencial+jitter; intenta RESUME; ante
    Invalid Session hace IDENTIFY limpio; publica estado de conexión.
    (Soporta RF-7, CA-7.)

- **[T-45] Señalización de voz en el Gateway principal (op 4)**
  - dep: T-41, T-10
  - DoD: enviar **Voice State Update (op 4)** para unirse/salir de un canal de voz;
    recibir y deserializar `VOICE_STATE_UPDATE` y `VOICE_SERVER_UPDATE` (endpoint,
    token, session_id) y entregarlos al subsistema de voz. (Habilita RF-6.)

---

## Fase 5 — UI mínima usable (incremental) ✅

> Se construye sobre el **toolkit primario (fltk)**; antes, un prototipo permite
> medir RAM (ver Fase 7 T-72).

- **[T-50] Ventana base + bucle GUI en hilo principal**
  - dep: T-32
  - DoD: abre una ventana nativa; drena `AppEvent`/repinta; cierra limpio.
    (Soporta RNF-1.)

- **[T-51] Pantalla de login (token)**
  - dep: T-50, T-12, T-20
  - DoD: campo de token oculto; valida vía `validate_token`; guarda en almacén
    seguro; error claro si inválido. (Soporta RF-1, CA-1.)

- **[T-52] Vista de canal: listar seguidos + mostrar historial**
  - dep: T-51, T-14, T-21, T-31
  - DoD: muestra canales seguidos y, al abrir uno, su historial reciente.
    (Soporta RF-2/3, CA-3.) **← primer "leer un canal".**

- **[T-53] Mensajes en vivo en la vista**
  - dep: T-52, T-43
  - DoD: un mensaje enviado desde otro cliente aparece en vivo sin recargar.
    (Soporta CA-2.) **← lectura en tiempo real completa.**

- **[T-54] `create_message` + caja de envío en canal**
  - dep: T-53, T-11
  - DoD: el usuario escribe y envía; el mensaje llega al canal y se ve en la app y
    en otro cliente. (Soporta RF-4, CA-4.) **← "enviar".**

- **[T-55] Indicador de estado de conexión**
  - dep: T-44, T-50
  - DoD: la UI muestra conectado / reconectando / offline según `AppEvent`.
    (Soporta CA-7.)

- **[T-56] DMs: listar, abrir, historial, recibir y responder**
  - dep: T-54, T-43
  - DoD: lista DMs (`/users/@me/channels`), abre uno, ve historial, recibe nuevos
    en vivo y responde. (Soporta RF-5, CA-5.) **← "DMs".**

- **[T-57] Ajustes: gestionar canales seguidos + logout**
  - dep: T-52, T-21, T-20
  - DoD: añadir/quitar canales seguidos (persiste); logout borra el token del
    almacén. (Soporta RF-2/8, CA-12.)

---

## Fase 5-bis — Voz (segunda fase funcional) ✅ (salvo T-97)

> Se aborda **después** de que el texto (leer/enviar/DMs) funcione. Todo el
> subsistema es **aislado y desactivable** (R-5). Construye sobre T-45.

- **[T-90] Voice Gateway: handshake + heartbeat propio**
  - dep: T-45
  - DoD: con los datos de `VOICE_SERVER_UPDATE`, abre el **Voice Gateway**
    (WebSocket aparte), envía IDENTIFY de voz, recibe READY y mantiene su
    **heartbeat propio**. (Soporta RF-6.)

- **[T-91] UDP + IP discovery + SELECT PROTOCOL + clave de cifrado**
  - dep: T-90
  - DoD: abre el socket **UDP**, hace el **descubrimiento de IP**, envía SELECT
    PROTOCOL y recibe la **clave secreta** y el **modo de cifrado**. (Soporta RF-6.)

- **[T-92] Integración del códec Opus**
  - dep: T-02 (deps de voz)
  - DoD: encode/decode Opus a 48 kHz / frames de ~20 ms, verificado con un
    round-trip de audio de prueba. (Soporta RF-6, RNF-2b.)

- **[T-93] Audio I/O con `cpal` (captura + reproducción) + ring buffers**
  - dep: T-02
  - DoD: captura del micrófono y reproducción por el dispositivo de salida en hilos
    de tiempo real; comunicación por **ring buffers** sin bloquear. (Soporta RF-6,
    RNF-2b, RT-10.)

- **[T-94] Ruta RX: UDP → descifrar → decodificar → reproducir**
  - dep: T-91, T-92, T-93
  - DoD: el audio de los demás participantes **se escucha** con latencia razonable
    y sin cortes notables (jitter buffer). (Soporta RF-6, CA-6.) **← "escuchar".**

- **[T-95] Ruta TX: micrófono → codificar → cifrar → UDP + flag speaking**
  - dep: T-91, T-92, T-93
  - DoD: los demás **escuchan** al usuario; se envía el estado **speaking**.
    (Soporta RF-6, CA-6.) **← "hablar".**

- **[T-96] UI de voz: unirse/salir, participantes, mute/deafen/volumen**
  - dep: T-94, T-95, T-50, T-31
  - DoD: botón para unirse/salir de un canal de voz; lista de participantes y quién
    habla; controles de **mute**, **deafen** y **volumen** funcionales.
    (Soporta RF-6, CA-6.)

- **[T-97] Reconexión del subsistema de voz** ⬜ pendiente
  - dep: T-94, T-95
  - DoD: si la sesión de voz se cae, intenta restablecerla (re-IDENTIFY de voz);
    estado de la llamada reflejado en la UI. (Soporta RF-6, CA-6/CA-7.)

---

## Fase 6 — Decisión final de toolkit (validación de RAM) ✅

- **[T-60] Prototipo de medición de RAM por toolkit**
  - dep: T-50 (versión mínima portada a cada candidato)
  - DoD: medición de RAM en reposo con 2 canales en **fltk** (y, si hay duda,
    slint/egui); se documenta y se **confirma fltk** o se cambia según datos.
    (Soporta RNF-2, R-4/RT-1, CA-9.)

---

## Fase 7 — Empaquetado del ejecutable 🟡 (Windows ✅ · Linux ⬜)

- **[T-70] Build release Windows (sin consola) + deps de audio**
  - dep: T-56, T-60, T-96
  - DoD: `.exe` release con `windows_subsystem=windows` (sin ventana de consola),
    icono incrustado; **doble clic** abre la app; **libopus** enlazado estático o
    su DLL incluida junto al `.exe`. (Soporta RNF-1, CA-8/CA-13.)

- **[T-71] Build release Linux (ELF / AppImage) + deps de audio** ⬜ pendiente
  - dep: T-56, T-60, T-96
  - DoD: binario ELF ejecutable y/o AppImage auto-contenido que abre con doble
    clic en un entorno de escritorio típico, con **libopus** empaquetada.
    (Soporta RNF-6, CA-13.)

- **[T-72] Documentar procedimiento de build por SO** 🟡 parcial (README + REPORTE; falta Linux)
  - dep: T-70, T-71
  - DoD: `README` con pasos exactos para generar el artefacto en Windows y Linux.

---

## Fase 8 — Pruebas y aceptación ✅ (12/13 CA)

- **[T-80] Pruebas unitarias de protocolo y REST**
  - dep: T-10..T-14, T-40..T-44
  - DoD: tests de (de)serialización, rate limit y máquina de estados del Gateway
    (con fixtures) pasan en CI/local.

- **[T-81] Prueba de reconexión (corte de red)**
  - dep: T-44, T-55
  - DoD: cortar y restaurar la red → reconecta solo y reanuda eventos; estado
    reflejado en UI. (Verifica CA-7.)

- **[T-82] Medición de RAM y arranque en la PC objetivo (texto y voz)**
  - dep: T-70
  - DoD: RAM en reposo (sin voz) muy por debajo de 300 MB (objetivo 20–50 MB) y
    **con voz activa** sigue muy por debajo de ~300 MB; CPU estable sin cortes de
    audio; arranque en pocos segundos. (Verifica CA-9, CA-10, RNF-2b.)

- **[T-83] Auditoría de seguridad del token**
  - dep: T-20, T-57
  - DoD: confirmar que el token no está en código, repo ni logs; está en
    keychain/archivo restringido; logout lo borra. (Verifica CA-12.)

- **[T-85] Prueba de voz end-to-end**
  - dep: T-96, T-97
  - DoD: unirse a un canal de voz con otra persona/cliente: **se escucha y se es
    escuchado**, mute/deafen funcionan, y tras un corte la voz se restablece.
    (Verifica CA-6.)

- **[T-84] Repaso final de criterios de aceptación**
  - dep: todas
  - DoD: checklist CA-1..CA-13 de `spec.md` marcada como cumplida; demo end-to-end
    (login → leer canal → enviar → DM → **voz** → reconexión).

---

## Fase 5-ter — E2EE / DAVE (NO planeada; surgió en implementación) ✅

> Los canales reales del usuario **exigen E2EE** y cierran con `4017` si no se
> negocia **DAVE** (E2EE de voz de Discord, basado en **MLS / RFC 9420**). No
> estaba en el plan original (que asumía solo cifrado de transporte). Fue el bloque
> más difícil del proyecto (8+ ciclos de depuración en vivo). Módulo: `src/dave.rs`.
> Bitácora completa en [CONTINUAR.md](CONTINUAR.md).

- **[T-D1] Handshake MLS sobre el Voice Gateway v8 (opcodes 21–31)** ✅
  - dep: T-90
  - DoD: con `openmls` (ciphersuite P256) se entra al grupo MLS: KeyPackage (op26),
    external sender (op25), Proposals (op27), Welcome (op30), `ready_for_transition`
    (op23). Superado el muro `4017` y el cierre `4006`. **Verificado en vivo.**

- **[T-D2] Derivación de clave de medios por-emisor** ✅
  - dep: T-D1
  - DoD: `MLS-Exporter("Discord Secure Frames v0", userId_LE, 16)` + **HashRatchet**
    (RFC 9420 §9). Cada emisor deriva su clave con su userId. 4 tests unitarios.

- **[T-D3] Cifrado/descifrado de frame DAVE (AES-128-GCM tag-64)** ✅
  - dep: T-D2
  - DoD: AES-128-GCM con **tag truncado a 8 B** (a mano con `aes`+`ghash`) y el
    formato de trailer de **libdave**. **TX** (nos oyen) y **RX** (los oímos)
    verificados en vivo. Causa raíz final del RX: **prefijo de 8 B no cifrado** en
    los frames entrantes, que hay que saltar. **CA-6 cumplido con E2EE.**

- **[T-D4] Transiciones de epoch y downgrade** ✅
  - dep: T-D1
  - DoD: `process_commit` avanza epoch (op29/op22); op21 pv=0 hace downgrade a
    no-E2EE. Implementado y compila; re-upgrade v0→v1 en vivo queda como edge case ⬜.

---

## Fase 9 — Pulido de UX (NO planeada; mejora posterior) ✅

> Mejoras de usabilidad sobre la base funcional. Módulos: `src/ui.rs`, `src/voice.rs`.

- **[T-U1] Tema oscuro consistente** ✅ — paleta estilo Discord en login y ventana
  principal; área de mensajes legible (oscuro + texto claro).
- **[T-U2] DMs separados de los canales + buscador** ✅ — lista propia de DMs
  (`Command::LoadDms` → `rest.list_dms`), filtrada por nombre al teclear.
- **[T-U3] Título de conversación activa + panel de info colapsable** ✅ — botón
  ℹ Info muestra/oculta datos del chat (nombre, ID, tipo, participantes).
- **[T-U4] Panel de log colapsable** ✅ — botón 📋 oculta/muestra el registro.
- **[T-U5] Supresor de ruido blanco del micrófono** ✅ — noise gate adaptativo en el
  TX (aprende el piso de ruido, histéresis + envolvente, sin clics). (RNF-2b.)

---

## Camino crítico (lo mínimo para "algo funcional pronto")

```
T-01→T-02→T-03 → T-10→T-11→T-12 → T-20 → T-30→T-31→T-32
   → T-40→T-41→T-42→T-43 → T-50→T-51→T-52 (LEER)
   → T-53 (TIEMPO REAL) → T-54 (ENVIAR) → T-56 (DMs)
   → T-44/T-55 (RECONEXIÓN)
   ── [el cliente de TEXTO ya es útil aquí] ──
   → T-45 → T-90→T-91 + T-92→T-93 → T-94 (ESCUCHAR) → T-95 (HABLAR)
   → T-96 (UI VOZ) → T-97 (RECONEXIÓN VOZ)
   → T-70 (EXE) → T-82/T-85/T-84 (ACEPTACIÓN)
```

> **Nota de secuencia:** la voz es la **segunda fase funcional**. El cliente de
> texto (leer → enviar → DMs → reconexión) se entrega y se puede usar **antes** de
> empezar la voz, que es el bloque más complejo y arriesgado del proyecto.

Cada hito entre paréntesis es un punto donde la app es **demostrable y útil**,
permitiendo construir y probar de forma incremental.
