//! DAVE — *Discord Audio & Video End-to-End Encryption* (Fase 2.1).
//!
//! Hace compatible la app con canales que **exigen** E2EE (cierre 4017). Dos partes:
//!
//!   1. **Cifrado de frames de audio** (implementado + testeado aquí): del
//!      *exporter secret* MLS se deriva por emisor una clave con `HashRatchet`, y
//!      cada frame Opus se cifra con **AES-128-GCM con tag truncado a 64 bits**
//!      en el formato de trailer de `libdave`.
//!   2. **Handshake MLS (RFC 9420)** sobre el Voice Gateway v8 (opcodes 21–31)
//!      con `openmls`, ciphersuite 2 (P256). *(Pendiente: ver `dave_mls.rs`/TODO.)*
//!
//! Constantes y formato tomados de discord/libdave (autoritativo). GCM se
//! implementa a mano (`aes` + `ghash`) porque `aes-gcm` no admite tags < 12 B.
#![allow(dead_code)] // varias piezas se conectan al integrar el handshake MLS

use aes::cipher::generic_array::GenericArray;
use aes::cipher::{BlockEncrypt, KeyInit};
use aes::Aes128;
use anyhow::{anyhow, bail, Result};
use ghash::universal_hash::UniversalHash;
use ghash::GHash;

// --- Constantes de libdave (cpp/src/common.h) ------------------------------
pub const PROTOCOL_VERSION: u16 = 1;
const KEY_BYTES: usize = 16; // AES-128
const NONCE_BYTES: usize = 12; // AES-GCM nonce
const SYNC_NONCE_BYTES: usize = 4; // contador truncado (u32)
const SYNC_NONCE_OFFSET: usize = NONCE_BYTES - SYNC_NONCE_BYTES; // 8
const TRUNCATED_TAG_BYTES: usize = 8; // tag AES-GCM truncado a 64 bits
const GENERATION_SHIFT_BITS: u32 = 8 * (SYNC_NONCE_BYTES as u32 - 1); // 24
const MAGIC_MARKER: u16 = 0xFAFA;
/// Tope de búsqueda del prefijo en claro (rango sin cifrar) al descifrar frames
/// ajenos: el cliente oficial deja una cabecera al frente y cifra solo el resto.
/// Empíricamente son 8 bytes; buscamos por si varía. Nuestros frames usan 0.
const MAX_UNENCRYPTED_PREFIX: usize = 16;
/// Etiqueta del exporter MLS para derivar el secreto base por usuario.
pub const EXPORTER_LABEL: &str = "Discord Secure Frames v0";

// ===========================================================================
// Opcodes DAVE del Voice Gateway v8
// ===========================================================================
pub mod op {
    pub const PREPARE_TRANSITION: u8 = 21; // JSON  (S→C)
    pub const EXECUTE_TRANSITION: u8 = 22; // JSON  (S→C)
    pub const READY_FOR_TRANSITION: u8 = 23; // JSON (C→S)
    pub const PREPARE_EPOCH: u8 = 24; // JSON   (S→C)
    pub const EXTERNAL_SENDER: u8 = 25; // bin   (S→C)
    pub const KEY_PACKAGE: u8 = 26; // bin       (C→S)
    pub const PROPOSALS: u8 = 27; // bin         (S→C)
    pub const COMMIT_WELCOME: u8 = 28; // bin    (C→S)
    pub const ANNOUNCE_COMMIT: u8 = 29; // bin   (S→C)
    pub const WELCOME: u8 = 30; // bin           (S→C)
    pub const INVALID_COMMIT_WELCOME: u8 = 31; // JSON (C→S)
}

// ===========================================================================
// Cifrado de frames de audio (parte verificable)
// ===========================================================================

/// Cifrador por-emisor: deriva la clave AES del secreto base (exporter MLS) y
/// cifra/descifra cada frame Opus en el formato de trailer de libdave.
pub struct FrameCryptor {
    base_secret: Vec<u8>,
    send_counter: u32,
}

