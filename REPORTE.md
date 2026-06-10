# REPORTE — discord-lite (estado del proyecto)

> Reporte de lo **construido y verificado**. Fecha: 2026-06-09.
> Documentos relacionados: [spec.md](spec.md) (qué/por qué), [plan.md](plan.md)
> (arquitectura), [tasks.md](tasks.md) (desglose), [CONTINUAR.md](CONTINUAR.md)
> (bitácora detallada de la voz/DAVE).

---

## 1. Resumen ejecutivo

`discord-lite` es un **cliente de Discord nativo y ultraligero en Rust** para uso
personal, que reemplaza al cliente oficial (~300 MB Electron) con un **ejecutable
único de ~5.4 MB** y **~22 MB de RAM** en reposo. Cubre **texto** (leer/enviar,
DMs, tiempo real, reconexión) y **voz bidireccional** — incluyendo el **cifrado
de extremo a extremo (E2EE/DAVE)** que Discord exige en muchos canales.

**Estado: funcional y verificado en vivo en ambas fases (texto y voz).** El último
gran bloqueador —el descifrado E2EE de la voz entrante— quedó **resuelto el
2026-06-09**. Se añadió además una ronda de **pulido de UX** (tema oscuro, búsqueda
de DMs, paneles colapsables, supresor de ruido).

| Métrica | Objetivo (spec) | Logrado |
|---|---|---|
| Tamaño del ejecutable | auto-contenido razonable | **~5.4 MB** (un solo `.exe`) |
| RAM en reposo (texto) | 20–50 MB (techo blando 80) | **~22 MB** ✅ |
| RAM con voz activa | « 300 MB (techo blando 120) | dentro de objetivo ✅ |
| Arranque | < 3 s | pocos segundos ✅ |
| Sin Electron/WebView | obligatorio | ✅ (GUI nativa FLTK) |
| Líneas de código | — | ~4 470 (Rust, 13 módulos) |

---

## 2. Estado por criterio de aceptación (spec.md §7)

| CA | Descripción | Estado |
|----|-------------|--------|
| CA-1 | Login con token, persiste entre sesiones; token inválido se rechaza | ✅ Verificado |
| CA-2 | Lectura en vivo (Gateway `MESSAGE_CREATE`) | ✅ Verificado |
| CA-3 | Historial reciente por REST | ✅ Verificado |
| CA-4 | Envío de mensajes llega al canal | ✅ Verificado |
| CA-5 | DMs: abrir, historial, recibir y responder | ✅ Verificado (lista propia + búsqueda) |
| CA-6 | Voz bidireccional, mute/deafen, salir | ✅ **Verificado en vivo** (E2EE incl.) |
| CA-7 | Reconexión automática del Gateway + estado en UI | ✅ Verificado (RESUME + backoff) |
| CA-8 | Doble clic, sin terminal | ✅ (`windows_subsystem=windows`, icono incrustado) |
| CA-9 | RAM « 300 MB | ✅ ~22 MB texto |
| CA-10 | Arranque en pocos segundos | ✅ |
| CA-11 | Sin Chromium/WebView | ✅ |
| CA-12 | Token nunca en código/repo/logs; logout lo borra | ✅ (keychain + redacción) |
| CA-13 | Empaquetado Windows con deps de audio | ✅ Windows · ⬜ Linux/AppImage pendiente |

**Resumen:** 12 de 13 criterios cumplidos y verificados. El único pendiente es el
**empaquetado Linux/AppImage** (CA-13 parcial); Windows —el objetivo principal— está
completo.

---

## 3. Lo construido (módulos)

Crate binario único, 13 módulos (~4 470 líneas):

| Módulo | Líneas | Responsabilidad |
|---|---:|---|
| `voice.rs` | 1085 | Voz: Voice Gateway v8, UDP, IP discovery, transporte XChaCha20-Poly1305 rtpsize, Opus (cpal dúplex), integración DAVE, **noise gate** |
| `ui.rs` | 1047 | GUI FLTK: login, lista de canales + DMs, búsqueda, panel de info y log colapsables, controles de voz, **tema oscuro** |
| `dave.rs` | 602 | **E2EE/DAVE**: MLS (openmls), KeyPackage, exporter, HashRatchet, AES-128-GCM tag-8, formato de frame libdave |
| `gateway.rs` | 356 | Gateway principal: HELLO/IDENTIFY/heartbeat/READY, `MESSAGE_CREATE`, reconexión+RESUME, señalización de voz (op4) |
| `model.rs` | 213 | Tipos `serde` tolerantes (usuario, canal, mensaje, payloads de voz) |
| `state.rs` | 211 | Estado central + `Command`/`AppEvent`; buffers acotados por canal |
| `token_import.rs` | 196 | Importa el token del Discord oficial local (LevelDB + AES-256-GCM + DPAPI) |
| `rest.rs` | 186 | Cliente REST v10 (historial, envío, DMs, validación, rate limits 429) |
| `net.rs` | 183 | Orquestador async: traduce `Command` → REST/voz; puente de eventos |
| `auth.rs` | 126 | Token seguro (keyring + fallback con ACL) + redacción |
| `config.rs` | 93 | Preferencias persistentes (canales seguidos, último canal/voz) |
| `applog.rs` | 90 | Log a archivo junto al `.exe` + panel in-app |
| `main.rs` | 79 | Arranque, runtime tokio, flags `--import`/`--check-import` |

