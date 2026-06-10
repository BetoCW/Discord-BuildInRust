//! Capa de interfaz (FLTK). Corre en el hilo principal; se comunica con el
//! mundo async (tokio) solo por canales:
//!   - `ui → net`  : `Command` (unbounded mpsc).
//!   - `net → ui`  : `AppEvent` (mpsc) reenviado al canal de FLTK que despierta
//!                   el bucle de eventos de la GUI.
//! No hay `await` en este hilo (RT-6).

use crate::config::{Config, VoiceTarget};
use crate::model::{Channel, Message};
use crate::state::{AppEvent, AppState, Command, ConnState};
use anyhow::Result;
use fltk::{enums::*, prelude::*, *};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use tokio::runtime::{Handle, Runtime};
use tokio::sync::mpsc;

/// Estado que vive en el hilo de la UI.
struct UiState {
    app: AppState,
    active: Option<String>,
    cfg: Config,
    /// Filtro de búsqueda actual de la lista de mensajes directos.
    dm_filter: String,
}

/// Une a un canal de voz y lo recuerda como "último canal" (lo persiste en
/// config). Lo usan el botón manual, el doble clic y el botón de reunión.
fn join_voice_remember(
    ui: &Rc<RefCell<UiState>>,
    cmd_tx: &mpsc::UnboundedSender<Command>,
    guild_id: String,
    channel_id: String,
) {
    {
        let mut s = ui.borrow_mut();
        s.cfg.last_voice = Some(VoiceTarget {
            guild_id: guild_id.clone(),
            channel_id: channel_id.clone(),
        });
        let _ = s.cfg.save();
    }
    let _ = cmd_tx.send(Command::JoinVoice {
        guild_id,
        channel_id,
    });
}