impl FrameCryptor {
    /// Crea el cifrador desde el secreto base de 16 bytes del exporter MLS.
    pub fn from_base_secret(base_secret: &[u8]) -> Self {
        Self {
            base_secret: base_secret.to_vec(),
            send_counter: 0,
        }
    }

    fn key_for(&self, generation: u32) -> [u8; KEY_BYTES] {
        hash_ratchet_key(&self.base_secret, generation)
    }

    /// Nonce de 12 bytes: contador (u32 LE) en el offset 8.
    fn build_nonce(counter: u32) -> [u8; NONCE_BYTES] {
        let mut n = [0u8; NONCE_BYTES];
        n[SYNC_NONCE_OFFSET..].copy_from_slice(&counter.to_le_bytes());
        n
    }

    /// Cifra un frame Opus (completamente cifrado, sin rangos en claro).
    pub fn encrypt(&mut self, opus: &[u8]) -> Result<Vec<u8>> {
        let counter = self.send_counter;
        self.send_counter = self.send_counter.wrapping_add(1);
        let generation = counter >> GENERATION_SHIFT_BITS;

        let key = self.key_for(generation);
        let nonce = Self::build_nonce(counter);
        let (ciphertext, full_tag) = gcm128_encrypt(&key, &nonce, opus, &[]);

        // Trailer: [ciphertext][tag(8)][nonce_leb128][supplemental_size(1)][magic(2)]
        let mut nonce_leb = Vec::new();
        write_leb128(&mut nonce_leb, counter as u64);
        let supplemental_size = (TRUNCATED_TAG_BYTES + nonce_leb.len() + 1 + 2) as u8;

        let mut out = Vec::with_capacity(ciphertext.len() + supplemental_size as usize);
        out.extend_from_slice(&ciphertext);
        out.extend_from_slice(&full_tag[..TRUNCATED_TAG_BYTES]);
        out.extend_from_slice(&nonce_leb);
        out.push(supplemental_size);
        out.extend_from_slice(&MAGIC_MARKER.to_le_bytes());
        Ok(out)
    }

    /// Descifra un frame en formato DAVE. Devuelve el paquete Opus completo.
    ///
    /// El emisor (cliente oficial) deja una **cabecera en claro** al frente del
    /// frame y cifra solo el resto (rango sin cifrar de libdave; AAD vacío). El
    /// tamaño del prefijo se localiza probando hasta que el tag de 64 bits
    /// autentica (colisión ~1/2^64). Se reconstruye el paquete original "en su
    /// sitio": cabecera en claro + payload descifrado. Nuestros propios frames no
    /// llevan prefijo (autentican en 0), así que el roundtrip propio sigue válido.
    pub fn decrypt(&self, frame: &[u8]) -> Result<Vec<u8>> {
        if frame.len() < TRUNCATED_TAG_BYTES + 1 + 2 {
            bail!("frame DAVE demasiado corto");
        }
        let n = frame.len();
        let magic = u16::from_le_bytes([frame[n - 2], frame[n - 1]]);
        if magic != MAGIC_MARKER {
            bail!("magic marker DAVE inválido");
        }
        let supp_size = frame[n - 3] as usize;
        if supp_size > n || supp_size < TRUNCATED_TAG_BYTES + 1 + 2 {
            bail!("supplemental size DAVE inválido");
        }
        let trailer = n - supp_size;
        let tag = &frame[trailer..trailer + TRUNCATED_TAG_BYTES];
        let (counter, _) = read_leb128(&frame[trailer + TRUNCATED_TAG_BYTES..])
            .ok_or_else(|| anyhow!("nonce leb128 inválido"))?;
        let counter = counter as u32;
        let generation = counter >> GENERATION_SHIFT_BITS;

        let key = self.key_for(generation);
        let nonce = Self::build_nonce(counter);

        let max_prefix = trailer.min(MAX_UNENCRYPTED_PREFIX);
        for prefix in 0..=max_prefix {
            let ct = &frame[prefix..trailer];
            if let Ok(payload) = gcm128_decrypt(&key, &nonce, ct, &[], tag) {
                // El prefijo es framing interno del emisor (secuencia + ceros), NO
                // datos Opus: se descarta. El payload descifrado YA es el paquete
                // Opus completo. (Nuestros propios frames usan prefijo 0.)
                return Ok(payload);
            }
        }
        bail!("tag GCM no coincide (ningún prefijo en claro autentica)")
    }
}

