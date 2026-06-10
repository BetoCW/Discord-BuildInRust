# spec.md — Cliente Discord nativo ultraligero (uso personal)

> Documento del **Analista**. Define **qué** se construye y **por qué**. Es la
> fuente de verdad para `plan.md` y `tasks.md`. No describe implementación.

---

## 1. Objetivo y motivación

Construir un **cliente de Discord nativo, ligero y de escritorio** para uso
**personal**, que reemplace al cliente oficial en una **PC de pocos recursos**.

El cliente oficial está basado en **Electron** (Chromium + Node empaquetados),
lo que implica un consumo de memoria de **~300 MB** en reposo y un arranque
lento. Para un usuario que solo necesita **leer y escribir texto** en unos pocos
canales y en DMs, ese coste es desproporcionado.

**Motivación medible:** reducir el consumo de RAM a una fracción del oficial
(objetivo 20–50 MB) y arrancar en segundos, manteniendo solo las funciones de
texto que el usuario realmente utiliza.

---

## 2. Alcance

### 2.1 Dentro de alcance (in scope)

- Autenticación mediante **token de usuario** de Discord.
- Conexión en **tiempo real** a la API de Discord (REST + Gateway WebSocket).
- Visualización y lectura en vivo de **uno o varios canales concretos** que el
  usuario elija explícitamente (no todos los servidores ni todos los canales).
- **Envío de mensajes de texto** a esos canales.
- **Lectura y respuesta de mensajes directos (DMs)**.
- **Voz**: conectarse a **canales de voz** elegidos para **hablar y escuchar**
  (audio bidireccional) — entrar/salir del canal, capturar micrófono y reproducir
  el audio de los demás participantes, con controles básicos (mute / silenciar
  entrada, ajustar/volumen o silenciar salida).
- **Reconexión automática** del Gateway tras una caída.
- **Almacenamiento seguro** del token (keychain del SO o archivo con permisos
  restringidos).
- **GUI nativa** lanzable con doble clic (sin terminal).
- **Ejecutable único** por sistema operativo.

### 2.2 Fuera de alcance (out of scope)

- **Video, compartir pantalla (screen-share), stages y streams.** (La **voz sí**
  está dentro de alcance; lo que queda fuera es todo lo que no sea audio.)
- Subida/descarga de archivos adjuntos, imágenes, stickers, GIFs, embeds ricos.
- Reacciones, hilos (threads), foros, eventos, slash-commands.
- Administración de servidores, roles, permisos, moderación.
- Notificaciones push del SO, badges, sonidos.
- Renderizado completo de Markdown de Discord, menciones enriquecidas, emojis
  personalizados (más allá de mostrar el texto crudo o un render mínimo).
- Múltiples cuentas simultáneas.
- Cifrado E2E, sincronización entre dispositivos, historial offline persistente.
- Soporte de bots / aplicaciones (es un cliente de *usuario*).

---

## 3. Requisitos funcionales (detallados)

### RF-1 — Autenticación por token de usuario
- El usuario introduce su **token de usuario** una sola vez.
- El token se valida contra la API (p. ej. `GET /users/@me`) antes de aceptarlo.
- El token se persiste de forma segura para no reintroducirlo en cada arranque.
- Debe existir una forma de **cerrar sesión** (borrar el token almacenado).

### RF-2 — Selección de canales a seguir
- El usuario define una **lista explícita de canales** a mostrar (por ID de
  canal, o eligiéndolos de los servidores a los que pertenece).
- La app **no** carga ni muestra todos los servidores/canales por defecto.
- La lista de canales seguidos se **persiste** en la configuración local.

### RF-3 — Lectura en tiempo real de canales
- Al abrir un canal seguido, se muestra el **historial reciente** (REST).
- Los **mensajes nuevos** aparecen en vivo sin recargar (Gateway → evento
  `MESSAGE_CREATE`).
- Cada mensaje muestra como mínimo: **autor**, **contenido de texto** y
  **marca de tiempo**.

### RF-4 — Envío de mensajes a canales
- El usuario escribe texto y lo envía al canal activo (REST `POST` de mensaje).
- El mensaje enviado aparece reflejado en la vista (por eco del Gateway o
  inserción local).
- Debe respetar **rate limits** de Discord (ver RNF-5 y R-1).

### RF-5 — Mensajes directos (DMs)
- Listar / acceder a conversaciones DM del usuario.
- Leer el historial reciente de un DM y recibir mensajes nuevos en vivo.
- Responder (enviar texto) en un DM.

### RF-6 — Voz (hablar y escuchar en canales de voz)
- El usuario puede **unirse a un canal de voz** de un servidor al que pertenece.
- **Captura de micrófono** y **envío** de su audio al canal en tiempo real.
- **Recepción y reproducción** del audio de los demás participantes.
- **Salir** del canal de voz.
- Controles básicos: **silenciar micrófono** (mute) y **silenciar salida**
  (deafen) / ajuste de volumen de reproducción.