/// Punto de entrada de la GUI. Bloquea hasta que se cierra la ventana.
pub fn run(rt: Runtime) -> Result<()> {
    let handle = rt.handle().clone();
    let app = app::App::default().with_scheme(app::Scheme::Gtk);
    // Tema oscuro consistente (estilo Discord): fondos oscuros, texto claro. Se
    // fija ANTES de crear ventanas para que login y ventana principal lo hereden.
    app::background(43, 45, 49); // cajas/botones   #2b2d31
    app::background2(30, 31, 34); // inputs/listas/texto  #1e1f22
    app::foreground(223, 226, 230); // texto/labels  #dfe2e6
    app::set_visible_focus(false); // sin recuadro punteado de foco

    // --- Token: almacén seguro → variable de entorno → pantalla de login ---
    let mut token = crate::auth::load_token().ok().flatten();
    if token.is_none() {
        if let Ok(t) = std::env::var("DISCORD_TOKEN") {
            let t = t.trim().to_string();
            if !t.is_empty() {
                let _ = crate::auth::save_token(&t);
                token = Some(t);
            }
        }
    }
    let token = match token {
        Some(t) => t,
        None => match run_login(&app, &handle) {
            Some(t) => t,
            None => return Ok(()), // el usuario cerró el login
        },
    };

    let mut cfg = Config::load().unwrap_or_default();
    // Limpia entradas antiguas inválidas (nombres en vez de IDs numéricos).
    let before = cfg.followed_channels.len();
    cfg.followed_channels.retain(|c| is_snowflake(c));
    if cfg.last_channel.as_deref().is_some_and(|c| !is_snowflake(c)) {
        cfg.last_channel = None;
    }
    if cfg.followed_channels.len() != before {
        let _ = cfg.save();
        tracing::info!("purgadas {} entradas inválidas de canales", before - cfg.followed_channels.len());
    }
    let history_limit = cfg.history_limit;
    // Vuelca los ajustes de voz persistidos a las opciones en vivo del audio.
    crate::audio::apply_settings(&cfg.voice);

    // --- Canales entre UI y red --------------------------------------------
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel::<Command>();
    let (ev_tx, ev_rx) = mpsc::unbounded_channel::<AppEvent>();
    handle.spawn(crate::net::run(token, ev_tx, cmd_rx));

    // Puente: AppEvent (tokio) → canal de FLTK (despierta el bucle de la GUI).
    let (fl_s, fl_r) = app::channel::<AppEvent>();
    handle.spawn(async move {
        let mut ev_rx = ev_rx;
        while let Some(ev) = ev_rx.recv().await {
            fl_s.send(ev);
        }
    });

    // --- Construcción de la ventana principal ------------------------------
    let mut win = window::Window::default()
        .with_size(1000, 880)
        .with_label("discord-lite");
    if let Ok(icon) = image::PngImage::from_data(include_bytes!("../icon_window.png")) {
        win.set_icon(Some(icon));
    }

    let mut root = group::Flex::default_fill().column();
    let mut status = frame::Frame::default().with_label("conectando…");
    status.set_align(Align::Left | Align::Inside);
    status.set_frame(FrameType::FlatBox);
    status.set_color(Color::from_rgb(40, 42, 54));
    status.set_label_color(Color::White);
    root.fixed(&status, 26);

    let mut body = group::Flex::default().row();

    // Columna izquierda: barra lateral con DOS listas (canales seguidos y
    // mensajes directos) más los controles de voz. El orden de creación = orden
    // visual en el Flex; las dos listas (browsers) son las únicas flexibles.
    let mut left = group::Flex::default().column();

    // Sección: canales de servidor seguidos.
    let mut ch_hdr = frame::Frame::default().with_label("📡 Canales seguidos");
    style_header(&mut ch_hdr);
    let mut browser = browser::HoldBrowser::default();
    browser.set_tooltip("Clic: ver mensajes · Doble clic en un canal 🔊: unirse a voz");
    let mut new_ch = input::Input::default();
    new_ch.set_tooltip("Pega el ID de un canal o DM y pulsa «Seguir»");
    let mut follow_btn = button::Button::default().with_label("➕ Seguir canal");
    let mut unfollow_btn = button::Button::default().with_label("➖ Quitar seleccionado");

    // Sección: mensajes directos (DMs), cargados aparte de los canales.
    let mut dm_hdr = frame::Frame::default().with_label("✉ Mensajes directos");
    style_header(&mut dm_hdr);
    let mut dm_search = input::Input::default();
    dm_search.set_trigger(CallbackTrigger::Changed); // filtra al teclear
    dm_search.set_tooltip("🔎 Buscar una conversación por nombre");
    let mut dm_browser = browser::HoldBrowser::default();
    dm_browser.set_tooltip("Tus conversaciones privadas. Clic para abrir una.");
    let mut dm_refresh_btn = button::Button::default().with_label("↻ Actualizar DMs");

    // Sección: voz.
    let mut voice_sep = frame::Frame::default().with_label("🔊 Voz");
    style_header(&mut voice_sep);
    // Botón de un clic: se reúne al último canal de voz guardado, sin teclear IDs.
    let mut rejoin_btn = button::Button::default().with_label("🔊 Reunirse al último");
    rejoin_btn.set_tooltip("Une al último canal de voz usado (recuerda los IDs por ti)");
    rejoin_btn.set_color(Color::from_rgb(46, 80, 60));
    rejoin_btn.set_label_color(Color::White);
    let mut join_btn = button::Button::default().with_label("Unirse a voz");
    let mut leave_btn = button::Button::default().with_label("Salir de voz");
    let mut guild_in = input::Input::default();
    guild_in.set_tooltip("Avanzado: ID del servidor (guild) del canal de voz");
    let mut vchan_in = input::Input::default();
    vchan_in.set_tooltip("Avanzado: ID del canal de voz");
    // Prefill de los IDs con el último canal usado (así el botón manual también
    // funciona sin re-teclear y se ve a dónde apunta "Reunirse").
    if let Some(v) = &cfg.last_voice {
        guild_in.set_value(&v.guild_id);
        vchan_in.set_value(&v.channel_id);
    }
    let mut mute_btn = button::Button::default().with_label("🎙 Mic: ON");
    let mut deaf_btn = button::Button::default().with_label("🔊 Salida: ON");
    let mut voice_cfg_btn = button::Button::default().with_label("⚙ Ajustes de voz");
    voice_cfg_btn.set_tooltip("Micrófono, altavoces, volúmenes, prueba de mic y anti-eco");
    let mut voice_status = frame::Frame::default().with_label("voz: inactiva");
    voice_status.set_align(Align::Left | Align::Inside | Align::Wrap);
    voice_status.set_label_size(11);

    let mut info_btn = button::Button::default().with_label("ℹ Info");
    info_btn.set_tooltip("Mostrar u ocultar el panel de información lateral");
    let mut log_btn = button::Button::default().with_label("📋 Ocultar log");
    log_btn.set_tooltip("Mostrar u ocultar el panel de registro inferior");
    let mut logout_btn = button::Button::default().with_label("Cerrar sesión");

    left.fixed(&ch_hdr, 22);
    left.fixed(&new_ch, 26);
    left.fixed(&follow_btn, 26);
    left.fixed(&unfollow_btn, 24);
    left.fixed(&dm_hdr, 22);
    left.fixed(&dm_search, 24);
    left.fixed(&dm_refresh_btn, 24);
    left.fixed(&voice_sep, 22);
    left.fixed(&rejoin_btn, 28);
    left.fixed(&join_btn, 26);
    left.fixed(&leave_btn, 26);
    left.fixed(&guild_in, 24);
    left.fixed(&vchan_in, 24);
    left.fixed(&mute_btn, 26);
    left.fixed(&deaf_btn, 26);
    left.fixed(&voice_cfg_btn, 26);
    left.fixed(&voice_status, 32);
    left.fixed(&info_btn, 24);
    left.fixed(&log_btn, 24);
    left.fixed(&logout_btn, 26);
    left.end();
    body.fixed(&left, 260);

    // Acento de selección (azul-morado tipo Discord) para ambas listas.
    let accent = Color::from_rgb(88, 101, 242);
    browser.set_selection_color(accent);
    dm_browser.set_selection_color(accent);

    // Columna central: título de la conversación + mensajes + caja de envío.
    let mut right = group::Flex::default().column();
    let mut active_title = frame::Frame::default().with_label("Selecciona un canal o DM");
    active_title.set_align(Align::Left | Align::Inside);
    active_title.set_label_size(15);
    active_title.set_label_color(Color::from_rgb(235, 236, 240));
    right.fixed(&active_title, 26);
    let mut disp = text::TextDisplay::default();
    let buf = text::TextBuffer::default();
    disp.set_buffer(buf.clone());
    disp.wrap_mode(text::WrapMode::AtBounds, 0);
    disp.set_text_size(14);
    disp.set_color(Color::from_rgb(30, 31, 34));
    disp.set_text_color(Color::from_rgb(223, 226, 230));

    let mut send_row = group::Flex::default().row();
    let mut msg_in = input::Input::default();
    msg_in.set_trigger(CallbackTrigger::EnterKey);
    let mut send_btn = button::Button::default().with_label("Enviar ➤");
    send_btn.set_color(accent);
    send_btn.set_label_color(Color::White);
    send_row.fixed(&send_btn, 100);
    send_row.end();
    right.fixed(&send_row, 30);
    right.end();

    // Columna derecha: panel de información colapsable (oculto por defecto).
    let mut info = group::Flex::default().column();
    let mut info_hdr = frame::Frame::default().with_label("ℹ Información");
    style_header(&mut info_hdr);
    info.fixed(&info_hdr, 24);
    let mut info_disp = text::TextDisplay::default();
    let info_buf = text::TextBuffer::default();
    info_disp.set_buffer(info_buf.clone());
    info_disp.wrap_mode(text::WrapMode::AtBounds, 0);
    info_disp.set_text_size(12);
    info_disp.set_color(Color::from_rgb(30, 31, 34));
    info_disp.set_text_color(Color::from_rgb(200, 204, 210));
    info.end();
    body.fixed(&info, 0); // arranca oculto; el botón ℹ Info lo despliega
    info.hide();

    body.end();

    // Panel de logs/errores dentro de la app (todo lo que va a la terminal).
    let mut logdisp = text::TextDisplay::default();
    let logbuf = text::TextBuffer::default();
    logdisp.set_buffer(logbuf.clone());
    logdisp.set_text_size(11);
    logdisp.set_text_font(Font::Courier);
    logdisp.set_color(Color::from_rgb(24, 24, 28));
    logdisp.set_text_color(Color::from_rgb(180, 220, 180));
    root.fixed(&logdisp, 170);

    root.end();
    win.end();
    win.make_resizable(true);
    win.show();

    // Refresco periódico del panel de logs desde el buffer global.
    {
        let mut logbuf = logbuf.clone();
        let mut logdisp = logdisp.clone();
        let mut last_len = 0usize;
        app::add_timeout3(0.4, move |handle| {
            let n = crate::applog::len();
            if n != last_len {
                last_len = n;
                logbuf.set_text(&crate::applog::snapshot());
                let lines = logbuf.count_lines(0, logbuf.length());
                logdisp.scroll(lines, 0);
            }
            app::repeat_timeout3(0.4, handle);
        });
    }

    // Botón para ocultar/mostrar el panel de log: alterna su visibilidad y su
    // altura (0 cuando se oculta) para que el área de mensajes ocupe el hueco.
    {
        let mut logdisp = logdisp.clone();
        let mut root = root.clone();
        let shown = Rc::new(Cell::new(true));
        log_btn.set_callback(move |b| {
            let now = !shown.get();
            shown.set(now);
            if now {
                logdisp.show();
                root.fixed(&logdisp, 170);
                b.set_label("📋 Ocultar log");
            } else {
                logdisp.hide();
                root.fixed(&logdisp, 0);
                b.set_label("📋 Mostrar log");
            }
            root.recalc();
            root.redraw();
        });
    }

    // --- Estado compartido de la UI ----------------------------------------
    let ui = Rc::new(RefCell::new(UiState {
        app: AppState::new(history_limit),
        active: None,
        cfg,
        dm_filter: String::new(),
    }));

    // Rellena la lista con los canales ya seguidos y pide su historial + nombre.
    {
        let s = ui.borrow();
        for ch in &s.cfg.followed_channels {
            browser.add(&channel_label(ch));
        }
        for ch in &s.cfg.followed_channels {
            let _ = cmd_tx.send(Command::LoadHistory {
                channel_id: ch.clone(),
            });
            let _ = cmd_tx.send(Command::ResolveChannel {
                channel_id: ch.clone(),
            });
        }
        // Selección inicial.
        let initial = s
            .cfg
            .last_channel
            .clone()
            .or_else(|| s.cfg.followed_channels.first().cloned());
        drop(s);
        if let Some(active) = initial {
            if let Some(pos) = ui
                .borrow()
                .cfg
                .followed_channels
                .iter()
                .position(|c| c == &active)
            {
                browser.select((pos + 1) as i32);
            }
            ui.borrow_mut().active = Some(active);
        }
    }
    update_active_views(&ui.borrow(), &mut active_title, &mut info_buf.clone());

    // Carga la lista de mensajes directos (DMs) al arrancar.
    let _ = cmd_tx.send(Command::LoadDms);

    // --- Callbacks ----------------------------------------------------------

    // Buscador de DMs: filtra la lista por nombre al teclear.
    {
        let ui = ui.clone();
        let mut dm_browser = dm_browser.clone();
        dm_search.set_callback(move |i| {
            ui.borrow_mut().dm_filter = i.value();
            rebuild_dm_browser(&ui.borrow(), &mut dm_browser);
        });
    }

    // Botón ℹ Info: despliega/oculta el panel lateral de información.
    {
        let ui = ui.clone();
        let mut info = info.clone();
        let mut body = body.clone();
        let mut info_buf = info_buf.clone();
        let shown = Rc::new(Cell::new(false));
        info_btn.set_callback(move |b| {
            let now = !shown.get();
            shown.set(now);
            if now {
                info_buf.set_text(&info_text(&ui.borrow()));
                info.show();
                body.fixed(&info, 250);
                b.set_label("ℹ Ocultar");
            } else {
                info.hide();
                body.fixed(&info, 0);
                b.set_label("ℹ Info");
            }
            body.recalc();
            body.redraw();
        });
    }

    // Seguir un canal nuevo.
    {
        let ui = ui.clone();
        let cmd_tx = cmd_tx.clone();
        let mut browser = browser.clone();
        let mut new_ch = new_ch.clone();
        let mut status = status.clone();
        follow_btn.set_callback(move |_| {
            let id = new_ch.value().trim().to_string();
            if id.is_empty() {
                return;
            }
            if !is_snowflake(&id) {
                status.set_label(
                    "⚠ Eso no es un ID. Usa el ID numérico del canal (Modo desarrollador → Copiar ID).",
                );
                return;
            }
            {
                let mut s = ui.borrow_mut();
                if s.cfg.followed_channels.iter().any(|c| c == &id) {
                    return;
                }
                s.cfg.follow(id.clone());
                let _ = s.cfg.save();
            }
            browser.add(&channel_label(&id));
            new_ch.set_value("");
            let _ = cmd_tx.send(Command::LoadHistory {
                channel_id: id.clone(),
            });
            let _ = cmd_tx.send(Command::ResolveChannel { channel_id: id });
        });
    }

    // Quitar el canal seleccionado de la lista.
    {
        let ui = ui.clone();
        let mut browser2 = browser.clone();
        let mut buf = buf.clone();
        let mut disp = disp.clone();
        unfollow_btn.set_callback(move |_| {
            let line = browser2.value();
            if line <= 0 {
                return;
            }
            let id = {
                let s = ui.borrow();
                s.cfg.followed_channels.get((line - 1) as usize).cloned()
            };
            if let Some(id) = id {
                {
                    let mut s = ui.borrow_mut();
                    s.cfg.unfollow(&id);
                    let _ = s.cfg.save();
                    if s.active.as_deref() == Some(&id) {
                        s.active = None;
                    }
                }
                rebuild_browser(&ui.borrow(), &mut browser2);
                refresh_messages(&ui.borrow(), &mut buf, &mut disp);
            }
        });
    }

    // Seleccionar un canal de la lista.
    {
        let ui = ui.clone();
        let cmd_tx = cmd_tx.clone();
        let mut buf = buf.clone();
        let mut disp = disp.clone();
        let mut dm_browser = dm_browser.clone();
        let mut active_title = active_title.clone();
        let mut info_buf = info_buf.clone();
        browser.set_callback(move |b| {
            let line = b.value();
            if line <= 0 {
                return;
            }
            let id = {
                let s = ui.borrow();
                s.cfg.followed_channels.get((line - 1) as usize).cloned()
            };
            if let Some(id) = id {
                {
                    let mut s = ui.borrow_mut();
                    s.active = Some(id.clone());
                    s.cfg.last_channel = Some(id.clone());
                    let _ = s.cfg.save();
                }
                dm_browser.deselect(0); // solo una lista resaltada a la vez
                update_active_views(&ui.borrow(), &mut active_title, &mut info_buf);
                refresh_messages(&ui.borrow(), &mut buf, &mut disp);
                let _ = cmd_tx.send(Command::LoadHistory { channel_id: id });
            }
        });
    }

    // Seleccionar un mensaje directo (DM) de su lista (índice según el filtro).
    {
        let ui = ui.clone();
        let cmd_tx = cmd_tx.clone();
        let mut buf = buf.clone();
        let mut disp = disp.clone();
        let mut browser = browser.clone();
        let mut active_title = active_title.clone();
        let mut info_buf = info_buf.clone();
        dm_browser.set_callback(move |b| {
            let line = b.value();
            if line <= 0 {
                return;
            }
            let id = {
                let s = ui.borrow();
                filtered_dms(&s).get((line - 1) as usize).map(|c| c.id.clone())
            };
            if let Some(id) = id {
                {
                    let mut s = ui.borrow_mut();
                    s.active = Some(id.clone());
                    s.cfg.last_channel = Some(id.clone());
                    let _ = s.cfg.save();
                }
                browser.deselect(0); // limpia la selección de la lista de canales
                update_active_views(&ui.borrow(), &mut active_title, &mut info_buf);
                refresh_messages(&ui.borrow(), &mut buf, &mut disp);
                let _ = cmd_tx.send(Command::LoadHistory { channel_id: id });
            }
        });
    }

    // Botón «Actualizar DMs»: recarga la lista de mensajes directos.
    {
        let cmd_tx = cmd_tx.clone();
        dm_refresh_btn.set_callback(move |_| {
            let _ = cmd_tx.send(Command::LoadDms);
        });
    }

    // Doble clic en un canal de voz (🔊) → unirse directamente, sin teclear IDs.
    // Toma el guild_id del canal ya resuelto (`ResolveChannel` al iniciar).
    {
        let ui = ui.clone();
        let cmd_tx = cmd_tx.clone();
        let mut voice_status = voice_status.clone();
        browser.handle(move |b, ev| {
            if ev != enums::Event::Released || !app::event_clicks() {
                return false; // deja que el browser haga la selección normal
            }
            let line = b.value();
            if line <= 0 {
                return false;
            }
            let id = match ui
                .borrow()
                .cfg
                .followed_channels
                .get((line - 1) as usize)
                .cloned()
            {
                Some(id) => id,
                None => return false,
            };
            let info = ui
                .borrow()
                .app
                .channels
                .iter()
                .rev()
                .find(|c| c.id == id)
                .map(|c| (c.is_voice(), c.guild_id.clone()));
            match info {
                Some((true, Some(guild_id))) => {
                    voice_status.set_label("voz: conectando…");
                    join_voice_remember(&ui, &cmd_tx, guild_id, id);
                }
                Some((true, None)) => {
                    voice_status.set_label("voz: resolviendo el canal; reintenta en 1 s");
                }
                Some((false, _)) => {
                    voice_status.set_label("voz: ese canal no es de voz (🔊)");
                }
                None => {
                    voice_status.set_label("voz: canal aún sin resolver; reintenta");
                }
            }
            true // consumimos el doble clic
        });
    }

    // Enviar mensaje (botón o Enter).
    let send = {
        let ui = ui.clone();
        let cmd_tx = cmd_tx.clone();
        move |inp: &mut input::Input| {
            let content = inp.value();
            let content = content.trim();
            if content.is_empty() {
                return;
            }
            let active = ui.borrow().active.clone();
            if let Some(channel_id) = active {
                let _ = cmd_tx.send(Command::SendMessage {
                    channel_id,
                    content: content.to_string(),
                });
                inp.set_value("");
            }
        }
    };
    {
        let mut send2 = send.clone();
        let mut msg_in2 = msg_in.clone();
        send_btn.set_callback(move |_| send2(&mut msg_in2));
    }
    {
        let mut send3 = send.clone();
        msg_in.set_callback(move |i| send3(i));
    }

    // Cerrar sesión: borra token y cierra.
    {
        let cmd_tx = cmd_tx.clone();
        logout_btn.set_callback(move |_| {
            let _ = cmd_tx.send(Command::Logout);
            app::quit();
        });
    }

    // --- Voz ---------------------------------------------------------------
    let muted = Rc::new(Cell::new(false));
    let deafened = Rc::new(Cell::new(false));

    {
        let ui = ui.clone();
        let cmd_tx = cmd_tx.clone();
        let guild_in = guild_in.clone();
        let vchan_in = vchan_in.clone();
        let mut voice_status = voice_status.clone();
        join_btn.set_callback(move |_| {
            let g = guild_in.value().trim().to_string();
            let c = vchan_in.value().trim().to_string();
            if g.is_empty() || c.is_empty() {
                voice_status.set_label("voz: pon ID de servidor y de canal de voz");
                return;
            }
            if !is_snowflake(&g) || !is_snowflake(&c) {
                voice_status
                    .set_label("voz: usa IDs numéricos (Modo desarrollador → Copiar ID)");
                return;
            }
            voice_status.set_label("voz: conectando…");
            join_voice_remember(&ui, &cmd_tx, g, c);
        });
    }
    // Botón de un clic: reunirse al último canal de voz guardado.
    {
        let ui = ui.clone();
        let cmd_tx = cmd_tx.clone();
        let mut voice_status = voice_status.clone();
        rejoin_btn.set_callback(move |_| {
            let target = ui.borrow().cfg.last_voice.clone();
            match target {
                Some(v) => {
                    voice_status.set_label("voz: reuniéndote al último canal…");
                    join_voice_remember(&ui, &cmd_tx, v.guild_id, v.channel_id);
                }
                None => {
                    voice_status
                        .set_label("voz: aún no hay último canal; únete una vez y se recordará");
                }
            }
        });
    }
    {
        let cmd_tx = cmd_tx.clone();
        let guild_in = guild_in.clone();
        leave_btn.set_callback(move |_| {
            let g = guild_in.value().trim().to_string();
            if g.is_empty() {
                return;
            }
            let _ = cmd_tx.send(Command::LeaveVoice { guild_id: g });
        });
    }
    {
        let cmd_tx = cmd_tx.clone();
        let muted = muted.clone();
        mute_btn.set_callback(move |b| {
            let m = !muted.get();
            muted.set(m);
            b.set_label(if m { "🎙 Mic: OFF" } else { "🎙 Mic: ON" });
            let _ = cmd_tx.send(Command::VoiceMute(m));
        });
    }
    {
        let cmd_tx = cmd_tx.clone();
        let deafened = deafened.clone();
        deaf_btn.set_callback(move |b| {
            let d = !deafened.get();
            deafened.set(d);
            b.set_label(if d { "🔊 Salida: OFF" } else { "🔊 Salida: ON" });
            let _ = cmd_tx.send(Command::VoiceDeaf(d));
        });
    }
    // Ventana de «Ajustes de voz» (estilo Discord). Guard para no abrir dos.
    {
        let ui = ui.clone();
        let open = Rc::new(Cell::new(false));
        voice_cfg_btn.set_callback(move |_| {
            if open.get() {
                return;
            }
            open.set(true);
            open_voice_settings(&ui, open.clone());
        });
    }

    // --- Bucle principal de la GUI -----------------------------------------
    // Auto-unión a voz para pruebas: DISCORD_LITE_AUTOJOIN="guild:channel".
    if let Ok(spec) = std::env::var("DISCORD_LITE_AUTOJOIN") {
        if let Some((g, c)) = spec.split_once(':') {
            tracing::info!("autojoin de voz: guild {g}, canal {c}");
            let _ = cmd_tx.send(Command::JoinVoice {
                guild_id: g.trim().to_string(),
                channel_id: c.trim().to_string(),
            });
        }
    }

    while app.wait() {
        if let Some(ev) = fl_r.recv() {
            handle_event(
                ev,
                &ui,
                &mut buf.clone(),
                &mut disp,
                &mut status,
                &mut voice_status,
                &mut browser,
                &mut dm_browser,
                &mut active_title,
                &mut info_buf.clone(),
            );
        }
    }

    // Persistir preferencias al salir.
    let _ = ui.borrow().cfg.save();
    Ok(())
}