/// ¿El frame termina con el magic marker de DAVE (`0xFAFA`)? Los frames de
/// relleno/silencio que manda el emisor NO lo llevan; se distinguen así de un
/// fallo real de descifrado para no contarlos como error en el RX.
pub fn has_dave_magic(frame: &[u8]) -> bool {
    let n = frame.len();
    n >= 2 && u16::from_le_bytes([frame[n - 2], frame[n - 1]]) == MAGIC_MARKER
}

/// Deriva la clave de una generación con el `HashRatchet` de MLS (RFC 9420 §9).
///
/// CLAVE para interoperar con libdave/mlspp: `DeriveTreeSecret(secret, label, gen,
/// len) = ExpandWithLabel(secret, label, encode_uint32(gen), len)` — el **context
/// es el índice de generación como u32 BIG-ENDIAN**, NO vacío. mlspp avanza el
/// secreto y deriva la clave del paso `i` usando `i` como context:
///   `secret_{i+1} = DeriveTreeSecret(secret_i, "secret", i, Nh)`
///   `key_g       = DeriveTreeSecret(secret_g, "key",    g, Nk)`
/// (Antes pasábamos context vacío: roundtrip propio OK pero incompatible con
/// libdave incluso en generación 0, cuyo context real es `[0,0,0,0]`.)
fn hash_ratchet_key(base_secret: &[u8], generation: u32) -> [u8; KEY_BYTES] {
    let mut secret = base_secret.to_vec();
    for i in 0..generation {
        secret = expand_with_label(&secret, "secret", &i.to_be_bytes(), 32);
    }
    let key = expand_with_label(&secret, "key", &generation.to_be_bytes(), KEY_BYTES);
    let mut out = [0u8; KEY_BYTES];
    out.copy_from_slice(&key);
    out
}

// ===========================================================================
// GCM-128 manual con tag truncado (aes + ghash)
// ===========================================================================

fn aes_block(cipher: &Aes128, input: [u8; 16]) -> [u8; 16] {
    let mut b = GenericArray::clone_from_slice(&input);
    cipher.encrypt_block(&mut b);
    let mut out = [0u8; 16];
    out.copy_from_slice(&b);
    out
}

fn inc32(block: &mut [u8; 16]) {
    let c = u32::from_be_bytes([block[12], block[13], block[14], block[15]]).wrapping_add(1);
    block[12..].copy_from_slice(&c.to_be_bytes());
}

/// CTR: XOR del flujo de claves empezando en `counter` (que se incrementa).
fn ctr_xor(cipher: &Aes128, mut counter: [u8; 16], data: &[u8]) -> Vec<u8> {
    let mut out = vec![0u8; data.len()];
    for (i, chunk) in data.chunks(16).enumerate() {
        let ks = aes_block(cipher, counter);
        for (j, b) in chunk.iter().enumerate() {
            out[i * 16 + j] = b ^ ks[j];
        }
        inc32(&mut counter);
    }
    out
}

/// Calcula el tag GCM completo (16 bytes) sobre `aad` + `ciphertext`.
fn gcm_tag(cipher: &Aes128, h: &[u8; 16], j0: [u8; 16], aad: &[u8], ct: &[u8]) -> [u8; 16] {
    let mut gh = GHash::new(GenericArray::from_slice(h));
    gh.update_padded(aad);
    gh.update_padded(ct);
    let mut lenblock = [0u8; 16];
    lenblock[..8].copy_from_slice(&((aad.len() as u64) * 8).to_be_bytes());
    lenblock[8..].copy_from_slice(&((ct.len() as u64) * 8).to_be_bytes());
    gh.update(&[GenericArray::clone_from_slice(&lenblock)]);
    let s = gh.finalize();

    let ej0 = aes_block(cipher, j0);
    let mut tag = [0u8; 16];
    for i in 0..16 {
        tag[i] = ej0[i] ^ s[i];
    }
    tag
}

