//! Layer-1 inputs: the perception enums computed by HA template sensors
//! (configuration.d/homeostat.yaml). The daemon consumes them as entities and
//! parses them into real types at the boundary — an unknown/unavailable
//! state clears the slot and suspends decisions instead of acting on junk.

/// Entities the daemon subscribes to (perception layer, computed in HA).
pub const ENTITY_OCCUPANCY: &str = "sensor.homeostat_occupancy";
pub const ENTITY_ENERGY_PERIOD: &str = "sensor.homeostat_energy_period";
pub const ENTITY_SEASON: &str = "sensor.homeostat_season";
pub const ENTITY_AUX_ZONE_OCCUPIED: &str = "binary_sensor.homeostat_aux_zone_occupied";
pub const ENTITY_COMFORT_SETPOINT: &str = "input_number.homeostat_comfort_setpoint";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Occupancy {
    Home,
    HomeAsleep,
    AwayReturning,
    Away,
    AwayFar,
}

impl Occupancy {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "home" => Some(Self::Home),
            "home_asleep" => Some(Self::HomeAsleep),
            "away_returning" => Some(Self::AwayReturning),
            "away" => Some(Self::Away),
            "away_far" => Some(Self::AwayFar),
            _ => None,
        }
    }

    pub fn is_home(self) -> bool {
        matches!(self, Self::Home | Self::HomeAsleep)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnergyPeriod {
    Normal,
    Preheat,
    Peak,
}

impl EnergyPeriod {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "normal" => Some(Self::Normal),
            "preheat" => Some(Self::Preheat),
            "peak" => Some(Self::Peak),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Season {
    Heat,
    Fan,
    Cool,
}

impl Season {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "heat" => Some(Self::Heat),
            "fan" => Some(Self::Fan),
            "cool" => Some(Self::Cool),
            _ => None,
        }
    }
}

/// Complete, validated snapshot of the perception layer.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Inputs {
    pub occupancy: Occupancy,
    pub energy_period: EnergyPeriod,
    pub season: Season,
    pub aux_zone_occupied: bool,
    /// Manual hold: absolute setpoint in degrees C; 0 = none (automatic).
    pub comfort_setpoint: f64,
}

/// Accumulates entity states as they arrive; yields `Inputs` only once every
/// slot holds a valid value. `unknown`/`unavailable` clears the slot.
#[derive(Debug, Default)]
pub struct RawInputs {
    occupancy: Option<Occupancy>,
    energy_period: Option<EnergyPeriod>,
    season: Option<Season>,
    aux_zone_occupied: Option<bool>,
    /// Plain f64, not Option: this input is optional by design -
    /// unavailable/unknown means "no hold" and must not suspend decisions.
    comfort_setpoint: f64,
}

impl RawInputs {
    /// Ingest a state update. Returns true if the update was for an entity we
    /// track and changed its slot.
    pub fn ingest(&mut self, entity_id: &str, state: &str) -> bool {
        match entity_id {
            ENTITY_OCCUPANCY => Self::set(&mut self.occupancy, Occupancy::parse(state)),
            ENTITY_ENERGY_PERIOD => Self::set(&mut self.energy_period, EnergyPeriod::parse(state)),
            ENTITY_SEASON => Self::set(&mut self.season, Season::parse(state)),
            ENTITY_AUX_ZONE_OCCUPIED => Self::set(
                &mut self.aux_zone_occupied,
                match state {
                    "on" => Some(true),
                    "off" => Some(false),
                    _ => None,
                },
            ),
            ENTITY_COMFORT_SETPOINT => {
                let value = state.parse::<f64>().unwrap_or(0.0);
                if self.comfort_setpoint == value {
                    false
                } else {
                    self.comfort_setpoint = value;
                    true
                }
            }
            _ => false,
        }
    }

    fn set<T: PartialEq>(slot: &mut Option<T>, value: Option<T>) -> bool {
        if *slot == value {
            false
        } else {
            *slot = value;
            true
        }
    }

    pub fn complete(&self) -> Option<Inputs> {
        Some(Inputs {
            occupancy: self.occupancy?,
            energy_period: self.energy_period?,
            season: self.season?,
            aux_zone_occupied: self.aux_zone_occupied?,
            comfort_setpoint: self.comfort_setpoint,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unavailable_comfort_hold_means_no_hold_and_does_not_suspend() {
        let mut raw = RawInputs::default();
        raw.ingest(ENTITY_OCCUPANCY, "home");
        raw.ingest(ENTITY_ENERGY_PERIOD, "normal");
        raw.ingest(ENTITY_SEASON, "heat");
        raw.ingest(ENTITY_AUX_ZONE_OCCUPIED, "off");
        raw.ingest(ENTITY_COMFORT_SETPOINT, "unavailable");
        let inputs = raw
            .complete()
            .expect("an optional input must not suspend decisions");
        assert_eq!(inputs.comfort_setpoint, 0.0);
    }

    #[test]
    fn unknown_occupancy_suspends_decisions() {
        let mut raw = RawInputs::default();
        raw.ingest(ENTITY_OCCUPANCY, "unknown");
        raw.ingest(ENTITY_ENERGY_PERIOD, "normal");
        raw.ingest(ENTITY_SEASON, "heat");
        raw.ingest(ENTITY_AUX_ZONE_OCCUPIED, "off");
        assert!(
            raw.complete().is_none(),
            "garbage in a required input must suspend"
        );
    }
}