/// Aplica un `AppEvent` al estado y refresca los widgets afectados.
#[allow(clippy::too_many_arguments)]
fn handle_event(
    ev: AppEvent,
    ui: &Rc<RefCell<UiState>>,
    buf: &mut text::TextBuffer,
    disp: &mut text::TextDisplay,
    status: &mut frame::Frame,
    voice_status: &mut frame::Frame,
    browser: &mut browser::HoldBrowser,
    dm_browser: &mut browser::HoldBrowser,
    active_title: &mut frame::Frame,
    info_buf: &mut text::TextBuffer,
) {
    let active = ui.borrow().active.clone();
    let (affects_active, error_text) = match &ev {
        AppEvent::NewMessage(m) | AppEvent::Sent(m) => (Some(m.channel_id.clone()) == active, None),
        AppEvent::History { channel_id, .. } => (Some(channel_id.clone()) == active, None),
        AppEvent::Error(e) => (false, Some(e.clone())),
        _ => (false, None),
    };
    let voice_text = matches!(ev, AppEvent::VoiceUpdate { .. });
    let channel_info = matches!(ev, AppEvent::ChannelInfo(_));
    let dms_loaded = matches!(ev, AppEvent::Dms(_));

    {
        let mut s = ui.borrow_mut();
        s.app.apply(ev);
    }

    if channel_info {
        rebuild_browser(&ui.borrow(), browser);
    }
    if dms_loaded {
        rebuild_dm_browser(&ui.borrow(), dm_browser);
    }
    // El nombre/los datos de la conversación activa pueden llegar tarde (resolución
    // de canal, carga de DMs o nuevos mensajes); refresca título e info entonces.
    if channel_info || dms_loaded || affects_active {
        update_active_views(&ui.borrow(), active_title, info_buf);
    }

    if voice_text {
        let s = ui.borrow();
        if let Some(txt) = &s.app.voice_status {
            voice_status.set_label(txt);
            voice_status.set_label_color(if s.app.voice_connected {
                Color::from_rgb(60, 160, 60)
            } else {
                Color::from_rgb(150, 150, 150)
            });
        }
    }

    // Etiqueta de estado: conexión + usuario, o el último error.
    {
        let s = ui.borrow();
        let label = if let Some(err) = &error_text {
            format!("⚠ {err}")
        } else {
            let name = s
                .app
                .me
                .as_ref()
                .map(|u| u.display_name().to_string())
                .unwrap_or_default();
            match (s.app.conn, name.is_empty()) {
                (c, true) => c.label().to_string(),
                (c, false) => format!("{} — {}", conn_icon(c), name),
            }
        };
        status.set_label(&label);
    }

    if affects_active {
        refresh_messages(&ui.borrow(), buf, disp);
    }
}

