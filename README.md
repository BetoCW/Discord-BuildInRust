<div align="center">

<img src="IconoM.png" width="160" alt="Logo de Discord Lite"/>

# Discord Lite

**Cliente de Discord nativo y ultraligero, escrito en Rust.**

Texto + voz en un único `.exe` de doble clic: **~3 MB** de tamaño y **~22 MB de RAM**,
frente a los ~300 MB del cliente oficial (Electron).

[![Release](https://img.shields.io/github/v/release/BetoCW/Discord-BuildInRust?label=Release&color=brightgreen)](https://github.com/BetoCW/Discord-BuildInRust/releases/latest)
[![Descargas](https://img.shields.io/github/downloads/BetoCW/Discord-BuildInRust/total?label=Descargas&color=blue)](https://github.com/BetoCW/Discord-BuildInRust/releases)
[![Rust](https://img.shields.io/badge/Rust-stable--gnu-orange?logo=rust)](https://www.rust-lang.org/)
[![Licencia](https://img.shields.io/badge/Licencia-MIT%20%7C%20Apache--2.0-lightgrey)](#licencia)

### [⬇️ Descargar la última versión](https://github.com/BetoCW/Discord-BuildInRust/releases/latest)

*Descarga `discord-lite.exe`, haz doble clic y listo. No requiere instalación.*

</div>

---

## ✨ Características

| | |
|---|---|
| 💬 **Texto en tiempo real** | Historial, envío de mensajes, DMs y canales de servidores, con reconexión automática y manejo de rate limits. |
| 🎙️ **Voz** | Unirse a canales de voz con audio dúplex (Opus + cifrado XChaCha20-Poly1305), controles de mute/deaf. |
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

1. Descarga `discord-lite.exe` desde [Releases](https://github.com/BetoCW/Discord-BuildInRust/releases/latest) y ábrelo con doble clic.
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

En el panel izquierdo, sección **Voz**: introduce el **ID del servidor (guild)** y
el **ID del canal de voz**, y pulsa **"Unirse a voz"**. Usa **Mic** y **Salida**
para silenciar entrada/salida, y **"Salir de voz"** para colgar.

### 📂 Dónde se guardan los datos

- **Token**: Windows Credential Manager (servicio `discord-lite`), o
  `…\discord-lite\config\token.secret` con permisos restringidos como fallback.
- **Config**: `…\discord-lite\config\config.json` (canales seguidos, último canal).

Para cerrar sesión usa el botón **"Cerrar sesión"** (borra el token guardado).

## 📊 Estado del proyecto

**Fase 1 — Texto: ✅ funcional.**

- [x] Autenticación por token con almacenamiento seguro (keyring / Credential Manager).
- [x] Gateway en tiempo real: heartbeat con vigilancia de ACK, `MESSAGE_CREATE`,
      reconexión automática con RESUME/backoff.
- [x] REST: historial, envío, listar guilds/canales/DMs, abrir DM, rate limits (429).
- [x] GUI FLTK: canales seguidos, mensajes en vivo, estado de conexión, logout.

**Fase 2 — Voz: ⚙️ implementada, pendiente de prueba en vivo.**

- [x] Señalización + Voice Gateway (IDENTIFY → READY → SELECT PROTOCOL → SESSION DESCRIPTION).
- [x] UDP + IP discovery, cifrado XChaCha20-Poly1305 (rtpsize).
- [x] Opus (encode/decode) y audio dúplex con cpal; controles mute/deaf en la GUI.
- [ ] Verificación en vivo con un canal de voz real y un segundo participante.
- [ ] Reconexión automática del Voice Gateway (v1 hace un único intento).

> Diseño y alcance detallados: [`spec.md`](spec.md) · [`plan.md`](plan.md) · [`tasks.md`](tasks.md)

## 🛠️ Compilar desde el código fuente

<details>
<summary><b>Requisitos e instrucciones (Windows)</b> — solo si no quieres usar el .exe de Releases</summary>

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
# Resultado: target\release\discord-lite.exe   (~3 MB)
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
| `config` | Preferencias persistentes (no secretas). |
| `rest` | Cliente REST (historial, envío, listas, rate limits). |
| `gateway` | WebSocket en tiempo real (heartbeat, dispatch, reconexión). |
| `voice` / `dave` | Voz: Voice Gateway, UDP, cifrado, Opus, audio. |
| `net` | Orquestador async: REST + Gateway + comandos de la UI. |
| `state` | `Command`/`AppEvent` y estado central de la app. |
| `ui` | GUI FLTK (login + ventana principal). |

## 📜 Licencia

MIT OR Apache-2.0
