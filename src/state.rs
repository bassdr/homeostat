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
pub const ENTITY_BACK_DURING_RECOVERY: &str = "binary_sensor.homeostat_back_during_recovery";
pub const ENTITY_RECOVERY_MINUTES: &str = "sensor.homeostat_recovery_minutes";

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

/// Minutes-until-return below which an away house is treated as "returning".
/// This is the comfort pre-start (heater/cooler on ~20 min before arrival;
/// finishing the warm-up while home is acceptable). From a cold house the
/// lead is stretched by the recovery estimate instead - see `derive`.
const RETURNING_LEAD_MIN: f64 = 20.0;
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
    /// - `return_eta_min`: a credible estimate; 0 = no estimate (the
    ///   comfort-hold convention). Perception emits one only when return is
    ///   confirmed (heading home: worst case, home in floor minutes) or
    ///   announced (manual/calendar).
    /// - `return_floor_min`: earliest possible arrival (phone travel time,
    ///   or GPS distance at highway speed); always valid, 0 when unknown
    ///   (a floor of zero is vacuously true).
    ///
    /// The floor caps optimism (`max`): an estimate that contradicts it is
    /// stale. Returning requires a positive estimate - a small *floor* only
    /// means "could be nearby" (20 min from the girlfriend's couch), never
    /// "is coming back".
    ///
    /// `recovery_min` is how long the house needs to warm back to *livable*
    /// (not full comfort) from its current temperature (perception
    /// estimates it; 0 = warm enough or unknown). It stretches the
    /// returning lead: from a deep-cold cycle (12C after a shed peak)
    /// recovery takes hours, and arriving anywhere inside that window to a
    /// cold house is a comfort failure - so the returning check runs first
    /// and outranks Far. From the normal 19C away baseline it contributes
    /// nothing and the plain comfort pre-start applies.
    pub fn derive(
        presence: Presence,
        return_eta_min: f64,
        return_floor_min: f64,
        recovery_min: f64,
    ) -> Self {
        match presence {
            Presence::Home => Self::Home,
            Presence::HomeAsleep => Self::HomeAsleep,
            Presence::Away => {
                let has_estimate = return_eta_min > 0.0;
                let expected = return_eta_min.max(return_floor_min);
                let lead = recovery_min.max(RETURNING_LEAD_MIN);
                if has_estimate && expected <= lead {
                    Self::AwayReturning
                } else if expected >= FAR_MIN {
                    Self::AwayFar
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
    /// Someone is expected home during the grid event or within the
    /// recovery horizon after it ends. Unknown = true: the comfort-safe
    /// default is a normal preheat; the bare-preheat saving needs positive
    /// evidence of absence (calendar/manual estimate, or a huge floor).
    pub back_during_recovery: bool,
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
    recovery_min: f64,
    /// Optional with an asymmetric default: unknown means "assume someone
    /// will be back" (normal preheat, the comfort-safe direction).
    back_during_recovery: Option<bool>,
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
            ENTITY_RECOVERY_MINUTES => Self::set_f64(&mut self.recovery_min, state),
            ENTITY_BACK_DURING_RECOVERY => Self::set(
                &mut self.back_during_recovery,
                match state {
                    "on" => Some(true),
                    "off" => Some(false),
                    _ => None,
                },
            ),
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
                self.recovery_min,
            ),
            energy_period: self.energy_period?,
            season: self.season?,
            aux_zone_occupied: self.aux_zone_occupied?,
            comfort_setpoint: self.comfort_setpoint,
            back_during_recovery: self.back_during_recovery.unwrap_or(true),
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
        assert_eq!(Occupancy::derive(Presence::Home, 30.0, 0.0, 0.0), Home);
        assert_eq!(
            Occupancy::derive(Presence::HomeAsleep, 0.0, 500.0, 0.0),
            HomeAsleep
        );

        // a credible estimate within the comfort pre-start = returning,
        // inclusive at the boundary
        assert_eq!(Occupancy::derive(PAway, 15.0, 0.0, 0.0), AwayReturning);
        assert_eq!(Occupancy::derive(PAway, 20.0, 0.0, 0.0), AwayReturning);
        assert_eq!(Occupancy::derive(PAway, 45.0, 0.0, 0.0), Away);
        assert_eq!(Occupancy::derive(PAway, 240.0, 0.0, 0.0), AwayFar);

        // no estimate: never returning; plain away unless the physical
        // floor proves the deep setback is safe
        assert_eq!(Occupancy::derive(PAway, 0.0, 0.0, 0.0), Away);
        assert_eq!(Occupancy::derive(PAway, 0.0, 45.0, 0.0), Away);
        assert_eq!(Occupancy::derive(PAway, 0.0, 180.0, 0.0), AwayFar);

        // the floor caps a stale optimistic estimate
        assert_eq!(Occupancy::derive(PAway, 15.0, 180.0, 0.0), AwayFar);
        assert_eq!(Occupancy::derive(PAway, 15.0, 90.0, 0.0), Away);
    }

    #[test]
    fn cold_house_stretches_the_returning_lead() {
        use Occupancy::*;
        use Presence::Away as PAway;

        // house at 12C after a shed peak: recovery ~210 min. Returning
        // must engage hours out - even from "far" - or the arrival lands
        // in a cold house; recovery outranks the far setback.
        assert_eq!(Occupancy::derive(PAway, 180.0, 0.0, 210.0), AwayReturning);
        assert_eq!(Occupancy::derive(PAway, 180.0, 170.0, 210.0), AwayReturning);
        // beyond the stretched lead the normal buckets apply
        assert_eq!(Occupancy::derive(PAway, 240.0, 0.0, 210.0), AwayFar);
        // no estimate: a cold house alone never fakes a return
        assert_eq!(Occupancy::derive(PAway, 0.0, 0.0, 210.0), Away);
        // warm house: the lead is the plain comfort pre-start
        assert_eq!(Occupancy::derive(PAway, 45.0, 0.0, 0.0), Away);
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
        raw.ingest(ENTITY_RECOVERY_MINUTES, "unavailable");
        let inputs = raw
            .complete()
            .expect("optional inputs must not suspend decisions");
        assert_eq!(inputs.occupancy, Occupancy::Away, "no data = conservative");
    }

    #[test]
    fn unknown_back_during_recovery_defaults_to_assuming_a_return() {
        let mut raw = RawInputs::default();
        raw.ingest(ENTITY_OCCUPANCY, "away");
        raw.ingest(ENTITY_ENERGY_PERIOD, "preheat");
        raw.ingest(ENTITY_SEASON, "heat");
        raw.ingest(ENTITY_AUX_ZONE_OCCUPIED, "off");
        let inputs = raw.complete().unwrap();
        assert!(
            inputs.back_during_recovery,
            "unknown must fail toward the comfort-safe normal preheat"
        );

        raw.ingest(ENTITY_BACK_DURING_RECOVERY, "off");
        assert!(!raw.complete().unwrap().back_during_recovery);
        raw.ingest(ENTITY_BACK_DURING_RECOVERY, "unavailable");
        assert!(raw.complete().unwrap().back_during_recovery);
    }
}