fn conn_icon(c: ConnState) -> String {
    let dot = match c {
        ConnState::Connected => "🟢",
        ConnState::Connecting | ConnState::Reconnecting => "🟡",
        ConnState::Offline => "🔴",
    };
    format!("{dot} {}", c.label())
}

/// Reconstruye el texto del canal activo en el buffer y baja el scroll.
fn refresh_messages(s: &UiState, buf: &mut text::TextBuffer, disp: &mut text::TextDisplay) {
    let mut text = String::new();
    if let Some(active) = &s.active {
        for m in s.app.channel_messages(active) {
            text.push_str(&format!("{}: {}\n", m.author_name(), render_content(m)));
        }
    }
    buf.set_text(&text);
    let lines = buf.count_lines(0, buf.length());
    disp.scroll(lines, 0);
}

fn render_content(m: &Message) -> String {
    if m.content.is_empty() {
        "(sin texto)".to_string()
    } else {
        m.content.clone()
    }
}

fn channel_label(id: &str) -> String {
    format!("# {id}")
}

/// ¿Es un ID de Discord (snowflake) válido? (solo dígitos, longitud típica).
fn is_snowflake(s: &str) -> bool {
    let len = s.len();
    (16..=20).contains(&len) && s.bytes().all(|b| b.is_ascii_digit())
}

