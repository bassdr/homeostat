//! Publishes the daemon's decision as an HA entity via MQTT discovery, plus
//! availability (via MQTT last-will: broker flips us to offline if we crash)
//! and a periodic heartbeat timestamp for staleness alerting.

use anyhow::Result;
use rumqttc::{AsyncClient, LastWill, MqttOptions, QoS};
use serde_json::json;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tracing::warn;

use crate::decide::Desired;
use crate::state::Inputs;

const TOPIC_STATUS: &str = "homeostat/status";
const TOPIC_STATE: &str = "homeostat/desired/state";
const TOPIC_ATTRS: &str = "homeostat/desired/attributes";
const TOPIC_HEARTBEAT: &str = "homeostat/heartbeat";

pub struct Mqtt {
    client: AsyncClient,
}

impl Mqtt {
    pub async fn connect(host: &str, port: u16, credentials: Option<(String, String)>) -> Result<Self> {
        let mut options = MqttOptions::new("homeostat", host, port);
        options.set_keep_alive(Duration::from_secs(30));
        options.set_last_will(LastWill::new(TOPIC_STATUS, "offline", QoS::AtLeastOnce, true));
        if let Some((user, pass)) = credentials {
            options.set_credentials(user, pass);
        }

        let (client, mut eventloop) = AsyncClient::new(options, 16);
        tokio::spawn(async move {
            loop {
                if let Err(e) = eventloop.poll().await {
                    warn!("mqtt event loop error: {e}");
                    tokio::time::sleep(Duration::from_secs(5)).await;
                }
            }
        });

        let mqtt = Self { client };
        mqtt.publish_discovery().await?;
        mqtt.client
            .publish(TOPIC_STATUS, QoS::AtLeastOnce, true, "online")
            .await?;
        Ok(mqtt)
    }

    /// MQTT discovery config: creates sensor.homeostat_desired in HA with the
    /// decision attributes, marked unavailable whenever the daemon is down.
    async fn publish_discovery(&self) -> Result<()> {
        let config = json!({
            "name": "homeostat_desired",
            "unique_id": "homeostat_desired",
            "state_topic": TOPIC_STATE,
            "json_attributes_topic": TOPIC_ATTRS,
            "availability_topic": TOPIC_STATUS,
            "icon": "mdi:robot",
        });
        self.client
            .publish(
                "homeassistant/sensor/homeostat_desired/config",
                QoS::AtLeastOnce,
                true,
                config.to_string(),
            )
            .await?;
        Ok(())
    }

    pub async fn publish_desired(&self, desired: &Desired, inputs: &Inputs) -> Result<()> {
        let state = format!(
            "{}/{}/{}",
            desired.main_mode.as_str(),
            desired.main_setpoint,
            desired.fan_mode.as_str()
        );
        let attrs = json!({
            "main_mode": desired.main_mode.as_str(),
            "main_setpoint": desired.main_setpoint,
            "fan_mode": desired.fan_mode.as_str(),
            "basement_setpoint": desired.basement_setpoint,
            "water_heater": if desired.water_heater_on { "on" } else { "off" },
            "inputs": format!("{inputs:?}"),
        });
        self.client
            .publish(TOPIC_STATE, QoS::AtLeastOnce, true, state)
            .await?;
        self.client
            .publish(TOPIC_ATTRS, QoS::AtLeastOnce, true, attrs.to_string())
            .await?;
        Ok(())
    }

    pub async fn heartbeat(&self) -> Result<()> {
        let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
        self.client
            .publish(TOPIC_HEARTBEAT, QoS::AtLeastOnce, true, now.to_string())
            .await?;
        Ok(())
    }
}
