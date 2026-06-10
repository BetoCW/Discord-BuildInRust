# discord-lite

Cliente de Discord **nativo y ultraligero** para uso personal, escrito en Rust.
Reemplaza al cliente oficial (Electron, ~300 MB) por algo que **arranca en ~22 MB
de RAM** y pesa **~3 MB** en un único ejecutable de doble clic.

> Diseño y alcance: ver [`spec.md`](spec.md), [`plan.md`](plan.md) y
> [`tasks.md`](tasks.md).

## Estado actual

**Fase 1 — Texto: funcional.**

- [x] Autenticación por **token de usuario** con almacenamiento seguro
      (Windows Credential Manager vía `keyring`; fallback a archivo restringido).
- [x] Pantalla de **login** con validación del token contra la API.
- [x] **Gateway** en tiempo real: HELLO → IDENTIFY, heartbeat con vigilancia de
      ACK, `MESSAGE_CREATE`, y **reconexión automática** con RESUME/backoff.
- [x] **REST**: historial de canal/DM, envío de mensajes, listar guilds/canales/DMs,
      abrir DM, con manejo de **rate limits (429)**.
- [x] **GUI FLTK**: lista de canales seguidos, vista de mensajes en vivo, caja de
      envío, indicador de estado de conexión, alta de canales y logout.
- [x] **Config** persistente (canales seguidos, último canal).

**Fase 2 — Voz: implementada, pendiente de prueba en vivo.**

- [x] Señalización (Voice State Update op 4) y captura de
      `VOICE_STATE_UPDATE`/`VOICE_SERVER_UPDATE`.
- [x] **Voice Gateway** (WebSocket aparte): IDENTIFY → READY → SELECT PROTOCOL →
      SESSION DESCRIPTION, con heartbeat propio.
- [x] **UDP + IP discovery** y cifrado **XChaCha20-Poly1305 (rtpsize)**.
- [x] **Opus** (encode/decode) y audio dúplex con **cpal** (captura + reproducción).
- [x] Controles en la GUI: unirse/salir, **mute** y **deaf**.
- [ ] **Verificación en vivo** con un canal de voz real y un segundo participante.
- [ ] Reconexión automática del Voice Gateway (v1 hace un único intento).

> ⚠️ La voz con token de usuario es la parte de **mayor riesgo de baneo**
> (self-bot). Úsala con criterio.

## ⚠️ Aviso importante (ToS)

Usar un cliente propio con un **token de usuario** va contra los Términos de
Servicio de Discord (se considera *self-bot*) y **puede causar el baneo de la
cuenta**. Este proyecto es para **uso personal, de bajo volumen y a ritmo
humano**. El token da acceso total a la cuenta: trátalo como secreto crítico.

## Requisitos de compilación (Windows)

El proyecto usa el **toolchain GNU de Rust** (no requiere Visual Studio). Para
compilarlo se instaló:

1. **Rust (toolchain GNU)** — `stable-x86_64-pc-windows-gnu`.
   Instalado con `rustup-init.exe --default-host x86_64-pc-windows-gnu`.
2. **mingw-w64** (gcc/g++/dlltool/as) — WinLibs (POSIX, MSVCRT):
   `winget install BrechtSanders.WinLibs.POSIX.MSVCRT`
   Necesario para `dlltool` (import libs de `windows-sys`) y para compilar FLTK.
3. **CMake** — `winget install Kitware.CMake` (lo usa `fltk` para construir FLTK).
4. **libopus** (para la voz) — `audiopus_sys` no puede construir Opus con el
   toolchain GNU (necesita autotools/sh). Por eso se compiló **libopus.a** con
   mingw y se enlaza desde `thirdparty/opus-lib/`, configurado vía
   `.cargo/config.toml` (`OPUS_LIB_DIR`, `OPUS_STATIC`, `OPUS_NO_PKG`).
   Para regenerarla: descargar Opus 1.5.x, `cmake -G "MinGW Makefiles"
   -DOPUS_BUILD_SHARED_LIBRARY=OFF`, `cmake --build`, y copiar `libopus.a` a
   `thirdparty/opus-lib/`.

> TLS: se usa **native-tls = SChannel** (Windows), por lo que **no** hace falta
> OpenSSL ni rustls/ring (que requerirían más herramientas de C).