/// Etiqueta a mostrar para un canal: nombre resuelto o, si no, el ID.
fn display_label(s: &UiState, id: &str) -> String {
    s.app
        .channel_names
        .get(id)
        .cloned()
        .unwrap_or_else(|| channel_label(id))
}

/// Reconstruye la lista de canales con los nombres ya resueltos.
fn rebuild_browser(s: &UiState, browser: &mut browser::HoldBrowser) {
    let sel = browser.value();
    browser.clear();
    for id in &s.cfg.followed_channels {
        browser.add(&display_label(s, id));
    }
    if sel > 0 {
        browser.select(sel);
    }
}

/// DMs que pasan el filtro de búsqueda actual (subcadena en el nombre, sin
/// distinguir mayúsculas). Devuelve los canales en orden, igual que se muestran.
fn filtered_dms(s: &UiState) -> Vec<&Channel> {
    let f = s.dm_filter.trim().to_lowercase();
    s.app
        .dms
        .iter()
        .filter(|d| f.is_empty() || d.label().to_lowercase().contains(&f))
        .collect()
}

/// Reconstruye la lista de mensajes directos (DMs) aplicando el filtro de búsqueda.
fn rebuild_dm_browser(s: &UiState, dm_browser: &mut browser::HoldBrowser) {
    dm_browser.clear();
    for dm in filtered_dms(s) {
        dm_browser.add(&dm.label());
    }
}

