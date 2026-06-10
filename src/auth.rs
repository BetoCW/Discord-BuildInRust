//! Almacenamiento **seguro** del token de usuario (RF-1, RNF-8, R-2).
//!
//! Estrategia:
//!   1. Primaria  → keychain/credential manager del SO (crate `keyring`).
//!   2. Fallback  → archivo en el dir de config con permisos restringidos.
//!
//! Reglas duras: el token NUNCA se imprime en logs ni se serializa en `config`.
//! Para depurar usar siempre [`redact`].

use anyhow::{Context, Result};
use std::path::PathBuf;

const SERVICE: &str = "discord-lite";
const ACCOUNT: &str = "user-token";

/// Devuelve una versión censurada del token, segura para logs/errores.
pub fn redact(token: &str) -> String {
    let n = token.chars().count();
    if n <= 8 {
        "***".to_string()
    } else {
        let head: String = token.chars().take(4).collect();
        format!("{head}…(+{} car. ocultos)", n - 4)
    }
}

/// Guarda el token: intenta keychain y, si falla, archivo restringido.
pub fn save_token(token: &str) -> Result<()> {
    match keyring_entry().and_then(|e| e.set_password(token).map_err(Into::into)) {
        Ok(()) => {
            tracing::info!("token guardado en el keychain del SO");
            Ok(())
        }
        Err(e) => {
            tracing::warn!("keychain no disponible ({e}); usando archivo restringido");
            save_token_file(token)
        }
    }
}

/// Recupera el token desde keychain o, si no, desde el archivo de fallback.
pub fn load_token() -> Result<Option<String>> {
    if let Ok(entry) = keyring_entry() {
        match entry.get_password() {
            Ok(tok) => return Ok(Some(tok)),
            Err(keyring::Error::NoEntry) => {}
            Err(e) => tracing::warn!("error leyendo keychain ({e}); probando archivo"),
        }
    }
    load_token_file()
}

/// Borra el token de ambos sitios (logout). No falla si ya no existe.
pub fn delete_token() -> Result<()> {
    if let Ok(entry) = keyring_entry() {
        match entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => {}
            Err(e) => tracing::warn!("no se pudo borrar del keychain: {e}"),
        }
    }
    let path = token_file_path()?;
    if path.exists() {
        std::fs::remove_file(&path).with_context(|| format!("borrando {}", path.display()))?;
    }
    Ok(())
}

fn keyring_entry() -> Result<keyring::Entry> {
    keyring::Entry::new(SERVICE, ACCOUNT).context("creando entrada de keyring")
}

// --- Fallback en archivo con permisos restringidos -------------------------

fn token_file_path() -> Result<PathBuf> {
    let dirs = crate::config::project_dirs()?;
    Ok(dirs.config_dir().join("token.secret"))
}

fn save_token_file(token: &str) -> Result<()> {
    let path = token_file_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, token).with_context(|| format!("escribiendo {}", path.display()))?;
    restrict_permissions(&path)?;
    Ok(())
}

fn load_token_file() -> Result<Option<String>> {
    let path = token_file_path()?;
    if !path.exists() {
        return Ok(None);
    }
    let tok = std::fs::read_to_string(&path)?.trim().to_string();
    if tok.is_empty() {
        Ok(None)
    } else {
        Ok(Some(tok))
    }
}

/// Restringe el acceso al archivo solo al usuario actual.
#[cfg(unix)]
fn restrict_permissions(path: &std::path::Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(path)?.permissions();
    perms.set_mode(0o600);
    std::fs::set_permissions(path, perms)?;
    Ok(())
}

/// En Windows usamos `icacls` para limitar el acceso al usuario actual.
#[cfg(windows)]
fn restrict_permissions(path: &std::path::Path) -> Result<()> {
    let p = path.to_string_lossy().to_string();
    // Quita herencia y concede control total solo al usuario actual.
    let user = std::env::var("USERNAME").unwrap_or_else(|_| "%USERNAME%".to_string());
    let _ = std::process::Command::new("icacls")
        .args([&p, "/inheritance:r"])
        .output();
    let grant = format!("{user}:F");
    let _ = std::process::Command::new("icacls")
        .args([&p, "/grant:r", &grant])
        .output();
    Ok(())
}
