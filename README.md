<div align="center">

<img src="icon.png" width="160" alt="Logo de Discord Lite"/>

# Discord Lite

**Cliente de Discord nativo y ultraligero, escrito en Rust.**

Texto + voz (con E2EE) en una app nativa: **~6 MB** instalados y **~22 MB de RAM**,
frente a los ~300 MB del cliente oficial (Electron).

[![Release](https://img.shields.io/github/v/release/BetoCW/Discord-BuildInRust?label=Release&color=brightgreen)](https://github.com/BetoCW/Discord-BuildInRust/releases/latest)
[![Descargas](https://img.shields.io/github/downloads/BetoCW/Discord-BuildInRust/total?label=Descargas&color=blue)](https://github.com/BetoCW/Discord-BuildInRust/releases)
[![Rust](https://img.shields.io/badge/Rust-stable--gnu-orange?logo=rust)](https://www.rust-lang.org/)
[![Licencia](https://img.shields.io/badge/Licencia-MIT%20%7C%20Apache--2.0-lightgrey)](#licencia)

### [⬇️ Descargar la última versión](https://github.com/BetoCW/Discord-BuildInRust/releases/latest)

*Recomendado: `discord-lite-setup-x.y.z.exe` (instalador con accesos directos y desinstalador).*
*También hay `discord-lite.exe` portable: doble clic y listo, sin instalación.*

</div>

---

## ✨ Características

| | |
|---|---|
| 💬 **Texto en tiempo real** | Historial, envío de mensajes, DMs y canales de servidores, con reconexión automática y manejo de rate limits. |
| 🎙️ **Voz funcional** | Canales de voz con audio dúplex (Opus + XChaCha20-Poly1305) y **cifrado de extremo a extremo DAVE/MLS**, interoperando con clientes oficiales. Probado en vivo. |
| 🎛️ **Ajustes de voz** | Panel estilo Discord: elegir micrófono/altavoces, volúmenes 0–200 %, prueba de micrófono con medidor, sensibilidad de entrada, **cancelación de eco**, supresión de ruido y ganancia automática. Todo se aplica en vivo, incluso en plena llamada. |
| 🔐 **Token seguro** | Se guarda en el Credential Manager de Windows; nunca en texto plano. |
| 📥 **Importación automática** | Puede leer el token de tu Discord oficial instalado (tu propia cuenta) sin pegarlo a mano. |
| 🪶 **Ultraligero** | GUI nativa (FLTK), sin Electron ni navegador embebido. Un solo ejecutable autónomo. |

## ⚠️ Aviso importante (ToS)

Usar un cliente propio con un **token de usuario** va contra los Términos de
Servicio de Discord (se considera *self-bot*) y **puede causar el baneo de la
cuenta**. Este proyecto es para **uso personal, de bajo volumen y a ritmo
humano**. El token da acceso total a la cuenta: trátalo como secreto crítico.

> La voz con token de usuario es la parte de **mayor riesgo de baneo**. Úsala con criterio.

## 🚀 Primer uso

1. Descarga el **instalador** desde [Releases](https://github.com/BetoCW/Discord-BuildInRust/releases/latest)
   y síguelo (no pide administrador). O usa el `.exe` portable si lo prefieres.
   - ⚠️ Si tu antivirus o SmartScreen lo marca, es un **falso positivo** — ver
     [Falsos positivos de antivirus](#-falsos-positivos-de-antivirus).
2. En la pantalla de login tienes dos opciones:
   - **Pega tu token de usuario** y pulsa *Entrar*. Se valida contra la API y se
     guarda de forma segura; no tendrás que volver a introducirlo.
   - Pulsa **"Importar de Discord (cuenta propia)"** para leerlo automáticamente
     del Discord oficial instalado en tu PC.
3. Añade canales por **ID** en el panel izquierdo (*"Seguir canal"*).
4. Selecciona un canal para ver su historial y los mensajes en vivo; escribe y
   pulsa Enter o *"Enviar"*.

> 💡 Para obtener IDs en Discord: activa *Ajustes → Avanzado → Modo desarrollador*,
> y luego clic derecho sobre el servidor/canal → *Copiar ID*.

### 🎙️ Voz

- **Unirse**: doble clic en un canal 🔊 de tu lista, o botón **"🔊 Reunirse al
  último"** (recuerda el último canal usado). En modo avanzado puedes introducir
  los IDs de servidor y canal a mano.
- **Controles**: **Mic** y **Salida** para silenciar entrada/salida, y
  **"Salir de voz"** para colgar.
- **⚙ Ajustes de voz**: elige dispositivos, ajusta volúmenes y haz la **prueba
  de micrófono** antes de entrar. Si tus amigos oyen eco, deja activada la
  *cancelación de eco* (o mejor: usa auriculares); si te oyen bajo, el *control
  de ganancia automático* lo corrige.

> 🎤 **¿No te oyen?** Comprueba el permiso de micrófono de Windows:
> *Configuración → Privacidad y seguridad → Micrófono → "Permitir que las
> aplicaciones de escritorio accedan al micrófono"*. El instalador ofrece abrir
> esa página al terminar.

### 📂 Dónde se guardan los datos

- **Token**: Windows Credential Manager (servicio `discord-lite`), o
  `…\discord-lite\config\token.secret` con permisos restringidos como fallback.
- **Config**: `…\discord-lite\config\config.json` (canales seguidos, último
  canal, ajustes de voz).

Para cerrar sesión usa el botón **"Cerrar sesión"** (borra el token guardado).

## 🛡️ Falsos positivos de antivirus

Algunos antivirus (o SmartScreen de Windows) marcan el `.exe`/instalador como
sospechoso. **Es un falso positivo.** Pasa por dos motivos legítimos:

1. **El binario no está firmado** (no tengo un certificado Authenticode, que es de
   pago). Windows desconfía por defecto de los ejecutables de "editor desconocido".
2. **El importador de token** lee el almacenamiento local de Discord y lo descifra
   con DPAPI para sacar *tu propio* token. Ese comportamiento se parece al de un
   *info-stealer*, así que la heurística de los antivirus lo marca — aunque aquí el
   token **nunca sale de tu PC** (solo se usa para conectarte y se guarda cifrado).

El código es abierto: puedes revisarlo o **compilarlo tú mismo** (ver
[Compilar desde el código fuente](#️-compilar-desde-el-código-fuente)), y así el
binario es de tu máquina y no lo marca nada (por eso en la del autor no salta).

**Qué puedes hacer:**
- **Compílalo tú mismo** (la opción más limpia y verificable).
- **Añade una exclusión** en tu antivirus para la carpeta de instalación
  (`%LOCALAPPDATA%\Programs\discord-lite`).
- En SmartScreen: *Más información → Ejecutar de todas formas*.
- **Reporta el falso positivo** a tu antivirus (Microsoft Defender:
  *Seguridad de Windows → Protección antivirus → enviar muestra*); ayuda a que dejen
  de marcarlo.
- El arreglo de fondo sería **firmar** el binario con un certificado de código; está
  fuera de alcance para un proyecto personal sin coste.

## 📊 Estado del proyecto

**Fase 1 — Texto: ✅ funcional.**

- [x] Autenticación por token con almacenamiento seguro (keyring / Credential Manager).
- [x] Gateway en tiempo real: heartbeat con vigilancia de ACK, `MESSAGE_CREATE`,
      reconexión automática con RESUME/backoff.
- [x] REST: historial, envío, listar guilds/canales/DMs, abrir DM, rate limits (429).
- [x] GUI FLTK: canales seguidos, mensajes en vivo, estado de conexión, logout.

**Fase 2 — Voz: ✅ funcional (verificada en vivo con clientes oficiales).**

- [x] Señalización + Voice Gateway v8 (IDENTIFY → READY → SELECT PROTOCOL → SESSION DESCRIPTION).
- [x] UDP + IP discovery, cifrado de transporte XChaCha20-Poly1305 (rtpsize).
- [x] **E2EE DAVE** (MLS RFC 9420): handshake completo, claves por participante,
      rotación de epoch al entrar/salir gente y downgrade negociado.
- [x] Opus (encode/decode) y audio dúplex con cpal; mute/deaf en la GUI.
- [x] Panel de **Ajustes de voz**: dispositivos (con cambio en caliente),
      volúmenes, prueba de mic, sensibilidad, anti-eco, supresión de ruido y AGC.
- [ ] Reconexión automática del Voice Gateway (hoy hace un único intento).

> Diseño y alcance detallados: [`spec.md`](spec.md) · [`plan.md`](plan.md) · [`tasks.md`](tasks.md)

## 🛠️ Compilar desde el código fuente

<details>
<summary><b>Requisitos e instrucciones (Windows)</b> — solo si no quieres usar el instalador de Releases</summary>

El proyecto usa el **toolchain GNU de Rust** (no requiere Visual Studio):

1. **Rust (toolchain GNU)** — `stable-x86_64-pc-windows-gnu`.
   Instalado con `rustup-init.exe --default-host x86_64-pc-windows-gnu`.
2. **mingw-w64** (gcc/g++/dlltool/as) — WinLibs (POSIX, MSVCRT):
   `winget install BrechtSanders.WinLibs.POSIX.MSVCRT`
   Necesario para `dlltool` (import libs de `windows-sys`) y para compilar FLTK.
3. **CMake** — `winget install Kitware.CMake` (lo usa `fltk` para construir FLTK).
4. **libopus** (para la voz) — `audiopus_sys` no puede construir Opus con el
   toolchain GNU. Por eso se compiló **libopus.a** con mingw y se enlaza desde
   `thirdparty/opus-lib/`, configurado vía `.cargo/config.toml`
   (`OPUS_LIB_DIR`, `OPUS_STATIC`, `OPUS_NO_PKG`). Para regenerarla: descargar
   Opus 1.5.x, `cmake -G "MinGW Makefiles" -DOPUS_BUILD_SHARED_LIBRARY=OFF`,
   `cmake --build`, y copiar `libopus.a` a `thirdparty/opus-lib/`.

> TLS: se usa **native-tls = SChannel** (Windows), por lo que **no** hace falta
> OpenSSL ni rustls/ring.

```powershell
# Debug (con consola para ver logs)
cargo build

# Release (sin consola, optimizado para tamaño/RAM → el .exe de doble clic)
cargo build --release
# Resultado: target\release\discord-lite.exe

# Instalador (requiere Inno Setup 6: winget install JRSoftware.InnoSetup)
& "C:\Program Files (x86)\Inno Setup 6\ISCC.exe" installer\discord-lite.iss
# Resultado: dist\discord-lite-setup-<versión>.exe
```

El perfil `release` usa `opt-level="z"`, `lto`, `strip` y `panic="abort"`, y marca
la app como subsistema *windows* (sin ventana de consola al hacer doble clic).

El icono de la app (`icon.png` → `icon.ico`) se **incrusta en el `.exe`** mediante
`build.rs` (compilado con `windres`). El ejecutable es **autónomo** (solo depende
de DLLs del sistema de Windows).

</details>

## 🧩 Estructura del código

| Módulo | Responsabilidad |
|--------|-----------------|
| `model` | Tipos serde (REST + Gateway), tolerantes a cambios de API. |
| `auth` | Token seguro (keyring + fallback), redacción en logs. |
| `config` | Preferencias persistentes (no secretas), incl. ajustes de voz. |
| `rest` | Cliente REST (historial, envío, listas, rate limits). |
| `gateway` | WebSocket en tiempo real (heartbeat, dispatch, reconexión). |
| `voice` / `dave` | Voz: Voice Gateway, UDP, cifrado de transporte y E2EE (MLS), Opus. |
| `audio` | Dispositivos y procesado de mic (anti-eco, ruido, AGC) + prueba de mic. |
| `net` | Orquestador async: REST + Gateway + comandos de la UI. |
| `state` | `Command`/`AppEvent` y estado central de la app. |
| `ui` | GUI FLTK (login, ventana principal, Ajustes de voz). |

## 📜 Licencia

MIT OR Apache-2.0
