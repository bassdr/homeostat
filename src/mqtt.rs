//! Publishes the daemon's decision as HA entities via MQTT discovery, plus
//! availability (via MQTT last-will: broker flips everything to unavailable
//! if we crash) and a periodic heartbeat timestamp for staleness alerting.
//!
//! Two views of the same decision are published:
//! - `sensor.homeostat_desired`: the combined mode/setpoint/fan string. Its
//!   single state change is what the main-zone wire automation triggers on,
//!   so the triple is always forwarded together (the 2026-07-07 contract).
//! - Per-value entities (setpoint as a real temperature sensor, etc.) for
//!   history graphs and for the single-value wires (aux zone, load shed).

use anyhow::Result;
use rumqttc::{AsyncClient, LastWill, MqttOptions, QoS};
use serde_json::{json, Value};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tracing::warn;

use crate::decide::Desired;
use crate::state::Inputs;

const TOPIC_STATUS: &str = "homeostat/status";
const TOPIC_STATE: &str = "homeostat/desired/state";
const TOPIC_ATTRS: &str = "homeostat/desired/attributes";
const TOPIC_MAIN_SETPOINT: &str = "homeostat/desired/main_setpoint";
const TOPIC_MAIN_MODE: &str = "homeostat/desired/main_mode";
const TOPIC_FAN_MODE: &str = "homeostat/desired/fan_mode";
const TOPIC_AUX_SETPOINT: &str = "homeostat/desired/aux_zone_setpoint";
const TOPIC_LOAD_SHED: &str = "homeostat/desired/load_shed";
const TOPIC_HEARTBEAT: &str = "homeostat/heartbeat";

pub struct Mqtt {
    client: AsyncClient,
}

impl Mqtt {
    pub async fn connect(
        host: &str,
        port: u16,
        credentials: Option<(String, String)>,
    ) -> Result<Self> {
        let mut options = MqttOptions::new("homeostat", host, port);
        options.set_keep_alive(Duration::from_secs(30));
        options.set_last_will(LastWill::new(
            TOPIC_STATUS,
            "offline",
            QoS::AtLeastOnce,
            true,
        ));
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

    /// MQTT discovery: one combined sensor (the wires' atomic trigger) plus
    /// per-value entities, all grouped under a single "Homeostat" device and
    /// marked unavailable together whenever the daemon is down.
    async fn publish_discovery(&self) -> Result<()> {
        let device = json!({
            "identifiers": ["homeostat"],
            "name": "Homeostat",
            "sw_version": env!("CARGO_PKG_VERSION"),
        });
        // entity_id is composed by HA as <domain>.<device>_<slug(name)>, so
        // names must not repeat the homeostat_ prefix - the device supplies
        // it. unique_id is the registry key and must never change, or the
        // entity loses its identity and history.
        let entity = |name: &str, unique_id: &str, state_topic: &str, extra: Value| -> Value {
            let mut config = json!({
                "name": name,
                "unique_id": unique_id,
                "state_topic": state_topic,
                "availability_topic": TOPIC_STATUS,
                "device": device,
            });
            config
                .as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            config
        };
        let temperature = json!({
            "device_class": "temperature",
            "unit_of_measurement": "°C",
            "state_class": "measurement",
        });

        let configs = [
            (
                "sensor",
                entity(
                    "desired",
                    "homeostat_desired",
                    TOPIC_STATE,
                    json!({ "json_attributes_topic": TOPIC_ATTRS, "icon": "mdi:robot" }),
                ),
            ),
            (
                "sensor",
                entity(
                    "desired main setpoint",
                    "homeostat_desired_main_setpoint",
                    TOPIC_MAIN_SETPOINT,
                    temperature.clone(),
                ),
            ),
            (
                "sensor",
                entity(
                    "desired main mode",
                    "homeostat_desired_main_mode",
                    TOPIC_MAIN_MODE,
                    json!({ "icon": "mdi:sun-snowflake-variant" }),
                ),
            ),
            (
                "sensor",
                entity(
                    "desired fan mode",
                    "homeostat_desired_fan_mode",
                    TOPIC_FAN_MODE,
                    json!({ "icon": "mdi:fan" }),
                ),
            ),
            // 0 = zone off (same convention as the comfort hold: 0 = none)
            (
                "sensor",
                entity(
                    "desired aux zone setpoint",
                    "homeostat_desired_aux_zone_setpoint",
                    TOPIC_AUX_SETPOINT,
                    temperature,
                ),
            ),
            (
                "binary_sensor",
                entity(
                    "desired load shed",
                    "homeostat_desired_load_shed",
                    TOPIC_LOAD_SHED,
                    json!({ "payload_on": "on", "payload_off": "off", "icon": "mdi:transmission-tower-off" }),
                ),
            ),
        ];

        for (component, config) in configs {
            let object_id = config["unique_id"].as_str().unwrap().to_owned();
            self.client
                .publish(
                    format!("homeassistant/{component}/{object_id}/config"),
                    QoS::AtLeastOnce,
                    true,
                    config.to_string(),
                )
                .await?;
        }

        // Retired outputs: an empty retained payload on the old discovery
        // topic makes HA delete the entity (registry entry included).
        for topic in ["homeassistant/binary_sensor/homeostat_desired_water_heater/config"] {
            self.client
                .publish(topic, QoS::AtLeastOnce, true, "")
                .await?;
        }
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
            "aux_zone_setpoint": desired.aux_zone_setpoint,
            "shed_loads": if desired.shed_loads { "on" } else { "off" },
            "inputs": format!("{inputs:?}"),
        });
        let shed = if desired.shed_loads { "on" } else { "off" };

        // per-value topics first, the combined state last: by the time the
        // main wire fires on the combined change, every value is current
        for (topic, payload) in [
            (TOPIC_MAIN_SETPOINT, desired.main_setpoint.to_string()),
            (TOPIC_MAIN_MODE, desired.main_mode.as_str().to_owned()),
            (TOPIC_FAN_MODE, desired.fan_mode.as_str().to_owned()),
            (
                TOPIC_AUX_SETPOINT,
                desired.aux_zone_setpoint.unwrap_or(0.0).to_string(),
            ),
            (TOPIC_LOAD_SHED, shed.to_owned()),
            (TOPIC_ATTRS, attrs.to_string()),
            (TOPIC_STATE, state),
        ] {
            self.client
                .publish(topic, QoS::AtLeastOnce, true, payload)
                .await?;
        }
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
