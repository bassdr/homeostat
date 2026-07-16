//! Minimal Home Assistant WebSocket client: auth, get_states, event
//! subscription, service calls. The message zoo is parsed into a typed enum
//! at the boundary — unknown messages are logged, never silently acted on.

use anyhow::{bail, Context, Result};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::json;
use tokio::net::TcpStream;
use tokio_tungstenite::{connect_async, tungstenite::Message, MaybeTlsStream, WebSocketStream};
use tracing::{debug, info, warn};

use crate::decide::ServiceCall;

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ServerMsg {
    AuthRequired,
    AuthOk,
    AuthInvalid { message: String },
    Result { id: u64, success: bool },
    Event { event: EventBody },
    Pong,
}

#[derive(Debug, Deserialize)]
struct EventBody {
    event_type: String,
    data: serde_json::Value,
}

#[derive(Debug, Deserialize)]
pub struct EntityState {
    pub entity_id: String,
    pub state: String,
}

/// A state change relevant to us: (entity_id, new_state).
pub type StateChange = (String, String);

pub struct HaClient {
    ws: WebSocketStream<MaybeTlsStream<TcpStream>>,
    next_id: u64,
}

impl HaClient {
    pub async fn connect(url: &str, token: &str) -> Result<Self> {
        let (ws, _) = connect_async(url)
            .await
            .with_context(|| format!("connecting to {url}"))?;
        let mut client = Self { ws, next_id: 0 };

        match client.recv().await? {
            ServerMsg::AuthRequired => {}
            other => bail!("expected auth_required, got {other:?}"),
        }
        client
            .send_raw(json!({ "type": "auth", "access_token": token }))
            .await?;
        match client.recv().await? {
            ServerMsg::AuthOk => info!("authenticated to Home Assistant"),
            ServerMsg::AuthInvalid { message } => bail!("auth rejected: {message}"),
            other => bail!("expected auth_ok, got {other:?}"),
        }
        Ok(client)
    }

    /// Fetch all current states (used to seed the input snapshot at startup).
    pub async fn get_states(&mut self) -> Result<Vec<EntityState>> {
        let id = self.send(json!({ "type": "get_states" })).await?;
        loop {
            let raw = self.recv_raw().await?;
            let msg: ServerMsg = serde_json::from_str(&raw).context("parsing get_states reply")?;
            if let ServerMsg::Result { id: rid, success } = msg {
                if rid == id {
                    if !success {
                        bail!("get_states failed");
                    }
                    #[derive(Deserialize)]
                    struct ResultMsg {
                        result: Vec<EntityState>,
                    }
                    let parsed: ResultMsg = serde_json::from_str(&raw)?;
                    return Ok(parsed.result);
                }
            }
        }
    }

    pub async fn subscribe_state_changed(&mut self) -> Result<()> {
        self.send(json!({ "type": "subscribe_events", "event_type": "state_changed" }))
            .await?;
        Ok(())
    }

    /// Next state_changed event. Skips unrelated messages; logs failed
    /// service call results instead of dropping them silently.
    pub async fn next_state_change(&mut self) -> Result<StateChange> {
        loop {
            match self.recv().await? {
                ServerMsg::Event { event } if event.event_type == "state_changed" => {
                    #[derive(Deserialize)]
                    struct ChangeData {
                        entity_id: String,
                        new_state: Option<NewState>,
                    }
                    #[derive(Deserialize)]
                    struct NewState {
                        state: String,
                    }
                    let data: ChangeData = serde_json::from_value(event.data)
                        .context("parsing state_changed data")?;
                    if let Some(new_state) = data.new_state {
                        return Ok((data.entity_id, new_state.state));
                    }
                    // entity removed; ignore
                }
                ServerMsg::Result { id, success } => {
                    if !success {
                        warn!("service call (id {id}) failed");
                    }
                }
                other => debug!("ignoring message: {other:?}"),
            }
        }
    }

    pub async fn call_service(&mut self, call: &ServiceCall) -> Result<()> {
        info!(
            "call_service {}.{} -> {:?} {}",
            call.domain, call.service, call.entity_ids, call.data
        );
        self.send(json!({
            "type": "call_service",
            "domain": call.domain,
            "service": call.service,
            "target": { "entity_id": call.entity_ids },
            "service_data": call.data,
        }))
        .await?;
        Ok(())
    }

    async fn send(&mut self, mut payload: serde_json::Value) -> Result<u64> {
        self.next_id += 1;
        payload["id"] = json!(self.next_id);
        self.send_raw(payload).await?;
        Ok(self.next_id)
    }

    async fn send_raw(&mut self, payload: serde_json::Value) -> Result<()> {
        self.ws.send(Message::Text(payload.to_string())).await?;
        Ok(())
    }

    async fn recv(&mut self) -> Result<ServerMsg> {
        let raw = self.recv_raw().await?;
        serde_json::from_str(&raw).with_context(|| format!("parsing server message: {raw}"))
    }

    async fn recv_raw(&mut self) -> Result<String> {
        loop {
            match self.ws.next().await {
                Some(Ok(Message::Text(text))) => return Ok(text),
                Some(Ok(Message::Ping(_) | Message::Pong(_))) => continue,
                Some(Ok(Message::Close(frame))) => bail!("server closed connection: {frame:?}"),
                Some(Ok(other)) => debug!("ignoring non-text frame: {other:?}"),
                Some(Err(e)) => return Err(e.into()),
                None => bail!("connection closed"),
            }
        }
    }
}
