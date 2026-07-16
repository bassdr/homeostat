mod decide;
mod ha;
mod mqtt;
mod state;

use anyhow::{Context, Result};
use std::time::Duration;
use tracing::{error, info, warn};

use decide::{decide, Desired};
use state::RawInputs;

struct Config {
    ha_url: String,
    ha_token: String,
    mqtt_host: String,
    mqtt_port: u16,
    mqtt_credentials: Option<(String, String)>,
}

impl Config {
    fn from_env() -> Result<Self> {
        Ok(Self {
            ha_url: std::env::var("HOMEOSTAT_HA_URL")
                .unwrap_or_else(|_| "ws://127.0.0.1:8123/api/websocket".into()),
            ha_token: std::env::var("HOMEOSTAT_HA_TOKEN")
                .context("HOMEOSTAT_HA_TOKEN is required (long-lived access token)")?,
            mqtt_host: std::env::var("HOMEOSTAT_MQTT_HOST").unwrap_or_else(|_| "127.0.0.1".into()),
            mqtt_port: std::env::var("HOMEOSTAT_MQTT_PORT")
                .ok()
                .and_then(|p| p.parse().ok())
                .unwrap_or(1883),
            mqtt_credentials: match (
                std::env::var("HOMEOSTAT_MQTT_USER"),
                std::env::var("HOMEOSTAT_MQTT_PASS"),
            ) {
                (Ok(user), Ok(pass)) => Some((user, pass)),
                _ => None,
            },
        })
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "homeostat=info".into()),
        )
        .init();

    let config = Config::from_env()?;
    info!("starting homeostat -> {}", config.ha_url);
    info!("decisions are published to MQTT; actuation is HA's job (wire automations)");

    let mqtt = mqtt::Mqtt::connect(
        &config.mqtt_host,
        config.mqtt_port,
        config.mqtt_credentials.clone(),
    )
    .await
    .context("connecting to MQTT broker")?;

    let mut backoff = Duration::from_secs(1);
    loop {
        match run(&config, &mqtt).await {
            Ok(()) => return Ok(()), // clean shutdown (ctrl-c)
            Err(e) => {
                error!("connection lost: {e:#}");
                warn!("reconnecting in {backoff:?}");
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(Duration::from_secs(60));
            }
        }
    }
}

async fn run(config: &Config, mqtt: &mqtt::Mqtt) -> Result<()> {
    let mut ha = ha::HaClient::connect(&config.ha_url, &config.ha_token).await?;

    // Seed the perception snapshot, then follow live changes.
    let mut raw = RawInputs::default();
    for entity in ha.get_states().await? {
        raw.ingest(&entity.entity_id, &entity.state);
    }
    ha.subscribe_state_changed().await?;

    let mut last_published: Option<Desired> = None;
    let mut heartbeat = tokio::time::interval(Duration::from_secs(60));

    // Decide immediately from the seeded snapshot.
    step(&mut raw, &mut last_published, mqtt).await?;

    loop {
        tokio::select! {
            _ = heartbeat.tick() => mqtt.heartbeat().await?,
            _ = tokio::signal::ctrl_c() => {
                info!("shutting down");
                return Ok(());
            }
            change = ha.next_state_change() => {
                let (entity_id, new_state) = change?;
                if raw.ingest(&entity_id, &new_state) {
                    info!("input changed: {entity_id} = {new_state}");
                    step(&mut raw, &mut last_published, mqtt).await?;
                }
            }
        }
    }
}

async fn step(
    raw: &mut RawInputs,
    last_published: &mut Option<Desired>,
    mqtt: &mqtt::Mqtt,
) -> Result<()> {
    let Some(inputs) = raw.complete() else {
        warn!("perception layer incomplete, holding decisions: {raw:?}");
        return Ok(());
    };

    let desired = decide(&inputs);
    if last_published.as_ref() == Some(&desired) {
        return Ok(());
    }

    info!("decision: {desired:?} (from {inputs:?})");
    mqtt.publish_desired(&desired, &inputs).await?;
    *last_published = Some(desired);
    Ok(())
}