/// Texto del título de la conversación activa (nombre resuelto o invitación).
fn active_title_text(s: &UiState) -> String {
    match &s.active {
        Some(id) => display_label(s, id),
        None => "Selecciona un canal o DM".to_string(),
    }
}

/// Detalle de la conversación activa para el panel de información lateral.
fn info_text(s: &UiState) -> String {
    let Some(id) = &s.active else {
        return "Sin conversación activa.\n\nElige un canal o un mensaje directo \
                para ver sus datos aquí."
            .to_string();
    };
    let ch = s.app.dms.iter().chain(s.app.channels.iter()).find(|c| &c.id == id);
    let mut t = format!("Nombre:\n  {}\n\nID del canal:\n  {id}\n", display_label(s, id));
    if let Some(c) = ch {
        let tipo = if c.is_voice() {
            "Canal de voz"
        } else if c.is_dm() {
            "Mensaje directo"
        } else {
            "Canal de texto"
        };
        t.push_str(&format!("\nTipo:\n  {tipo}\n"));
        if let Some(g) = &c.guild_id {
            t.push_str(&format!("\nServidor:\n  {g}\n"));
        }
        if !c.recipients.is_empty() {
            let r: Vec<&str> = c.recipients.iter().map(|u| u.display_name()).collect();
            t.push_str(&format!("\nParticipantes:\n  {}\n", r.join("\n  ")));
        }
    }
    let n = s.app.channel_messages(id).count();
    t.push_str(&format!("\nMensajes cargados:\n  {n}\n"));
    t
}

/// Refresca el título de la conversación y el panel de información a la vez.
fn update_active_views(
    s: &UiState,
    active_title: &mut frame::Frame,
    info_buf: &mut text::TextBuffer,
) {
    active_title.set_label(&active_title_text(s));
    info_buf.set_text(&info_text(s));
}

/// Estilo común de los encabezados de sección de la barra lateral.
fn style_header(f: &mut frame::Frame) {
    f.set_label_color(Color::from_rgb(140, 160, 220));
    f.set_label_size(12);
    f.set_align(Align::Left | Align::Inside);
}

// --- Ajustes de voz (panel estilo Discord) -----------------------------------

/// Encabezado de sección del panel de ajustes (mayúsculas grises pequeñas,
/// como las secciones de «Voz y vídeo» de Discord).
fn settings_header(title: &str) -> frame::Frame {
    let mut h = frame::Frame::default().with_label(title);
    h.set_label_size(11);
    h.set_label_color(Color::from_rgb(181, 186, 193));
    h.set_align(Align::Left | Align::Inside);
    h
}

/// Escapa los metacaracteres de los menús FLTK ('/', '_', '&', '\\' crean
/// submenús/atajos) para mostrar nombres de dispositivo literales.
fn menu_escape(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('/', "\\/")
        .replace('_', "\\_")
        .replace('&', "\\&")
        .replace('|', "¦")
}

/// Desplegable de dispositivos: «Predeterminado» + los nombres detectados,
/// preseleccionando el guardado en config (si sigue existiendo).
fn device_choice(names: &[String], current: &Option<String>) -> menu::Choice {
    let mut c = menu::Choice::default();
    c.set_color(Color::from_rgb(30, 31, 34));
    c.add_choice("Predeterminado");
    for n in names {
        c.add_choice(&menu_escape(n));
    }
    let idx = current
        .as_ref()
        .and_then(|cur| names.iter().position(|n| n == cur))
        .map(|i| i as i32 + 1)
        .unwrap_or(0);
    c.set_value(idx);
    c
}

/// Slider horizontal con el valor visible (volúmenes y sensibilidad).
fn settings_slider(min: f64, max: f64, value: f64) -> valuator::HorValueSlider {
    let mut s = valuator::HorValueSlider::default();
    s.set_bounds(min, max);
    s.set_step(1.0, 1);
    s.set_value(value);
    s.set_color(Color::from_rgb(30, 31, 34));
    s.set_selection_color(Color::from_rgb(88, 101, 242));
    s.set_text_size(10);
    s.set_text_color(Color::from_rgb(223, 226, 230));
    s
}

