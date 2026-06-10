//! Captura de logs para mostrarlos **dentro de la app** (panel inferior) además
//! de en la terminal. Un `MakeWriter` para `tracing_subscriber` que escribe a
//! stderr y a un buffer en memoria que la GUI lee y pinta.

use std::collections::VecDeque;
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use tracing_subscriber::fmt::MakeWriter;

static BUFFER: OnceLock<Arc<Mutex<VecDeque<String>>>> = OnceLock::new();
static FILE: OnceLock<Mutex<std::fs::File>> = OnceLock::new();
const MAX_LINES: usize = 1000;

fn buf() -> &'static Arc<Mutex<VecDeque<String>>> {
    BUFFER.get_or_init(|| Arc::new(Mutex::new(VecDeque::new())))
}

/// Ruta del archivo de log: junto al ejecutable (`discord-lite.log`). Así queda
/// en `dist\` aunque se abra con doble clic (sin consola que capture stderr).
pub fn log_path() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("discord-lite.log")))
        .unwrap_or_else(|| PathBuf::from("discord-lite.log"))
}

/// Abre (truncando) el archivo de log. Llamar una vez al arrancar, antes de
/// inicializar el subscriber.
pub fn init_file() {
    if let Ok(f) = std::fs::File::create(log_path()) {
        let _ = FILE.set(Mutex::new(f));
    }
}

/// Writer que duplica los logs: terminal (stderr) + buffer de la GUI.
#[derive(Clone, Copy, Default)]
pub struct GuiWriter;

impl Write for GuiWriter {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        let _ = std::io::stderr().write_all(data);
        if let Some(f) = FILE.get() {
            if let Ok(mut f) = f.lock() {
                let _ = f.write_all(data);
                let _ = f.flush();
            }
        }
        if let Ok(s) = std::str::from_utf8(data) {
            let mut b = buf().lock().unwrap();
            for line in s.lines() {
                if !line.is_empty() {
                    b.push_back(line.to_string());
                }
            }
            while b.len() > MAX_LINES {
                b.pop_front();
            }
        }
        Ok(data.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        std::io::stderr().flush()
    }
}

impl<'a> MakeWriter<'a> for GuiWriter {
    type Writer = GuiWriter;
    fn make_writer(&'a self) -> Self::Writer {
        *self
    }
}

/// Para `tracing_subscriber::fmt().with_writer(applog::writer())`.
pub fn writer() -> GuiWriter {
    GuiWriter
}

/// Número de líneas acumuladas (para refrescar la GUI solo cuando cambie).
pub fn len() -> usize {
    buf().lock().map(|b| b.len()).unwrap_or(0)
}

/// Texto completo del log para el panel de la GUI.
pub fn snapshot() -> String {
    buf()
        .lock()
        .map(|b| b.iter().cloned().collect::<Vec<_>>().join("\n"))
        .unwrap_or_default()
}
