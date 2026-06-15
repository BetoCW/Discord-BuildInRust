//! discord-lite — cliente Discord nativo y ultraligero (uso personal).
//!
//! Texto en tiempo real (REST + Gateway) con GUI nativa FLTK de doble clic.
//! La voz es la segunda fase (ver `tasks.md`).

// En release: sin ventana de consola (doble clic limpio). En debug: con consola
// para ver los logs.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod aec;
mod applog;
mod audio;
mod auth;
mod config;
mod dave;
mod gateway;
mod model;
mod net;
mod rest;
mod state;
mod token_import;
mod ui;
mod voice;

use anyhow::Result;

fn main() -> Result<()> {
    // Log a archivo junto al exe (para diagnosticar aunque se abra con doble clic).
    applog::init_file();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "discord_lite=debug,warn".into()),
        )
        .with_ansi(false) // sin códigos de color: legible en el panel de la GUI
        .with_writer(applog::writer())
        .init();
    tracing::info!("discord-lite arrancando; log en {}", applog::log_path().display());

    // Runtime async en hilos de fondo; la GUI corre en el hilo principal.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    // Diagnóstico: comprueba la importación del token sin exponerlo.
    if std::env::args().any(|a| a == "--check-import") {
        let cands = token_import::extract_candidates();
        println!("tokens candidatos encontrados: {}", cands.len());
        for (i, t) in cands.iter().enumerate() {
            println!("  [{i}] {}", auth::redact(t));
        }
        return Ok(());
    }

    // Importa el token del Discord local, lo valida y lo guarda (sin exponerlo).
    if std::env::args().any(|a| a == "--import") {
        let cands = token_import::extract_candidates();
        if cands.is_empty() {
            println!("No se encontró token local (¿Discord instalado y con sesión?).");
            return Ok(());
        }
        for t in cands {
            match runtime.block_on(rest::validate(&t)) {
                Ok(u) => {
                    auth::save_token(&t)?;
                    println!(
                        "Token importado y guardado para {} ({}).",
                        u.display_name(),
                        auth::redact(&t)
                    );
                    return Ok(());
                }
                Err(_) => continue,
            }
        }
        println!("Se encontraron tokens pero ninguno validó (¿expiró? abre Discord).");
        return Ok(());
    }

    ui::run(runtime)
}