/// Ventana «Ajustes de voz» (réplica del panel Voz y vídeo de Discord):
/// dispositivos de entrada/salida, volúmenes, «Probemos el micrófono» con
/// medidor en vivo, sensibilidad de entrada y procesamiento de voz
/// (cancelación de eco, supresión de ruido, control de ganancia automático).
/// Todos los cambios se aplican EN VIVO —también en mitad de una llamada— y se
/// persisten en la config.
fn open_voice_settings(ui: &Rc<RefCell<UiState>>, open_flag: Rc<Cell<bool>>) {
    use crate::audio;
    use std::sync::atomic::Ordering;

    let accent = Color::from_rgb(88, 101, 242); // blurple Discord
    let green = Color::from_rgb(35, 165, 89); // verde del medidor de Discord
    let red = Color::from_rgb(218, 55, 60);
    let v = ui.borrow().cfg.voice.clone();
    let in_names = audio::input_device_names();
    let out_names = audio::output_device_names();

    let mut win = window::Window::default()
        .with_size(620, 600)
        .with_label("Ajustes de voz");
    win.set_color(Color::from_rgb(49, 51, 56)); // panel central Discord #313338

    let mut col = group::Flex::default_fill().column();
    col.set_margin(20);
    col.set_pad(8);

    // --- Dispositivos (entrada / salida en dos columnas) --------------------
    let mut dev_row = group::Flex::default().row();
    dev_row.set_pad(16);
    let in_choice = {
        let mut c = group::Flex::default().column();
        c.set_pad(4);
        let h = settings_header("DISPOSITIVO DE ENTRADA");
        c.fixed(&h, 16);
        let ch = device_choice(&in_names, &v.input_device);
        c.fixed(&ch, 28);
        c.end();
        ch
    };
    let out_choice = {
        let mut c = group::Flex::default().column();
        c.set_pad(4);
        let h = settings_header("DISPOSITIVO DE SALIDA");
        c.fixed(&h, 16);
        let ch = device_choice(&out_names, &v.output_device);
        c.fixed(&ch, 28);
        c.end();
        ch
    };
    dev_row.end();
    col.fixed(&dev_row, 52);

    // --- Volúmenes -----------------------------------------------------------
    let mut vol_row = group::Flex::default().row();
    vol_row.set_pad(16);
    let in_vol = {
        let mut c = group::Flex::default().column();
        c.set_pad(4);
        let h = settings_header("VOLUMEN DE ENTRADA (%)");
        c.fixed(&h, 16);
        let s = settings_slider(0.0, 200.0, v.input_volume as f64);
        c.fixed(&s, 26);
        c.end();
        s
    };
    let out_vol = {
        let mut c = group::Flex::default().column();
        c.set_pad(4);
        let h = settings_header("VOLUMEN DE SALIDA (%)");
        c.fixed(&h, 16);
        let s = settings_slider(0.0, 200.0, v.output_volume as f64);
        c.fixed(&s, 26);
        c.end();
        s
    };
    vol_row.end();
    col.fixed(&vol_row, 50);

    // --- Prueba de micrófono -------------------------------------------------
    let h = settings_header("PROBEMOS EL MICRÓFONO");
    col.fixed(&h, 18);
    let mut test_hint = frame::Frame::default().with_label(
        "¿Tienes problemas de voz? Inicia una prueba y di algo divertido: \
         te lo reproduciremos por la salida elegida.",
    );
    test_hint.set_label_size(11);
    test_hint.set_label_color(Color::from_rgb(181, 186, 193));
    test_hint.set_align(Align::Left | Align::Inside | Align::Wrap);
    col.fixed(&test_hint, 28);
    let mut test_row = group::Flex::default().row();
    test_row.set_pad(12);
    let mut mic_btn = button::Button::default().with_label("Probemos");
    mic_btn.set_color(accent);
    mic_btn.set_label_color(Color::White);
    test_row.fixed(&mic_btn, 140);
    let mut meter = misc::Progress::default();
    meter.set_minimum(0.0);
    meter.set_maximum(100.0);
    meter.set_value(0.0);
    meter.set_frame(FrameType::FlatBox);
    meter.set_color(Color::from_rgb(30, 31, 34));
    meter.set_selection_color(green);
    test_row.end();
    col.fixed(&test_row, 30);

    // --- Sensibilidad de entrada ----------------------------------------------
    let h = settings_header("SENSIBILIDAD DE ENTRADA");
    col.fixed(&h, 18);
    let mut auto_chk = button::CheckButton::default()
        .with_label("Determinar automáticamente la sensibilidad de entrada");
    auto_chk.set_value(v.auto_sensitivity);
    col.fixed(&auto_chk, 22);
    let mut sens = settings_slider(-100.0, 0.0, v.sensitivity_db as f64);
    sens.set_tooltip("Umbral en dB: el micrófono solo transmite por encima de este nivel");
    if v.auto_sensitivity {
        sens.deactivate();
    }
    col.fixed(&sens, 26);

    // --- Procesamiento de voz ---------------------------------------------------
    let h = settings_header("PROCESAMIENTO DE VOZ");
    col.fixed(&h, 18);
    let mut echo_chk = button::CheckButton::default().with_label("Cancelación de eco");
    echo_chk.set_tooltip("Atenúa tu micrófono mientras suena la voz de otros (evita que les vuelva su propia voz)");
    echo_chk.set_value(v.echo_suppression);
    col.fixed(&echo_chk, 22);
    let mut noise_chk = button::CheckButton::default().with_label("Supresión de ruido");
    noise_chk.set_tooltip("Silencia el ruido de fondo del micrófono cuando no hablas");
    noise_chk.set_value(v.noise_suppression);
    col.fixed(&noise_chk, 22);
    let mut agc_chk =
        button::CheckButton::default().with_label("Control de ganancia automático");
    agc_chk.set_tooltip("Sube automáticamente el volumen si tu voz se oye baja");
    agc_chk.set_value(v.auto_gain);
    col.fixed(&agc_chk, 22);

    // Pie con consejos (elemento flexible: ocupa lo que sobra).
    let mut foot = frame::Frame::default().with_label(
        "Los cambios se aplican al instante, incluso en plena llamada.\n\
         Consejo anti-eco: usa auriculares. Sin ellos, deja activada la \
         cancelación de eco para que tus amigos no se oigan a sí mismos.",
    );
    foot.set_label_size(11);
    foot.set_label_color(Color::from_rgb(150, 152, 158));
    foot.set_align(Align::Left | Align::Inside | Align::Wrap | Align::Top);

    col.end();
    win.end();
    win.make_resizable(false);
    win.show();

    // --- Callbacks: todo se aplica en vivo y se persiste ----------------------
    {
        let ui = ui.clone();
        let names = in_names.clone();
        let mut c = in_choice.clone();
        c.set_callback(move |c| {
            let idx = c.value();
            let name = if idx <= 0 { None } else { names.get((idx - 1) as usize).cloned() };
            audio::options().set_input_device(name.clone());
            let mut s = ui.borrow_mut();
            s.cfg.voice.input_device = name;
            let _ = s.cfg.save();
        });
    }
    {
        let ui = ui.clone();
        let names = out_names.clone();
        let mut c = out_choice.clone();
        c.set_callback(move |c| {
            let idx = c.value();
            let name = if idx <= 0 { None } else { names.get((idx - 1) as usize).cloned() };
            audio::options().set_output_device(name.clone());
            let mut s = ui.borrow_mut();
            s.cfg.voice.output_device = name;
            let _ = s.cfg.save();
        });
    }
    {
        let ui = ui.clone();
        let mut s2 = in_vol.clone();
        s2.set_callback(move |s| {
            let val = s.value().round() as u32;
            audio::options().set_input_volume(val);
            let mut st = ui.borrow_mut();
            st.cfg.voice.input_volume = val;
            let _ = st.cfg.save();
        });
    }
    {
        let ui = ui.clone();
        let mut s2 = out_vol.clone();
        s2.set_callback(move |s| {
            let val = s.value().round() as u32;
            audio::options().set_output_volume(val);
            let mut st = ui.borrow_mut();
            st.cfg.voice.output_volume = val;
            let _ = st.cfg.save();
        });
    }

    // Prueba de micrófono: alterna captura→procesado→reproducción local.
    let test: Rc<RefCell<Option<audio::MicTest>>> = Rc::new(RefCell::new(None));
    {
        let test = test.clone();
        let mut meter2 = meter.clone();
        mic_btn.set_callback(move |b| {
            let mut t = test.borrow_mut();
            if t.is_some() {
                *t = None; // Drop detiene el hilo de la prueba
                b.set_label("Probemos");
                b.set_color(accent);
                meter2.set_value(0.0);
            } else {
                *t = Some(audio::start_mic_test());
                b.set_label("Deja de probar");
                b.set_color(red);
            }
            b.redraw();
        });
    }
    // Medidor en vivo (también se mueve durante una llamada real).
    {
        let win2 = win.clone();
        let mut meter2 = meter.clone();
        app::add_timeout3(0.06, move |h| {
            if !win2.shown() {
                return; // la ventana se cerró: dejar de repetir
            }
            meter2.set_value(audio::options().mic_level() as f64);
            app::repeat_timeout3(0.06, h);
        });
    }

    {
        let ui = ui.clone();
        let mut sens2 = sens.clone();
        auto_chk.set_callback(move |c| {
            let auto = c.value();
            audio::options().auto_sensitivity.store(auto, Ordering::Relaxed);
            if auto {
                sens2.deactivate();
            } else {
                sens2.activate();
            }
            let mut s = ui.borrow_mut();
            s.cfg.voice.auto_sensitivity = auto;
            let _ = s.cfg.save();
        });
    }
    {
        let ui = ui.clone();
        let mut s2 = sens.clone();
        s2.set_callback(move |s| {
            let db = s.value().round() as i32;
            audio::options().set_sensitivity_db(db);
            let mut st = ui.borrow_mut();
            st.cfg.voice.sensitivity_db = db;
            let _ = st.cfg.save();
        });
    }
    {
        let ui = ui.clone();
        echo_chk.set_callback(move |c| {
            let on = c.value();
            audio::options().echo_suppress.store(on, Ordering::Relaxed);
            let mut s = ui.borrow_mut();
            s.cfg.voice.echo_suppression = on;
            let _ = s.cfg.save();
        });
    }
    {
        let ui = ui.clone();
        noise_chk.set_callback(move |c| {
            let on = c.value();
            audio::options().noise_suppress.store(on, Ordering::Relaxed);
            let mut s = ui.borrow_mut();
            s.cfg.voice.noise_suppression = on;
            let _ = s.cfg.save();
        });
    }
    {
        let ui = ui.clone();
        agc_chk.set_callback(move |c| {
            let on = c.value();
            audio::options().agc.store(on, Ordering::Relaxed);
            let mut s = ui.borrow_mut();
            s.cfg.voice.auto_gain = on;
            let _ = s.cfg.save();
        });
    }

    // Cerrar la ventana detiene la prueba y libera el guard de apertura.
    {
        let test = test.clone();
        win.set_callback(move |w| {
            *test.borrow_mut() = None;
            open_flag.set(false);
            w.hide();
        });
    }
}