fn gcm128_encrypt(key: &[u8; 16], nonce: &[u8; 12], pt: &[u8], aad: &[u8]) -> (Vec<u8>, [u8; 16]) {
    let cipher = Aes128::new(GenericArray::from_slice(key));
    let h = aes_block(&cipher, [0u8; 16]);
    let mut j0 = [0u8; 16];
    j0[..12].copy_from_slice(nonce);
    j0[15] = 1;
    let mut ctr = j0;
    inc32(&mut ctr);
    let ct = ctr_xor(&cipher, ctr, pt);
    let tag = gcm_tag(&cipher, &h, j0, aad, &ct);
    (ct, tag)
}

fn gcm128_decrypt(
    key: &[u8; 16],
    nonce: &[u8; 12],
    ct: &[u8],
    aad: &[u8],
    trunc_tag: &[u8],
) -> Result<Vec<u8>> {
    let cipher = Aes128::new(GenericArray::from_slice(key));
    let h = aes_block(&cipher, [0u8; 16]);
    let mut j0 = [0u8; 16];
    j0[..12].copy_from_slice(nonce);
    j0[15] = 1;
    let full_tag = gcm_tag(&cipher, &h, j0, aad, ct);
    if trunc_tag.len() > 16 || full_tag[..trunc_tag.len()] != *trunc_tag {
        bail!("tag GCM no coincide (frame DAVE no auténtico)");
    }
    let mut ctr = j0;
    inc32(&mut ctr);
    Ok(ctr_xor(&cipher, ctr, ct))
}

// ===========================================================================
// MLS KDF: ExpandWithLabel (RFC 9420 §8.1) + LEB128 + helpers
// ===========================================================================

fn expand_with_label(secret: &[u8], label: &str, context: &[u8], length: usize) -> Vec<u8> {
    let full_label = format!("MLS 1.0 {label}");
    let mut info = Vec::new();
    info.extend_from_slice(&(length as u16).to_be_bytes());
    write_mls_vec(&mut info, full_label.as_bytes());
    write_mls_vec(&mut info, context);
    hkdf_expand(secret, &info, length)
}

/// HKDF-Expand (RFC 5869) con HMAC-SHA256. A diferencia de `hkdf::from_prk`,
/// acepta PRK de cualquier longitud (MLS usa secretos de 16 bytes).
fn hkdf_expand(prk: &[u8], info: &[u8], length: usize) -> Vec<u8> {
    use hmac::{Hmac, Mac};
    use sha2::Sha256;
    type HmacSha256 = Hmac<Sha256>;

    let mut okm = Vec::with_capacity(length);
    let mut t: Vec<u8> = Vec::new();
    let mut counter: u8 = 1;
    while okm.len() < length {
        let mut mac =
            <HmacSha256 as Mac>::new_from_slice(prk).expect("HMAC acepta cualquier clave");
        mac.update(&t);
        mac.update(info);
        mac.update(&[counter]);
        t = mac.finalize().into_bytes().to_vec();
        okm.extend_from_slice(&t);
        counter = counter.wrapping_add(1);
    }
    okm.truncate(length);
    okm
}

/// Vector de longitud variable de MLS (prefijo estilo QUIC varint).
fn write_mls_vec(out: &mut Vec<u8>, data: &[u8]) {
    let len = data.len();
    if len < 64 {
        out.push(len as u8);
    } else if len < 16384 {
        out.extend_from_slice(&((len as u16) | 0x4000).to_be_bytes());
    } else {
        out.extend_from_slice(&((len as u32) | 0x8000_0000).to_be_bytes());
    }
    out.extend_from_slice(data);
}