- Indicación mínima de **quién está en el canal** de voz (lista de participantes).
- **Fuera de este requisito:** video, compartir pantalla, supresión de ruido
  avanzada, cancelación de eco sofisticada, prioridad de orador, soundboard.
- Notas técnicas (ver `plan.md` para el detalle): la voz **no** usa REST ni el
  Gateway principal para el audio; requiere el **Voice Gateway** (WebSocket
  aparte), una conexión **UDP** para el audio, **cifrado** del paquete de voz y
  el **códec Opus**. El Gateway principal solo se usa para señalizar la conexión
  (Voice State Update / Voice Server Update).

### RF-7 — Reconexión automática
- Si la conexión Gateway se cae, la app **reintenta conectarse** automáticamente
  con backoff, sin intervención del usuario, y reanuda la recepción de eventos.
- La GUI indica el **estado de conexión** (conectado / reconectando / sin
  conexión).

### RF-8 — Configuración persistente
- Token (almacén seguro) y preferencias (lista de canales, último canal abierto)
  sobreviven al reinicio de la app.

---

## 4. Requisitos no funcionales

| ID | Requisito | Objetivo / criterio |
|----|-----------|---------------------|
| RNF-1 | **Ejecutable de doble clic con GUI** | El usuario abre la app desde un icono/archivo; **nunca** necesita abrir una terminal para el uso normal. |
| RNF-2 | **Consumo de RAM** | Muy por debajo de ~300 MB. **Objetivo: 20–50 MB** en reposo (solo texto, sin voz activa). Aceptable como techo blando: <80 MB. **Con voz activa** se admite un incremento moderado por los buffers de audio y el códec (objetivo seguir **muy por debajo de ~300 MB**; techo blando con voz: <120 MB). |
| RNF-2b | **Consumo de CPU (voz)** | La codificación/decodificación Opus y el cifrado consumen CPU; debe mantenerse **bajo y estable** en la PC objetivo, sin cortes de audio (sin xruns perceptibles) durante una llamada normal. |
| RNF-3 | **Arranque rápido** | De doble clic a ventana usable en **pocos segundos** (objetivo < 3 s en la PC objetivo). |
| RNF-4 | **Sin Electron ni navegador embebido** | Nada de Chromium/CEF/WebView pesado. GUI nativa o de modo inmediato. |
| RNF-5 | **Respeto de rate limits** | Cumplir las cabeceras de rate limit de Discord; ritmo humano, sin ráfagas. |
| RNF-6 | **Multiplataforma** | **Windows = objetivo principal.** Linux = deseable. Formato por SO: `.exe` en Windows; binario ELF / **AppImage** en Linux. |
| RNF-7 | **Tamaño y dependencias** | Binario auto-contenido razonable; sin runtime externo obligatorio para el usuario. |
| RNF-8 | **Seguridad del token** | El token nunca se hardcodea, nunca se registra en logs, nunca entra a control de versiones. Almacenamiento con permisos restringidos. |

### Nota de objetivo principal de SO
El **objetivo principal es Windows** (es la PC del usuario). Linux se diseña como
soporte secundario "best-effort": el código y las dependencias se eligen para ser
portables, pero las pruebas y el empaquetado prioritario son para Windows. El
**cómo** generar cada ejecutable se detalla en `plan.md` (sección de empaquetado);
aquí solo se fija el requisito:
- **Windows:** un `.exe` único, lanzable con doble clic.
- **Linux:** un binario ELF o un **AppImage** auto-contenido y ejecutable.

---

## 5. Restricciones técnicas

- **Lenguaje: Rust.**
- **Comunicación directa con la API de Discord**, sin abrir ni depender del
  cliente oficial. Dos canales:
  - **REST** — `https://discord.com/api/v10` para acciones puntuales: enviar
    mensaje, traer historial de un canal/DM, validar token, listar guilds/canales.
  - **Gateway** — conexión **WebSocket persistente** para eventos en tiempo real.
    Debe manejar:
    - **Handshake / IDENTIFY** con el token.
    - **Heartbeat** periódico (según `heartbeat_interval` del opcode HELLO) para
      mantener viva la conexión.
    - Recepción del evento **`MESSAGE_CREATE`** (y los mínimos necesarios:
      `READY`, `HELLO`, ACKs de heartbeat).
    - **Señalización de voz:** enviar **Voice State Update** (op 4) para
      unirse/salir de un canal de voz y recibir los eventos
      **`VOICE_STATE_UPDATE`** y **`VOICE_SERVER_UPDATE`** que entregan el
      endpoint, token y `session_id` de voz.
    - **Reconexión automática** ante caídas (con RESUME cuando sea posible, o
      re-IDENTIFY).
  - **Voice Gateway + UDP (para la voz)** — la voz **no** viaja por REST ni por el
    Gateway principal. Tras la señalización anterior, la app debe:
    - Abrir un **Voice Gateway** (WebSocket aparte) y completar su propio handshake
      (IDENTIFY de voz, SELECT PROTOCOL, descubrimiento de IP, heartbeat propio).
    - Establecer una conexión **UDP** para transportar el audio.
    - **Cifrar/descifrar** cada paquete de voz con el esquema vigente de Discord
      (variantes de `xsalsa20_poly1305` / `aead_xchacha20_poly1305`).
    - **Codificar/decodificar** el audio con el códec **Opus**.
    - Capturar el **micrófono** y **reproducir** el audio entrante usando la API de
      audio del SO.
    - **Reconexión** propia del Voice Gateway si la llamada se cae.