Asegúrate de que estén en el `PATH` (el instalador de Rust y winget normalmente
lo hacen). El `bin` de mingw es algo como:
`%LOCALAPPDATA%\Microsoft\WinGet\Packages\BrechtSanders.WinLibs.POSIX.MSVCRT_*\mingw64\bin`

## Compilar

```powershell
# Debug (con consola para ver logs)
cargo build

# Release (sin consola, optimizado para tamaño/RAM → el .exe de doble clic)
cargo build --release
# Resultado: target\release\discord-lite.exe   (~3 MB)
```

El perfil `release` usa `opt-level="z"`, `lto`, `strip` y `panic="abort"`, y marca
la app como subsistema *windows* (sin ventana de consola al hacer doble clic).

## Lanzador en el escritorio

El icono de la app (`icon.png` → `icon.ico`) se **incrusta en el `.exe`** mediante
`build.rs` (compilado con `windres`). El ejecutable es **autónomo** (solo depende
de DLLs del sistema de Windows).

Para (re)crear el acceso directo del escritorio: se copia el release a
`dist\discord-lite.exe` y se genera `Discord Lite.lnk` en el escritorio apuntando
ahí, con el icono. (Hecho con un script PowerShell de `WScript.Shell`.)

## Primer uso

1. Ejecuta `discord-lite.exe` (doble clic).
2. En la pantalla de login, **pega tu token de usuario** y pulsa *Entrar*.
   - Se valida contra la API y, si es correcto, se **guarda de forma segura**;
     no tendrás que volver a introducirlo.
   - Alternativa para la primera vez: define `DISCORD_TOKEN` en el entorno antes
     de abrir la app y se guardará automáticamente.
3. Añade canales por **ID** en el panel izquierdo ("Seguir canal").
4. Selecciona un canal para ver su historial y los mensajes en vivo; escribe y
   pulsa Enter o "Enviar".

### Voz

En el panel izquierdo, sección **Voz**: introduce el **ID del servidor (guild)** y
el **ID del canal de voz**, y pulsa **"Unirse a voz"**. Usa **Mic** y **Salida**
para silenciar entrada/salida, y **"Salir de voz"** para colgar.

> Para obtener IDs en Discord: activa *Ajustes → Avanzado → Modo desarrollador*,
> y luego clic derecho sobre el servidor/canal → *Copiar ID*.

### Importar el token automáticamente

En vez de pegar el token, puedes pulsar **"Importar de Discord (cuenta propia)"**:
lee el token de tu Discord oficial instalado (descifrando con tu sesión de
Windows), lo valida y entra. Diagnóstico sin exponer el token:
`discord-lite.exe --check-import`.

## Dónde se guardan los datos

- **Token**: Windows Credential Manager (servicio `discord-lite`), o
  `…\discord-lite\config\token.secret` con permisos restringidos como fallback.
- **Config**: `…\discord-lite\config\config.json` (canales seguidos, último canal).
  Rutas exactas según `directories` (`%APPDATA%` en Windows).

Para cerrar sesión usa el botón **"Cerrar sesión"** (borra el token).

## Empaquetado por SO

- **Windows** (objetivo principal): `cargo build --release` → `discord-lite.exe`
  autocontenido (TLS por SChannel del sistema, FLTK enlazado estático).
- **Linux** (best-effort, pendiente de probar): `cargo build --release` → ELF;
  empaquetar como AppImage. La voz (fase 2) requerirá empaquetar `libopus`.

## Estructura del código

| Módulo | Responsabilidad |
|--------|-----------------|
| `model` | Tipos serde (REST + Gateway), tolerantes a cambios de API. |
| `auth` | Token seguro (keyring + fallback), redacción en logs. |
| `config` | Preferencias persistentes (no secretas). |
| `rest` | Cliente REST (historial, envío, listas, rate limits). |
| `gateway` | WebSocket en tiempo real (heartbeat, dispatch, reconexión). |
| `net` | Orquestador async: REST + Gateway + comandos de la UI. |
| `state` | `Command`/`AppEvent` y estado central de la app. |
| `ui` | GUI FLTK (login + ventana principal). |
