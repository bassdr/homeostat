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
pub const ENTITY_RETURN_ETA: &str = "sensor.homeostat_return_eta";
pub const ENTITY_RETURN_FLOOR: &str = "sensor.homeostat_return_floor";

/// What the occupancy sensor now publishes: presence facts only. The
/// away_returning/away_far distinction moved out of perception - it is
/// derived here from the expected-return sensors (see `Occupancy::derive`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Presence {
    Home,
    HomeAsleep,
    Away,
}

impl Presence {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "home" => Some(Self::Home),
            "home_asleep" => Some(Self::HomeAsleep),
            "away" => Some(Self::Away),
            // the legacy five-state values are deliberately not accepted:
            // deploy the perception package before this daemon version
            _ => None,
        }
    }
}

/// Minutes-until-return below which an away house is treated as "returning"
/// (start recovery). Matches the old nav rule of ETA < 60 min.
const RETURNING_LEAD_MIN: f64 = 60.0;
/// Minutes-until-return at or beyond which the deep away_far setback
/// applies ("cannot be back within a couple of hours"). Note: from the
/// distance floor alone (100 km/h assumed) this kicks in at ~200 km,
/// vs. the old away_far_distance knob's 100 km default.
const FAR_MIN: f64 = 120.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Occupancy {
    Home,
    HomeAsleep,
    AwayReturning,
    Away,
    AwayFar,
}

impl Occupancy {
    /// Classify away-ness from time-until-return. Two perception facts:
    ///
    /// - `return_eta_min`: a credible estimate (nav ETA, manual/calendar);
    ///   0 = no estimate (same convention as the comfort hold).
    /// - `return_floor_min`: physical lower bound from GPS distance; always
    ///   valid, 0 when unknown (a floor of zero is vacuously true).
    ///
    /// The floor caps optimism (`max`): a nav ETA that contradicts raw
    /// distance is stale. Returning requires a positive estimate - a small
    /// *floor* only means "could be nearby", not "is coming back".
    pub fn derive(presence: Presence, return_eta_min: f64, return_floor_min: f64) -> Self {
        match presence {
            Presence::Home => Self::Home,
            Presence::HomeAsleep => Self::HomeAsleep,
            Presence::Away => {
                let has_estimate = return_eta_min > 0.0;
                let expected = return_eta_min.max(return_floor_min);
                if expected >= FAR_MIN {
                    Self::AwayFar
                } else if has_estimate && expected <= RETURNING_LEAD_MIN {
                    Self::AwayReturning
                } else {
                    Self::Away
                }
            }
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
    presence: Option<Presence>,
    energy_period: Option<EnergyPeriod>,
    season: Option<Season>,
    aux_zone_occupied: Option<bool>,
    /// Plain f64, not Option: these inputs are optional by design -
    /// unavailable/unknown means "no hold" / "no estimate" and must not
    /// suspend decisions.
    comfort_setpoint: f64,
    return_eta_min: f64,
    return_floor_min: f64,
}

impl RawInputs {
    /// Ingest a state update. Returns true if the update was for an entity we
    /// track and changed its slot.
    pub fn ingest(&mut self, entity_id: &str, state: &str) -> bool {
        match entity_id {
            ENTITY_OCCUPANCY => Self::set(&mut self.presence, Presence::parse(state)),
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
            ENTITY_COMFORT_SETPOINT => Self::set_f64(&mut self.comfort_setpoint, state),
            ENTITY_RETURN_ETA => Self::set_f64(&mut self.return_eta_min, state),
            ENTITY_RETURN_FLOOR => Self::set_f64(&mut self.return_floor_min, state),
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

    fn set_f64(slot: &mut f64, state: &str) -> bool {
        let value = state.parse::<f64>().unwrap_or(0.0);
        if *slot == value {
            false
        } else {
            *slot = value;
            true
        }
    }

    pub fn complete(&self) -> Option<Inputs> {
        Some(Inputs {
            occupancy: Occupancy::derive(
                self.presence?,
                self.return_eta_min,
                self.return_floor_min,
            ),
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

    #[test]
    fn legacy_five_state_occupancy_suspends() {
        // the perception package must be deployed before this daemon
        // version; a legacy state means it was not - suspend, don't guess
        let mut raw = RawInputs::default();
        raw.ingest(ENTITY_OCCUPANCY, "away_returning");
        raw.ingest(ENTITY_ENERGY_PERIOD, "normal");
        raw.ingest(ENTITY_SEASON, "heat");
        raw.ingest(ENTITY_AUX_ZONE_OCCUPIED, "off");
        assert!(raw.complete().is_none());
    }

    #[test]
    fn derive_classifies_away_by_time_until_return() {
        use Occupancy::*;
        use Presence::Away as PAway;

        // presence at home ignores the return sensors entirely
        assert_eq!(Occupancy::derive(Presence::Home, 30.0, 0.0), Home);
        assert_eq!(
            Occupancy::derive(Presence::HomeAsleep, 0.0, 500.0),
            HomeAsleep
        );

        // a credible estimate within the recovery lead = returning,
        // inclusive at the boundary (the old nav rule was ETA < 60)
        assert_eq!(Occupancy::derive(PAway, 45.0, 0.0), AwayReturning);
        assert_eq!(Occupancy::derive(PAway, 60.0, 0.0), AwayReturning);
        assert_eq!(Occupancy::derive(PAway, 90.0, 0.0), Away);
        assert_eq!(Occupancy::derive(PAway, 240.0, 0.0), AwayFar);

        // no estimate: never returning; plain away unless the physical
        // floor proves the deep setback is safe
        assert_eq!(Occupancy::derive(PAway, 0.0, 0.0), Away);
        assert_eq!(Occupancy::derive(PAway, 0.0, 45.0), Away);
        assert_eq!(Occupancy::derive(PAway, 0.0, 180.0), AwayFar);

        // the floor caps a stale optimistic estimate
        assert_eq!(Occupancy::derive(PAway, 30.0, 180.0), AwayFar);
        assert_eq!(Occupancy::derive(PAway, 30.0, 90.0), Away);
    }

    #[test]
    fn unavailable_return_sensors_mean_no_estimate_and_do_not_suspend() {
        let mut raw = RawInputs::default();
        raw.ingest(ENTITY_OCCUPANCY, "away");
        raw.ingest(ENTITY_ENERGY_PERIOD, "normal");
        raw.ingest(ENTITY_SEASON, "heat");
        raw.ingest(ENTITY_AUX_ZONE_OCCUPIED, "off");
        raw.ingest(ENTITY_RETURN_ETA, "unavailable");
        raw.ingest(ENTITY_RETURN_FLOOR, "unknown");
        let inputs = raw
            .complete()
            .expect("optional inputs must not suspend decisions");
        assert_eq!(inputs.occupancy, Occupancy::Away, "no data = conservative");
    }
}