**Toolchain:** Rust **GNU** + mingw-w64 + CMake + `libopus.a` precompilada
(`thirdparty/opus-lib/`, env en `.cargo/config.toml`). GUI **FLTK 1.4** (decisión
del plan **confirmada** por el objetivo de RAM: render por software, sin GPU).

---

## 4. La epopeya DAVE / E2EE (el gran logro no planeado)

Esto **no estaba en el plan ni en las tareas originales**: el plan asumía cifrado
de transporte (`xsalsa`/`xchacha`) por paquete. En la práctica, los canales reales
del usuario **exigen E2EE** y cierran con `4017` si no se negocia **DAVE** (el E2EE
de voz de Discord, basado en **MLS / RFC 9420**). Resolverlo fue el bloque más
difícil del proyecto (8+ ciclos de depuración en vivo).

**Lo que se implementó en `dave.rs` (4 tests unitarios):**
- Handshake **MLS** completo con `openmls` (ciphersuite P256): KeyPackage,
  external sender, Proposals, Welcome, transiciones de epoch.
- Derivación de clave por-emisor vía **MLS-Exporter** (`"Discord Secure Frames v0"`,
  context = userId LE) + **HashRatchet** (RFC 9420 §9).
- **AES-128-GCM con tag truncado a 64 bits** implementado a mano (`aes`+`ghash`,
  porque `aes-gcm` no admite tags < 12 B) y el formato de trailer de **libdave**.

**Hitos de depuración (detallados en CONTINUAR.md):**
1. Superado el muro `4017` declarando `max_dave_protocol_version: 1`.
2. KeyPackage aceptado: la clave era el **lifetime de rango máximo**
   (`not_before=0`, `not_after=u64::MAX`) — openmls ponía `+90d` y Discord lo
   rechazaba por desfase de reloj.
3. Transporte RX arreglado (prefijo no cifrado = AAD mal calculado).
4. **TX E2EE verificado**: el amigo nos escucha.
5. **RX E2EE resuelto (2026-06-09)** — la causa raíz final: los frames entrantes
   del cliente oficial traen un **prefijo de 8 bytes de framing interno** (no Opus)
   delante del ciphertext; había que **saltarlo** y descifrar solo el resto. El
   `base_secret` (userId LE) siempre fue correcto. Se halló con un barrido en vivo
   (context × prefijo × modo-AAD) que dio `MATCH` exacto.

**Resultado:** `fallo_dave` cayó de ~96% a ~0% (de audio real); voz **bidireccional
E2EE funcionando**. Los ~10% de "fallos" residuales eran frames de relleno/silencio,
ahora contabilizados aparte (`relleno`), no como error.

---

## 5. Pulido de UX (2026-06-09)

Sobre la base funcional se añadió una capa de usabilidad:
- **Tema oscuro consistente** (estilo Discord) en login y ventana principal; el área
  de mensajes pasó de texto azul sobre blanco (bajo contraste) a oscuro legible.
- **Mensajes directos separados** de los canales de servidor: lista propia cargada
  de la API (`Command::LoadDms`), con **buscador** que filtra por nombre al teclear.
- **Título de la conversación activa** sobre los mensajes.
- **Panel de información lateral colapsable** (botón ℹ Info): nombre, ID, tipo,
  servidor, participantes y nº de mensajes.
- **Panel de log colapsable** (botón 📋).
- **Supresor de ruido blanco** del micrófono: noise gate adaptativo en el TX
  (aprende el piso de ruido, histéresis + envolvente, sin clics).

---

## 6. Pendientes / próximos pasos

| Prioridad | Tarea | Notas |
|---|---|---|
| Media | **Empaquetado Linux / AppImage** (CA-13) | Windows ya está; Linux es "best-effort" en la spec |
| Baja | Reconexión propia del **subsistema de voz** (T-97) | Hoy la voz no se auto-restablece tras una caída |
| Baja | **Re-upgrade** v0→v1 de DAVE en vivo | Edge case: hoy la sesión MLS se crea una sola vez |
| Baja | **Abrir DM nuevo por ID de usuario** | `rest.open_dm()` ya existe, falta cablear botón |
| Baja | **Limpieza de warnings** | 16 warnings de código muerto (helpers sin usar, etc.) |
| Opcional | Colorear autor distinto del mensaje | Requiere tabla de estilos en el `TextDisplay` |

---

## 7. Cómo construir y ejecutar

```sh
# Requisitos: Rust GNU + mingw-w64 + CMake + libopus.a en thirdparty/opus-lib/
cargo build --release
cp target/release/discord-lite.exe dist/          # lanzador de doble clic

# Importar el token del Discord local (cuenta propia), sin teclearlo:
discord-lite.exe --import

# El log se escribe junto al .exe: dist/discord-lite.log
```

El usuario final solo recibe `dist/discord-lite.exe` y lo abre con **doble clic**;
no necesita terminal ni runtime externo.

---

## 8. Riesgo asumido (recordatorio)

El cliente opera con un **token de usuario** (self-bot), lo cual **va contra los ToS
de Discord** y puede causar el baneo de la cuenta (R-1/R-2 de la spec). Se asume
conscientemente para uso personal: bajo volumen, ritmo humano, respeto de rate
limits, un solo flujo de audio. El token nunca se hardcodea, loguea ni versiona.
