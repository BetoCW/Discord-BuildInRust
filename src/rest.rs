//! Cliente REST de Discord (`https://discord.com/api/v10`).
//!
//! Acciones puntuales: validar token, historial, enviar mensaje, listar
//! guilds/canales/DMs. Respeta rate limits (429 + `retry_after`) — RNF-5, R-1.

use crate::model::{Channel, Guild, Message, User};
use anyhow::{anyhow, bail, Context, Result};
use serde::Serialize;
use std::time::Duration;

const API_BASE: &str = "https://discord.com/api/v10";
const USER_AGENT: &str = "discord-lite/0.1 (personal)";
const MAX_RETRIES: u32 = 4;

#[derive(Clone)]
pub struct RestClient {
    http: reqwest::Client,
    token: String,
}

impl RestClient {
    pub fn new(token: impl Into<String>) -> Result<Self> {
        let http = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .build()
            .context("construyendo cliente HTTP")?;
        Ok(Self {
            http,
            token: token.into(),
        })
    }

    fn url(&self, path: &str) -> String {
        format!("{API_BASE}{path}")
    }

    /// Ejecuta una request con reintentos ante 429 (rate limit).
    async fn send(&self, build: impl Fn() -> reqwest::RequestBuilder) -> Result<reqwest::Response> {
        let mut attempt = 0;
        loop {
            let resp = build()
                .header("Authorization", &self.token) // token de usuario: sin "Bot "
                .send()
                .await
                .context("enviando request HTTP")?;

            let status = resp.status();
            if status.as_u16() == 429 && attempt < MAX_RETRIES {
                let retry = retry_after(&resp).await;
                tracing::warn!("rate limit (429); esperando {:.2}s", retry.as_secs_f64());
                tokio::time::sleep(retry).await;
                attempt += 1;
                continue;
            }
            if !status.is_success() {
                let body = resp.text().await.unwrap_or_default();
                bail!("HTTP {status}: {}", body.chars().take(300).collect::<String>());
            }
            return Ok(resp);
        }
    }

    /// `GET /users/@me` — valida el token y devuelve el usuario.
    pub async fn validate_token(&self) -> Result<User> {
        let resp = self.send(|| self.http.get(self.url("/users/@me"))).await?;
        resp.json::<User>().await.context("parseando @me")
    }

    /// `GET /users/@me/guilds` — servidores del usuario.
    pub async fn list_guilds(&self) -> Result<Vec<Guild>> {
        let resp = self
            .send(|| self.http.get(self.url("/users/@me/guilds")))
            .await?;
        resp.json::<Vec<Guild>>().await.context("parseando guilds")
    }

    /// `GET /guilds/{id}/channels` — canales de un servidor.
    pub async fn list_guild_channels(&self, guild_id: &str) -> Result<Vec<Channel>> {
        let resp = self
            .send(|| self.http.get(self.url(&format!("/guilds/{guild_id}/channels"))))
            .await?;
        resp.json::<Vec<Channel>>()
            .await
            .context("parseando canales del guild")
    }

    /// `GET /users/@me/channels` — DMs abiertos.
    pub async fn list_dms(&self) -> Result<Vec<Channel>> {
        let resp = self
            .send(|| self.http.get(self.url("/users/@me/channels")))
            .await?;
        resp.json::<Vec<Channel>>().await.context("parseando DMs")
    }

    /// `GET /channels/{id}` — metadatos de un canal concreto.
    pub async fn get_channel(&self, channel_id: &str) -> Result<Channel> {
        let resp = self
            .send(|| self.http.get(self.url(&format!("/channels/{channel_id}"))))
            .await?;
        resp.json::<Channel>().await.context("parseando canal")
    }

    /// `GET /channels/{id}/messages?limit=N` — historial reciente.
    /// Discord los devuelve del más nuevo al más viejo; aquí se invierten para
    /// presentarlos en orden cronológico.
    pub async fn get_channel_messages(&self, channel_id: &str, limit: u8) -> Result<Vec<Message>> {
        let limit = limit.clamp(1, 100);
        let resp = self
            .send(|| {
                self.http
                    .get(self.url(&format!("/channels/{channel_id}/messages")))
                    .query(&[("limit", limit.to_string())])
            })
            .await?;
        let mut msgs = resp
            .json::<Vec<Message>>()
            .await
            .context("parseando historial")?;
        msgs.reverse();
        Ok(msgs)
    }

    /// `POST /channels/{id}/messages` — envía un mensaje de texto.
    pub async fn create_message(&self, channel_id: &str, content: &str) -> Result<Message> {
        #[derive(Serialize)]
        struct Body<'a> {
            content: &'a str,
        }
        let body = Body { content };
        let resp = self
            .send(|| {
                self.http
                    .post(self.url(&format!("/channels/{channel_id}/messages")))
                    .json(&body)
            })
            .await?;
        resp.json::<Message>().await.context("parseando mensaje creado")
    }

    /// `POST /users/@me/channels` — abre (o recupera) un DM con un usuario.
    pub async fn open_dm(&self, recipient_id: &str) -> Result<Channel> {
        #[derive(Serialize)]
        struct Body<'a> {
            recipient_id: &'a str,
        }
        let body = Body { recipient_id };
        let resp = self
            .send(|| self.http.post(self.url("/users/@me/channels")).json(&body))
            .await?;
        resp.json::<Channel>().await.context("parseando DM abierto")
    }
}

/// Extrae el tiempo de espera de un 429 (cabecera o cuerpo JSON).
async fn retry_after(resp: &reqwest::Response) -> Duration {
    if let Some(v) = resp
        .headers()
        .get("retry-after")
        .and_then(|h| h.to_str().ok())
        .and_then(|s| s.parse::<f64>().ok())
    {
        return Duration::from_secs_f64(v.max(0.0) + 0.1);
    }
    // Fallback conservador si no viene la cabecera.
    Duration::from_secs(2)
}

/// Valida un token suelto (usado por la pantalla de login antes de guardarlo).
pub async fn validate(token: &str) -> Result<User> {
    RestClient::new(token)?.validate_token().await
}

/// Helper de error para mensajes claros al usuario.
pub fn friendly_auth_error(e: &anyhow::Error) -> String {
    let s = e.to_string();
    if s.contains("401") {
        "Token inválido o expirado.".to_string()
    } else {
        format!("Error de red/API: {}", s)
    }
}

#[allow(dead_code)]
fn _unused() -> anyhow::Error {
    anyhow!("placeholder")
}