- **Autenticación por token de usuario**, guardado de forma segura (keychain /
  credential manager del SO; o archivo de configuración con permisos
  restringidos como alternativa). **Nunca** hardcodeado ni versionado.
- **Stack a evaluar y justificar** (la decisión final y su justificación van en
  `plan.md`; aquí se fija qué hay que evaluar):
  - Async/runtime: **tokio**.
  - WebSocket: **tokio-tungstenite**.
  - HTTP: **reqwest**.
  - Serialización: **serde + serde_json**.
  - GUI: **comparar al menos `eframe/egui`** (fácil, un solo binario) **frente a
    `fltk-rs` o `slint`** (suelen usar menos RAM) y **elegir según el objetivo de
    memoria**, explicando el trade-off. Resolver además **cómo conviven el runtime
    async (tokio) y el hilo de la GUI** (canales / paso de mensajes entre hilos).

---

## 6. Riesgos

### R-1 — Violación de los Términos de Servicio (self-bot) — **riesgo asumido**
Usar un cliente propio con un **token de usuario** (no de bot) va **contra los
Términos de Servicio de Discord**: se considera un **"self-bot"** y **puede
causar el baneo de la cuenta**.

- **Severidad:** alta (pérdida de la cuenta).
- **Decisión:** se **asume conscientemente** este riesgo para uso **personal**.
- **Mitigación de diseño:**
  - Uso de **bajo volumen** y a **ritmo humano**: sin spam, sin envíos masivos,
    sin automatizaciones ni bucles de polling agresivos.
  - **Respetar estrictamente los rate limits** (cabeceras `X-RateLimit-*` y
    `429 Too Many Requests` con `retry_after`).
  - Comportamiento de cliente "normal": un `IDENTIFY`, un heartbeat según
    intervalo, sin acciones que un humano no haría.
  - Sin funciones que inviten al abuso (no automatización masiva, no scraping).

### R-2 — El token da acceso total a la cuenta — **secreto crítico**
El token de usuario **equivale a la cuenta completa**: con él se puede leer y
actuar como el usuario.

- **Severidad:** alta (compromiso total de la cuenta).
- **Mitigación:**
  - **Nunca hardcodear** el token ni incluirlo en el repositorio (añadir a
    `.gitignore`, no imprimirlo en logs ni en mensajes de error).
  - Guardarlo en el **keychain/credential manager del SO**; si no es posible, en
    un **archivo con permisos restringidos** (solo el usuario; `600` en Linux,
    ACL de solo-usuario en Windows).
  - Permitir **cerrar sesión / borrar** el token fácilmente.
  - No transmitir el token a ningún servicio que no sea la API oficial de Discord.

### R-3 — Cambios no anunciados en la API/Gateway de Discord
Discord puede cambiar formatos de eventos o endurecer la detección de self-bots.
- **Mitigación:** aislar la lógica de protocolo en módulos propios; deserializar
  de forma tolerante (campos opcionales) para no romper ante campos nuevos. La
  **voz es especialmente frágil** ante cambios: el protocolo de Voice Gateway y,
  sobre todo, el **esquema de cifrado** han cambiado varias veces (deprecación de
  modos antiguos). Mantener el modo de cifrado como un punto fácil de actualizar.

### R-5 — Complejidad y riesgo añadidos por la voz — **riesgo asumido**
La voz multiplica la superficie técnica: Voice Gateway propio, UDP, descubrimiento
de IP, cifrado por paquete, códec Opus, captura/reproducción de audio del SO y
sincronización de hilos de audio en tiempo real. Además, la voz operada con un
**token de usuario** es **especialmente vigilada** por Discord (mayor riesgo de
detección de self-bot que el texto — ver R-1).
- **Severidad:** media-alta (complejidad de desarrollo; mayor exposición a R-1).
- **Decisión:** se **asume** para uso personal; la voz se construye **después** de
  que el texto funcione (entregable incremental), de modo que el proyecto sea útil
  aunque la voz quede como una segunda fase.