/// Pantalla de login: pide el token, lo valida contra la API y lo guarda.
fn run_login(app: &app::App, handle: &Handle) -> Option<String> {
    let mut win = window::Window::default()
        .with_size(480, 220)
        .with_label("discord-lite — iniciar sesión");
    let mut col = group::Flex::default_fill().column();
    col.set_margin(14);

    let mut info = frame::Frame::default()
        .with_label("Pega tu token de usuario de Discord y pulsa Entrar.");
    info.set_align(Align::Left | Align::Inside | Align::Wrap);

    let mut inp = input::SecretInput::default();
    let mut err = frame::Frame::default();
    err.set_label_color(Color::Red);
    err.set_align(Align::Left | Align::Inside | Align::Wrap);
    let mut btn = button::Button::default().with_label("Entrar");
    let mut import_btn =
        button::Button::default().with_label("Importar de Discord (cuenta propia)");

    col.fixed(&inp, 30);
    col.fixed(&btn, 32);
    col.fixed(&import_btn, 28);
    col.end();
    win.end();
    win.make_resizable(true);
    win.show();

    let result = Rc::new(RefCell::new(None::<String>));
    let finished = Rc::new(Cell::new(false));

    {
        let result = result.clone();
        let finished = finished.clone();
        let handle = handle.clone();
        let inp = inp.clone();
        let mut err = err.clone();
        let mut win2 = win.clone();
        btn.set_callback(move |_| {
            let tok = inp.value().trim().to_string();
            if tok.is_empty() {
                err.set_label("Token vacío.");
                return;
            }
            err.set_label("Validando…");
            app::flush();
            match handle.block_on(crate::rest::validate(&tok)) {
                Ok(u) => {
                    let _ = crate::auth::save_token(&tok);
                    tracing::info!("login correcto: {}", u.display_name());
                    *result.borrow_mut() = Some(tok);
                    finished.set(true);
                    win2.hide();
                }
                Err(e) => {
                    err.set_label(&crate::rest::friendly_auth_error(&e));
                }
            }
        });
    }

    // Importar el token del Discord oficial instalado (cuenta del propio usuario).
    {
        let result = result.clone();
        let finished = finished.clone();
        let handle = handle.clone();
        let mut err = err.clone();
        let mut win2 = win.clone();
        import_btn.set_callback(move |b| {
            b.deactivate();
            err.set_label("Buscando token en el Discord local…");
            app::flush();
            let candidates = crate::token_import::extract_candidates();
            if candidates.is_empty() {
                err.set_label("No se encontró token local (¿Discord instalado y con sesión?).");
                b.activate();
                return;
            }
            // Valida cada candidato y usa el primero correcto.
            for tok in candidates {
                if let Ok(u) = handle.block_on(crate::rest::validate(&tok)) {
                    let _ = crate::auth::save_token(&tok);
                    tracing::info!("token importado y validado: {}", u.display_name());
                    *result.borrow_mut() = Some(tok);
                    finished.set(true);
                    win2.hide();
                    return;
                }
            }
            err.set_label("Se encontró token pero no validó (¿expiró? abre Discord e inténtalo).");
            b.activate();
        });
    }

    while app.wait() {
        if finished.get() || !win.shown() {
            break;
        }
    }
    let r = result.borrow().clone();
    r
}
