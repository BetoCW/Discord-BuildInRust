//! Importación del token desde el cliente oficial de Discord **instalado
//! localmente** (cuenta y máquina del propio usuario).
//!
//! Discord (como Chrome) guarda el token en Local Storage (LevelDB) cifrado con
//! **AES-256-GCM**; la clave AES está en `Local State` protegida con **DPAPI**
//! (atada al usuario de Windows). Aquí:
//!   1. Se lee `Local State` → `os_crypt.encrypted_key`, se quita el prefijo
//!      "DPAPI" y se descifra con `CryptUnprotectData` → clave AES de 32 bytes.
//!   2. Se escanean los `.ldb`/`.log` del LevelDB buscando `dQw4w9WgXcQ:<b64>`.
//!   3. Cada blob se descifra (formato v10/v11: 3B prefijo + 12B nonce + ct+tag).
//!   4. Se devuelven los tokens candidatos (la validación contra la API la hace
//!      quien llame, p. ej. la pantalla de login).
//!
//! Solo Windows. En otros SO, [`extract_candidates`] devuelve vacío.

#[cfg(windows)]
use aes_gcm::{
    aead::{Aead, KeyInit},
    Aes256Gcm, Key, Nonce,
};
#[cfg(windows)]
use base64::{engine::general_purpose::STANDARD, Engine};

/// Devuelve los tokens candidatos hallados en los clientes de Discord locales.
/// Sin red: el llamador debe validar cada uno.
#[cfg(windows)]
pub fn extract_candidates() -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let appdata = match std::env::var("APPDATA") {
        Ok(p) => std::path::PathBuf::from(p),
        Err(_) => return out,
    };

    // Variantes de Discord que pueden estar instaladas.
    for flavor in ["discord", "discordcanary", "discordptb", "discorddevelopment"] {
        let base = appdata.join(flavor);
        if !base.exists() {
            continue;
        }
        let key = read_aes_key(&base.join("Local State"));
        let leveldb = base.join("Local Storage").join("leveldb");
        scan_leveldb(&leveldb, key.as_deref(), &mut out);
    }

    // Únicos, conservando el orden.
    let mut seen = std::collections::HashSet::new();
    out.retain(|t| seen.insert(t.clone()));
    out
}

#[cfg(not(windows))]
pub fn extract_candidates() -> Vec<String> {
    Vec::new()
}

/// Lee y descifra (DPAPI) la clave AES de `Local State`.
#[cfg(windows)]
fn read_aes_key(local_state: &std::path::Path) -> Option<Vec<u8>> {
    let data = std::fs::read_to_string(local_state).ok()?;
    let json: serde_json::Value = serde_json::from_str(&data).ok()?;
    let b64 = json
        .get("os_crypt")?
        .get("encrypted_key")?
        .as_str()?
        .to_string();
    let raw = STANDARD.decode(b64.as_bytes()).ok()?;
    // Prefijo "DPAPI" (5 bytes).
    if raw.len() <= 5 || &raw[..5] != b"DPAPI" {
        return None;
    }
    dpapi_decrypt(&raw[5..]).ok()
}

/// Escanea los archivos del LevelDB buscando tokens (cifrados y, como respaldo,
/// en texto plano de versiones antiguas).
#[cfg(windows)]
fn scan_leveldb(dir: &std::path::Path, key: Option<&[u8]>, out: &mut Vec<String>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };

    // Token cifrado: dQw4w9WgXcQ:<base64>
    let enc_re = regex::Regex::new(r"dQw4w9WgXcQ:([A-Za-z0-9+/=]+)").unwrap();
    // Token en claro (Discord antiguo): user-id . marca . hmac  o  mfa.<...>
    let plain_re =
        regex::Regex::new(r"([A-Za-z0-9_-]{23,28}\.[A-Za-z0-9_-]{6,7}\.[A-Za-z0-9_-]{27,})")
            .unwrap();
    let mfa_re = regex::Regex::new(r"(mfa\.[A-Za-z0-9_-]{80,})").unwrap();

    for entry in entries.flatten() {
        let path = entry.path();
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();
        if ext != "ldb" && ext != "log" {
            continue;
        }
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(_) => continue,
        };
        let text = String::from_utf8_lossy(&bytes);

        if let Some(key) = key {
            for cap in enc_re.captures_iter(&text) {
                if let Some(tok) = decrypt_token(&cap[1], key) {
                    out.push(tok);
                }
            }
        }
        for cap in plain_re.captures_iter(&text) {
            out.push(cap[1].to_string());
        }
        for cap in mfa_re.captures_iter(&text) {
            out.push(cap[1].to_string());
        }
    }
}

/// Descifra un blob base64 (v10/v11) con la clave AES-256-GCM.
#[cfg(windows)]
fn decrypt_token(b64: &str, key: &[u8]) -> Option<String> {
    let blob = STANDARD.decode(b64.as_bytes()).ok()?;
    // 3 bytes de prefijo de versión + 12 de nonce + ciphertext(+tag de 16).
    if blob.len() < 3 + 12 + 16 {
        return None;
    }
    let nonce = &blob[3..15];
    let ciphertext = &blob[15..];
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key));
    let plaintext = cipher.decrypt(Nonce::from_slice(nonce), ciphertext).ok()?;
    String::from_utf8(plaintext).ok()
}

// --- DPAPI (CryptUnprotectData) vía FFI mínima a crypt32 -------------------

#[cfg(windows)]
#[repr(C)]
struct DataBlob {
    cb_data: u32,
    pb_data: *mut u8,
}

#[cfg(windows)]
#[link(name = "crypt32")]
extern "system" {
    fn CryptUnprotectData(
        p_data_in: *const DataBlob,
        ppsz_data_descr: *mut *mut u16,
        p_optional_entropy: *const DataBlob,
        pv_reserved: *mut core::ffi::c_void,
        p_prompt_struct: *mut core::ffi::c_void,
        dw_flags: u32,
        p_data_out: *mut DataBlob,
    ) -> i32;
}

#[cfg(windows)]
extern "system" {
    fn LocalFree(h_mem: *mut core::ffi::c_void) -> *mut core::ffi::c_void;
}

#[cfg(windows)]
fn dpapi_decrypt(input: &[u8]) -> anyhow::Result<Vec<u8>> {
    let in_blob = DataBlob {
        cb_data: input.len() as u32,
        pb_data: input.as_ptr() as *mut u8,
    };
    let mut out_blob = DataBlob {
        cb_data: 0,
        pb_data: core::ptr::null_mut(),
    };
    let ok = unsafe {
        CryptUnprotectData(
            &in_blob,
            core::ptr::null_mut(),
            core::ptr::null(),
            core::ptr::null_mut(),
            core::ptr::null_mut(),
            0,
            &mut out_blob,
        )
    };
    if ok == 0 || out_blob.pb_data.is_null() {
        anyhow::bail!("CryptUnprotectData falló (¿otro usuario de Windows?)");
    }
    let data =
        unsafe { core::slice::from_raw_parts(out_blob.pb_data, out_blob.cb_data as usize) }.to_vec();
    unsafe {
        LocalFree(out_blob.pb_data as *mut core::ffi::c_void);
    }
    Ok(data)
}