- **Mitigación:**
  - Comportamiento de cliente normal (un solo flujo de audio, sin reenvío masivo).
  - Aislar todo el subsistema de voz para poder desactivarlo si Discord cambia el
    protocolo o si se decide reducir el riesgo de cuenta.
  - Dependencias nativas de audio/Opus elegidas por portabilidad (Windows/Linux).

### R-4 — No alcanzar el objetivo de RAM con el toolkit elegido
El toolkit de GUI es el principal factor de consumo.
- **Mitigación:** evaluar `egui` vs `fltk`/`slint` antes de comprometerse
  (decisión y medición en `plan.md`), con criterio de aceptación de RAM medible.

---

## 7. Criterios de aceptación (verificables)

Se considera **terminado** cuando se cumple **todo** lo siguiente:

- **CA-1 (Auth):** Con un token válido introducido una vez, la app arranca en
  sesiones posteriores **sin pedir el token de nuevo**, recuperándolo del almacén
  seguro. Un token inválido se rechaza con un mensaje claro.
- **CA-2 (Lectura en vivo):** Estando un canal seguido abierto, un mensaje
  enviado desde otro cliente aparece en la app **en tiempo real** (vía Gateway)
  sin recargar manualmente.
- **CA-3 (Historial):** Al abrir un canal o DM, se muestra su **historial
  reciente** traído por REST.
- **CA-4 (Envío):** Un mensaje escrito en la app **llega al canal** y es visible
  desde otro cliente de Discord.
- **CA-5 (DMs):** El usuario puede **abrir un DM, leer su historial, recibir
  mensajes nuevos en vivo y responder**.
- **CA-6 (Voz):** El usuario puede **unirse a un canal de voz**, **escuchar** a
  los demás y que **lo escuchen** (audio bidireccional funcional), **silenciar**
  micrófono y salida, ver **quién está** en el canal, y **salir** del canal.
- **CA-7 (Reconexión):** Tras cortar la red y restaurarla, el Gateway
  **se reconecta solo** y se reanuda la recepción de eventos; la GUI refleja el
  estado de conexión. (Si había una llamada de voz, el subsistema de voz también
  intenta restablecerse.)
- **CA-8 (GUI / doble clic):** La app se lanza con **doble clic** y se usa
  enteramente desde su ventana, **sin terminal**.
- **CA-9 (RAM):** En reposo con un par de canales abiertos (sin voz activa), el
  consumo de RAM está **muy por debajo de 300 MB** (objetivo 20–50 MB; techo
  blando < 80 MB); **con voz activa** sigue muy por debajo de ~300 MB (techo
  blando < 120 MB). Verificable con el monitor de recursos del SO.
- **CA-10 (Arranque):** De doble clic a ventana usable en **pocos segundos**.
- **CA-11 (Sin Electron):** El binario **no** embebe Chromium/WebView ni depende
  del cliente oficial.
- **CA-12 (Seguridad del token):** El token **no** aparece en el código fuente,
  ni en el repositorio, ni en logs; está en el almacén seguro o en un archivo con
  permisos restringidos.
- **CA-13 (Empaquetado):** Existe un **`.exe` de doble clic para Windows** y un
  binario/AppImage para Linux, con su procedimiento de generación documentado.
  El empaquetado **incluye** las dependencias nativas de audio/Opus necesarias
  para la voz.

---

## 8. Glosario breve

- **Gateway:** WebSocket persistente de Discord para eventos en tiempo real.
- **REST:** API HTTP de Discord para acciones puntuales (enviar, historial).
- **IDENTIFY / HELLO / HEARTBEAT / RESUME:** opcodes del protocolo Gateway.
- **`MESSAGE_CREATE`:** evento del Gateway emitido al crearse un mensaje nuevo.
- **Self-bot:** cliente no oficial que opera con un token de *usuario*; prohibido
  por los ToS de Discord.
- **Voice Gateway:** WebSocket **separado** del Gateway principal, dedicado a la
  señalización de una sesión de voz (su propio IDENTIFY, heartbeat, etc.).
- **Voice State Update (op 4):** mensaje del Gateway principal para entrar/salir de
  un canal de voz.
- **`VOICE_SERVER_UPDATE` / `VOICE_STATE_UPDATE`:** eventos que entregan endpoint,
  token y sesión necesarios para conectar al Voice Gateway.
- **UDP / RTP:** transporte de los paquetes de audio de voz (fuera del WebSocket).
- **Opus:** códec de audio usado por Discord para la voz.
- **xsalsa20_poly1305 / aead_xchacha20_poly1305:** esquemas de cifrado del paquete
  de voz.