fn write_leb128(out: &mut Vec<u8>, mut value: u64) {
    loop {
        let mut byte = (value & 0x7f) as u8;
        value >>= 7;
        if value != 0 {
            byte |= 0x80;
        }
        out.push(byte);
        if value == 0 {
            break;
        }
    }
}

fn read_leb128(data: &[u8]) -> Option<(u64, usize)> {
    let mut result: u64 = 0;
    let mut shift = 0;
    for (i, &byte) in data.iter().enumerate() {
        result |= ((byte & 0x7f) as u64) << shift;
        if byte & 0x80 == 0 {
            return Some((result, i + 1));
        }
        shift += 7;
        if shift >= 64 {
            return None;
        }
    }
    None
}

/// Hex de un buffer (para volcar y comparar contra implementaciones de referencia).
fn to_hex(data: &[u8]) -> String {
    let mut s = String::with_capacity(data.len() * 2);
    for b in data {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// Snowflake (cadena) → 8 bytes little-endian para el context del exporter.
pub fn user_id_context(user_id: &str) -> Vec<u8> {
    let n: u64 = user_id.parse().unwrap_or(0);
    n.to_le_bytes().to_vec()
}

/// Enmarca un mensaje binario DAVE de cliente→servidor: [opcode u8][payload].
pub fn client_binary(opcode: u8, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(1 + payload.len());
    out.push(opcode);
    out.extend_from_slice(payload);
    out
}

// ===========================================================================
// Sesión MLS (handshake DAVE) con openmls
// ===========================================================================

use openmls::prelude::tls_codec::{Deserialize as _, DeserializeBytes as _, Serialize as _};
use openmls::prelude::*;
use openmls_basic_credential::SignatureKeyPair;
use openmls_rust_crypto::OpenMlsRustCrypto;

/// Ciphersuite 2 de DAVE: P256 / AES-128-GCM / SHA-256.
const CIPHERSUITE: Ciphersuite = Ciphersuite::MLS_128_DHKEMP256_AES128GCM_SHA256_P256;

/// Estado MLS del cliente para una llamada E2EE.
pub struct MlsSession {
    provider: OpenMlsRustCrypto,
    signer: SignatureKeyPair,
    credential_with_key: CredentialWithKey,
    user_id: u64,
    external_sender: Option<ExternalSender>,
    group: Option<MlsGroup>,
}

impl MlsSession {
    /// Crea credencial (identity = userId big-endian) y par de firma P256.
    pub fn new(user_id: u64) -> Result<Self> {
        let provider = OpenMlsRustCrypto::default();
        let credential = BasicCredential::new(user_id.to_be_bytes().to_vec());
        let signer = SignatureKeyPair::new(CIPHERSUITE.signature_algorithm())
            .map_err(|e| anyhow!("signature keypair: {e:?}"))?;
        signer
            .store(provider.storage())
            .map_err(|e| anyhow!("store signer: {e:?}"))?;
        let credential_with_key = CredentialWithKey {
            credential: credential.into(),
            signature_key: signer.public().into(),
        };
        Ok(Self {
            provider,
            signer,
            credential_with_key,
            user_id,
            external_sender: None,
            group: None,
        })
    }

    /// Guarda el external sender que envía el gateway (opcode 25).
    pub fn set_external_sender(&mut self, bytes: &[u8]) -> Result<()> {
        let es = ExternalSender::tls_deserialize_exact(bytes)
            .map_err(|e| anyhow!("external sender inválido: {e:?}"))?;
        self.external_sender = Some(es);
        Ok(())
    }

    /// Genera y serializa nuestro KeyPackage para enviarlo (opcode 26).
    pub fn key_package_bytes(&self) -> Result<Vec<u8>> {
        // Capabilities EXACTAS de DAVE (libdave parameters.cpp): solo P256 y
        // credencial básica; sin extensiones ni proposals extra. openmls anuncia
        // muchas más por defecto y Discord lo rechaza.
        let capabilities = Capabilities::new(
            Some(&[ProtocolVersion::Mls10]),
            Some(&[CIPHERSUITE]),
            Some(&[]),
            Some(&[]),
            Some(&[CredentialType::Basic]),
        );
        // DAVE usa lifetime de RANGO MÁXIMO (not_before=0, not_after=u64::MAX) para
        // desactivar la validación temporal; openmls por defecto pone ahora..+90d,
        // que Discord rechaza por desfase de reloj. (davey hace exactamente esto.)
        const MAX_TIMESPAN_LIFETIME: [u8; 16] = [
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // not_before = 0
            0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, // not_after  = u64::MAX
        ];
        let lifetime = Lifetime::tls_deserialize_exact_bytes(&MAX_TIMESPAN_LIFETIME)
            .map_err(|e| anyhow!("lifetime: {e:?}"))?;
        let bundle = KeyPackage::builder()
            .key_package_extensions(Extensions::empty())
            .leaf_node_capabilities(capabilities)
            .key_package_lifetime(lifetime)
            .build(
                CIPHERSUITE,
                &self.provider,
                &self.signer,
                self.credential_with_key.clone(),
            )
            .map_err(|e| anyhow!("build key package: {e:?}"))?;
        // libdave real envía el KeyPackage **bare** (tls::marshal(KeyPackage)),
        // no envuelto en MLSMessage. Probamos bare (con capabilities ya corregidas).
        let bytes = bundle
            .key_package()
            .tls_serialize_detached()
            .map_err(|e| anyhow!("serialize key package: {e:?}"))?;
        tracing::debug!("KeyPackage bare hex ({}B): {}", bytes.len(), to_hex(&bytes));
        Ok(bytes)
    }

    /// Procesa el Welcome (opcode 30) y se une al grupo MLS.
    /// `payload` = `[transition_id u16][Welcome bare]` (la spec: op 30 no envuelve
    /// el Welcome en MLSMessage, y lleva un transition_id delante).
    pub fn process_welcome(&mut self, payload: &[u8]) -> Result<()> {
        if payload.len() < 2 {
            bail!("welcome demasiado corto");
        }
        let welcome_bytes = &payload[2..]; // salta transition_id
        let welcome = Welcome::tls_deserialize_exact(welcome_bytes)
            .map_err(|e| anyhow!("welcome inválido: {e:?}"))?;
        // Config idéntica a davey: árbol en extensión + wire format plaintext.
        let config = MlsGroupJoinConfig::builder()
            .use_ratchet_tree_extension(true)
            .wire_format_policy(PURE_PLAINTEXT_WIRE_FORMAT_POLICY)
            .build();
        let staged = StagedWelcome::new_from_welcome(&self.provider, &config, welcome, None)
            .map_err(|e| anyhow!("staged welcome: {e:?}"))?;
        let group = staged
            .into_group(&self.provider)
            .map_err(|e| anyhow!("into_group: {e:?}"))?;
        self.group = Some(group);
        Ok(())
    }

    /// ¿Ya estamos dentro del grupo MLS?
    pub fn has_group(&self) -> bool {
        self.group.is_some()
    }

    /// Epoch actual del grupo MLS (para logging/diagnóstico).
    pub fn epoch(&self) -> u64 {
        self.group
            .as_ref()
            .map(|g| g.epoch().as_u64())
            .unwrap_or(0)
    }

    /// Procesa un commit MLS entrante (opcode 29 ANNOUNCE_COMMIT) y **avanza el
    /// epoch** del grupo. Lo manda el gateway cuando un miembro entra/sale; tras
    /// procesarlo hay que re-derivar las claves de medios desde el nuevo epoch.
    /// `mls_message` = el `MLSMessage` (sin el prefijo transition_id, que se quita
    /// en `voice.rs`).
    pub fn process_commit(&mut self, mls_message: &[u8]) -> Result<()> {
        let provider = &self.provider;
        let group = self.group.as_mut().ok_or_else(|| anyhow!("sin grupo MLS"))?;
        let msg = MlsMessageIn::tls_deserialize_exact(mls_message)
            .map_err(|e| anyhow!("commit MLSMessage inválido: {e:?}"))?;
        let protocol_message = msg
            .try_into_protocol_message()
            .map_err(|e| anyhow!("op29 no es protocol message: {e:?}"))?;
        let processed = group
            .process_message(provider, protocol_message)
            .map_err(|e| anyhow!("process_message: {e:?}"))?;
        match processed.into_content() {
            ProcessedMessageContent::StagedCommitMessage(staged) => {
                group
                    .merge_staged_commit(provider, *staged)
                    .map_err(|e| anyhow!("merge_staged_commit: {e:?}"))?;
                Ok(())
            }
            _ => bail!("op29 no contenía un commit"),
        }
    }

    /// Deriva el secreto base de medios del epoch actual para un emisor concreto.
    /// Cada participante deriva su clave con SU `user_id` (LE) como context del
    /// exporter MLS: para TX usamos el nuestro; para RX, el de cada remoto.
    pub fn media_base_secret_for(&self, user_id: u64) -> Result<Vec<u8>> {
        let group = self.group.as_ref().ok_or_else(|| anyhow!("sin grupo MLS"))?;
        let ctx = user_id.to_le_bytes();
        let secret = group
            .export_secret(&self.provider, EXPORTER_LABEL, &ctx, KEY_BYTES)
            .map_err(|e| anyhow!("export_secret: {e:?}"))?;
        Ok(secret)
    }

    /// Secreto base de medios de NUESTRO emisor (para `FrameCryptor` de TX).
    pub fn media_base_secret(&self) -> Result<Vec<u8>> {
        self.media_base_secret_for(self.user_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leb128_roundtrip() {
        for v in [0u64, 1, 127, 128, 300, 16384, 1 << 20, u32::MAX as u64] {
            let mut buf = Vec::new();
            write_leb128(&mut buf, v);
            let (decoded, n) = read_leb128(&buf).unwrap();
            assert_eq!(decoded, v);
            assert_eq!(n, buf.len());
        }
    }

    #[test]
    fn gcm_roundtrip_known_answer() {
        // NIST-ish sanity: cifrar y descifrar con tag completo.
        let key = [0x42u8; 16];
        let nonce = [0x24u8; 12];
        let pt = b"opus frame payload de prueba 123";
        let (ct, tag) = gcm128_encrypt(&key, &nonce, pt, &[]);
        assert_ne!(&ct[..], &pt[..]);
        let dec = gcm128_decrypt(&key, &nonce, &ct, &[], &tag).unwrap();
        assert_eq!(&dec[..], &pt[..]);
        // tag truncado a 8 también valida.
        let dec8 = gcm128_decrypt(&key, &nonce, &ct, &[], &tag[..8]).unwrap();
        assert_eq!(&dec8[..], &pt[..]);
        // tag corrupto falla.
        let mut bad = tag;
        bad[0] ^= 1;
        assert!(gcm128_decrypt(&key, &nonce, &ct, &[], &bad[..8]).is_err());
    }

    #[test]
    fn frame_roundtrip() {
        let base = [0x11u8; 16];
        let mut enc = FrameCryptor::from_base_secret(&base);
        let dec = FrameCryptor::from_base_secret(&base);
        let opus = b"\x78\x9a\xbc fake opus packet bytes...";
        let frame = enc.encrypt(opus).unwrap();
        // El frame debe terminar en el magic marker.
        assert_eq!(&frame[frame.len() - 2..], &MAGIC_MARKER.to_le_bytes());
        let out = dec.decrypt(&frame).unwrap();
        assert_eq!(&out[..], &opus[..]);
    }

    #[test]
    fn nonce_layout() {
        let n = FrameCryptor::build_nonce(0x01020304);
        assert_eq!(&n[..8], &[0u8; 8]);
        assert_eq!(&n[8..], &0x01020304u32.to_le_bytes());
    }
}
