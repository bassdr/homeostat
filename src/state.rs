//! Layer-1 inputs: the perception enums computed by HA template sensors
//! (configuration.d/homeostat.yaml). The daemon consumes them as entities and
//! parses them into real types at the boundary — an unknown/unavailable
//! state clears the slot and suspends decisions instead of acting on junk.

/// Entities the daemon subscribes to (perception layer, computed in HA).
pub const ENTITY_OCCUPANCY: &str = "sensor.homeostat_occupancy";
pub const ENTITY_ENERGY_PERIOD: &str = "sensor.homeostat_energy_period";
// Today's forecast high, in C. A fact, not a verdict: the daemon owns the
// thresholds that turn it into a demanded mode (decide.rs::demanded_mode),
// so every policy temperature stays in one file. Required - a missing
// forecast suspends decisions rather than fabricating a mild day.
pub const ENTITY_FORECAST_MAX: &str = "sensor.homeostat_forecast_max_today";
// Drill knob: force the demanded mode to exercise the matrix out of season
// ("simulate winter in July"). Optional - `auto`, missing, unknown and
// unavailable all mean "follow the forecast".
pub const ENTITY_MAIN_MODE_OVERRIDE: &str = "sensor.homeostat_main_mode_override";
pub const ENTITY_AUX_ZONE_OCCUPIED: &str = "binary_sensor.homeostat_aux_zone_occupied";
// Manual comfort holds are enforced by the HA override (the wire stands
// down; the human's setpoint persists in the device), so the daemon does
// NOT subscribe to any comfort_setpoint entity - `desired` stays the pure
// matrix decision and the held value lives in HA only, as a record.
pub const ENTITY_RETURN_ETA: &str = "sensor.homeostat_return_eta";
pub const ENTITY_RETURN_FLOOR: &str = "sensor.homeostat_return_floor";
// The returning lead is the time to warm the house to what a return would
// command, so it needs where the house is and how fast it warms. Both are
// facts, measured in HA: the main zone's current temperature (C), and the
// sustained recovery rate (C/h, the fitted outdoor-temperature model lives in
// the perception package with its live-tunable knob). The target is not
// theirs to choose - it is the matrix's (`decide::returning_heat_setpoint`).
// Optional: either missing leaves the plain comfort pre-start.
pub const ENTITY_MAIN_TEMPERATURE: &str = "sensor.homeostat_main_temperature";
pub const ENTITY_RECOVERY_RATE: &str = "sensor.homeostat_recovery_rate";
// Minutes from now until the recovery horizon (grid peak end + the recovery
// window). Perception does the clock conversion (like return_eta); the daemon
// owns the "is someone back before then" policy - see `back_during_recovery`.
// 0 = no grid event announced.
pub const ENTITY_RECOVERY_HORIZON: &str = "sensor.homeostat_recovery_horizon_minutes";
// Nobody was home overnight and nobody has come home since (perception
// latches "nobody home at deep night" and clears it on arrival). The fact
// that makes a morning grid event different from an evening one without
// giving the daemon a wall clock - see `back_during_recovery`. Optional:
// missing/unknown reads as off (slept home), the comfort-safe default.
pub const ENTITY_SLEPT_AWAY: &str = "binary_sensor.homeostat_slept_away";

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

/// Minutes-until-return below which an away house is always treated as
/// "returning": the minimum pre-start, and the whole of it for cooling and
/// whenever the house's temperature or recovery rate is unknown. A house
/// further from the returning target stretches the lead to the time it
/// needs to get there - see `recovery_minutes` and `derive`.
const RETURNING_LEAD_MIN: f64 = 20.0;

/// Minutes to warm the main zone from `current` to `target` at `rate_c_per_h`,
/// rounded up to 5 min (the same hysteresis step as the return estimates, so
/// a temperature reading moving by a tenth does not flap the buckets).
/// 0 when the house is already there, or when either fact is unknown - the
/// plain comfort pre-start then applies.
///
/// It shrinks as the house warms, by a minute per minute at exactly the
/// assumed rate. The rate is the conservative sustained one, so a warm-up
/// that runs ahead of it can fall back out of `AwayReturning` until the
/// arrival catches up: the model saying, correctly, that it would still make
/// it from here. At worst that is one extra stop/start of the equipment per
/// return.
pub fn recovery_minutes(current: Option<f64>, rate_c_per_h: Option<f64>, target: f64) -> f64 {
    match (current, rate_c_per_h) {
        (Some(current), Some(rate)) if rate > 0.0 && current < target => {
            ((target - current) * 60.0 / rate / 5.0).ceil() * 5.0
        }
        _ => 0.0,
    }
}
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
    /// `recovery_min` is how long the house needs to warm to what a return
    /// commands - full comfort, 22.5C on a normal heating day - from its
    /// current temperature (`recovery_minutes`; 0 = already there or
    /// unknown). It stretches the returning lead so the house is *at* the
    /// target on arrival rather than starting toward it: from the 19C away
    /// baseline that is a couple of hours at winter rates, and from a
    /// deep-cold cycle (12C after a shed peak) many more - so the returning
    /// check runs first and outranks Far. In practice the estimate only
    /// exists once someone is heading home, so a long lead means "start the
    /// moment the return is known".
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

/// Will someone be home during the grid event or before the house recovers
/// on its own afterwards? Gates the away preheat boost (see decide.rs): true
/// = normal preheat, false = bare preheat and let the peak fall deep.
///
/// Perception provides the horizon as minutes-from-now (grid peak end + the
/// recovery window; 0 = no event); this applies the policy against the same
/// return estimates that drive occupancy:
/// - `horizon_min <= 0`: no event -> trivially back (comfort-safe default).
/// - the credible estimate (`eta`, when > 0) landing past the horizon proves
///   an estimated absence.
/// - the physical floor landing past the horizon proves an absence outright.
/// - `slept_away` with no estimate assumes an absence: nobody who slept
///   elsewhere starts driving home at 5AM unannounced, so an overnight
///   absence with zero return evidence means the event window passes empty.
///   This is what makes a morning event behave differently from an evening
///   one - the rule is symmetric, the *fact* is not (mornings follow
///   nights). Any positive eta outranks the assumption: real evidence of
///   return is compared against the horizon like always.
///
/// Back = no absence holds.
pub fn back_during_recovery(
    horizon_min: f64,
    return_eta_min: f64,
    return_floor_min: f64,
    slept_away: bool,
) -> bool {
    if horizon_min <= 0.0 {
        return true;
    }
    let absent_estimated = return_eta_min > 0.0 && return_eta_min >= horizon_min;
    let absent_proven = return_floor_min >= horizon_min;
    let absent_overnight = slept_away && return_eta_min <= 0.0;
    !(absent_estimated || absent_proven || absent_overnight)
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

/// Main-zone conditioning vocabulary, shared by the perception input
/// (what the day demands, derived here from the forecast) and the
/// published decision (`desired_main_mode`: what to command).
///
/// `Circulate` exists because of a wrong assumption this enum used to
/// encode. It said circulation was purely output-side (`desired_fan_mode`)
/// and that "fan" therefore did not belong here - so a mild day commanded
/// `Off` with the fan asked for separately. On the TH6500WF (and, David
/// reports, on the physical unit generally) the fan setting is only
/// independent while the system is *not* off: `off` means the air handler
/// too, and the fan setting sits there being ignored. The house spent mild
/// days with dead air while the daemon believed it had asked for a breeze.
///
/// So circulation is a *mode*, not a fan setting. The device has no
/// fan-only mode either - layer 3 expresses `Circulate` as cooling with a
/// setpoint that cannot be reached (see `CIRCULATE_SETPOINT`), which is
/// the same thing in device terms. `Off` remains, and now means what it
/// says: nobody home, or a grid peak.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HvacMode {
    Heat,
    Cool,
    Circulate,
    Off,
}

impl HvacMode {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "heat" => Some(Self::Heat),
            "cool" => Some(Self::Cool),
            "circulate" => Some(Self::Circulate),
            "off" => Some(Self::Off),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Heat => "heat",
            Self::Cool => "cool",
            Self::Circulate => "circulate",
            Self::Off => "off",
        }
    }
}

/// Complete, validated snapshot of the perception layer.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Inputs {
    pub occupancy: Occupancy,
    pub energy_period: EnergyPeriod,
    /// What the day demands of the main zone. Derived from the forecast by
    /// `decide::demanded_mode`, not read from perception - see
    /// `docs/where-policy-lives.md`.
    pub main_mode: HvacMode,
    pub aux_zone_occupied: bool,
    /// Someone is expected home during the grid event or within the recovery
    /// horizon after it ends (computed by `back_during_recovery`). Gates the
    /// away preheat boost; the no-event / no-estimate default is true (a
    /// normal preheat, the comfort-safe direction).
    pub back_during_recovery: bool,
}

/// Accumulates entity states as they arrive; yields `Inputs` only once every
/// slot holds a valid value. `unknown`/`unavailable` clears the slot.
#[derive(Debug, Default)]
pub struct RawInputs {
    presence: Option<Presence>,
    energy_period: Option<EnergyPeriod>,
    forecast_max: Option<f64>,
    aux_zone_occupied: Option<bool>,
    /// Optional: None = no drill running = follow the forecast. Distinct
    /// from the required slots above, whose None suspends decisions.
    main_mode_override: Option<HvacMode>,
    /// Plain f64, not Option: these inputs are optional by design -
    /// unavailable/unknown means "no estimate" and must not suspend
    /// decisions.
    return_eta_min: f64,
    return_floor_min: f64,
    recovery_horizon_min: f64,
    /// Option, unlike the minutes above: 0C is a real temperature, so
    /// "unknown" cannot be spelled 0. Optional all the same - None only
    /// drops the lead back to the plain pre-start.
    main_temperature: Option<f64>,
    recovery_rate: Option<f64>,
    /// Plain bool for the same reason: optional, and anything but "on"
    /// (including unknown/unavailable) means "slept home" - the
    /// comfort-safe default that keeps the in-doubt preheat.
    slept_away: bool,
}

impl RawInputs {
    /// Ingest a state update. Returns true if the update was for an entity we
    /// track and changed its slot.
    pub fn ingest(&mut self, entity_id: &str, state: &str) -> bool {
        match entity_id {
            ENTITY_OCCUPANCY => Self::set(&mut self.presence, Presence::parse(state)),
            ENTITY_ENERGY_PERIOD => Self::set(&mut self.energy_period, EnergyPeriod::parse(state)),
            ENTITY_FORECAST_MAX => Self::set_req_f64(&mut self.forecast_max, state),
            // HvacMode::parse rejects "auto" (and unknown/unavailable) to
            // None, which is exactly "no override" for this slot.
            ENTITY_MAIN_MODE_OVERRIDE => {
                Self::set(&mut self.main_mode_override, HvacMode::parse(state))
            }
            ENTITY_AUX_ZONE_OCCUPIED => Self::set(
                &mut self.aux_zone_occupied,
                match state {
                    "on" => Some(true),
                    "off" => Some(false),
                    _ => None,
                },
            ),
            ENTITY_RETURN_ETA => Self::set_f64(&mut self.return_eta_min, state),
            ENTITY_RETURN_FLOOR => Self::set_f64(&mut self.return_floor_min, state),
            ENTITY_MAIN_TEMPERATURE => Self::set(&mut self.main_temperature, state.parse().ok()),
            ENTITY_RECOVERY_RATE => Self::set(&mut self.recovery_rate, state.parse().ok()),
            ENTITY_RECOVERY_HORIZON => Self::set_f64(&mut self.recovery_horizon_min, state),
            ENTITY_SLEPT_AWAY => Self::set_flag(&mut self.slept_away, state),
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

    /// Required numeric input: unparseable clears the slot and suspends
    /// decisions. The opposite of `set_f64`, whose inputs are optional.
    fn set_req_f64(slot: &mut Option<f64>, state: &str) -> bool {
        Self::set(slot, state.parse::<f64>().ok())
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

    /// Optional binary input: only "on" is on; unknown/unavailable fall to
    /// off (the comfort-safe default) instead of suspending.
    fn set_flag(slot: &mut bool, state: &str) -> bool {
        let value = state == "on";
        if *slot == value {
            false
        } else {
            *slot = value;
            true
        }
    }

    pub fn complete(&self) -> Option<Inputs> {
        let presence = self.presence?;
        let energy_period = self.energy_period?;
        let main_mode = crate::decide::demanded_mode(self.forecast_max?, self.main_mode_override);
        let aux_zone_occupied = self.aux_zone_occupied?;
        let back_during_recovery = back_during_recovery(
            self.recovery_horizon_min,
            self.return_eta_min,
            self.return_floor_min,
            self.slept_away,
        );
        let target = crate::decide::returning_heat_setpoint(
            main_mode,
            energy_period,
            aux_zone_occupied,
            back_during_recovery,
        );
        Some(Inputs {
            occupancy: Occupancy::derive(
                presence,
                self.return_eta_min,
                self.return_floor_min,
                recovery_minutes(self.main_temperature, self.recovery_rate, target),
            ),
            energy_period,
            main_mode,
            aux_zone_occupied,
            back_during_recovery,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unavailable_optional_input_does_not_suspend() {
        let mut raw = RawInputs::default();
        raw.ingest(ENTITY_OCCUPANCY, "home");
        raw.ingest(ENTITY_ENERGY_PERIOD, "normal");
        raw.ingest(ENTITY_FORECAST_MAX, "-5");
        raw.ingest(ENTITY_AUX_ZONE_OCCUPIED, "off");
        raw.ingest(ENTITY_RETURN_ETA, "unavailable");
        raw.complete()
            .expect("an optional input must not suspend decisions");
    }

    /// The forecast is a required input, and its absence must suspend rather
    /// than fall back to a mild day. This is the regression test for the bug
    /// that motivated moving the policy here: perception used to read a
    /// forecast entity that had silently ceased to exist and apply
    /// `| float(20)` to the result, so "no idea what the weather is" became
    /// "a pleasant 20C day" and the house was commanded off for a whole
    /// summer. Holding the last decision is loud; inventing weather is not.
    #[test]
    fn a_missing_forecast_suspends_instead_of_inventing_a_mild_day() {
        let mut raw = RawInputs::default();
        raw.ingest(ENTITY_OCCUPANCY, "home");
        raw.ingest(ENTITY_ENERGY_PERIOD, "normal");
        raw.ingest(ENTITY_AUX_ZONE_OCCUPIED, "off");
        assert!(raw.complete().is_none(), "no forecast at all must suspend");

        raw.ingest(ENTITY_FORECAST_MAX, "unavailable");
        assert!(raw.complete().is_none(), "unavailable must suspend");

        raw.ingest(ENTITY_FORECAST_MAX, "30");
        assert_eq!(raw.complete().unwrap().main_mode, HvacMode::Cool);

        // and it suspends again if the forecast later goes away
        raw.ingest(ENTITY_FORECAST_MAX, "unknown");
        assert!(raw.complete().is_none(), "losing the forecast re-suspends");
    }

    /// The drill knob outranks the forecast all the way through the input
    /// boundary - "simulate winter in July" has to reach the matrix.
    #[test]
    fn the_drill_knob_overrides_the_forecast_end_to_end() {
        let mut raw = RawInputs::default();
        raw.ingest(ENTITY_OCCUPANCY, "home");
        raw.ingest(ENTITY_ENERGY_PERIOD, "normal");
        raw.ingest(ENTITY_AUX_ZONE_OCCUPIED, "off");
        raw.ingest(ENTITY_FORECAST_MAX, "30");
        assert_eq!(raw.complete().unwrap().main_mode, HvacMode::Cool);

        raw.ingest(ENTITY_MAIN_MODE_OVERRIDE, "heat");
        assert_eq!(raw.complete().unwrap().main_mode, HvacMode::Heat);

        // 'auto' is the knob's rest position, not a mode: back to the forecast
        raw.ingest(ENTITY_MAIN_MODE_OVERRIDE, "auto");
        assert_eq!(raw.complete().unwrap().main_mode, HvacMode::Cool);
    }

    #[test]
    fn unknown_occupancy_suspends_decisions() {
        let mut raw = RawInputs::default();
        raw.ingest(ENTITY_OCCUPANCY, "unknown");
        raw.ingest(ENTITY_ENERGY_PERIOD, "normal");
        raw.ingest(ENTITY_FORECAST_MAX, "-5");
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
        raw.ingest(ENTITY_FORECAST_MAX, "-5");
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
        raw.ingest(ENTITY_FORECAST_MAX, "-5");
        raw.ingest(ENTITY_AUX_ZONE_OCCUPIED, "off");
        raw.ingest(ENTITY_RETURN_ETA, "unavailable");
        raw.ingest(ENTITY_RETURN_FLOOR, "unknown");
        raw.ingest(ENTITY_MAIN_TEMPERATURE, "unavailable");
        raw.ingest(ENTITY_RECOVERY_RATE, "unknown");
        let inputs = raw
            .complete()
            .expect("optional inputs must not suspend decisions");
        assert_eq!(inputs.occupancy, Occupancy::Away, "no data = conservative");
    }

    #[test]
    fn recovery_minutes_is_the_time_to_the_target_at_the_measured_rate() {
        // 19C away baseline -> 22.5C at 1.4C/h: 150 min exactly
        assert_eq!(recovery_minutes(Some(19.0), Some(1.4), 22.5), 150.0);
        // rounded up to the 5-min step: 2026-11-03, 20.3C -> 94.3 -> 95
        assert_eq!(recovery_minutes(Some(20.3), Some(1.4), 22.5), 95.0);
        // at or above the target: nothing to recover
        assert_eq!(recovery_minutes(Some(22.5), Some(1.4), 22.5), 0.0);
        assert_eq!(recovery_minutes(Some(27.0), Some(1.4), 20.0), 0.0);
        // either fact unknown, or a nonsensical rate: no stretch
        assert_eq!(recovery_minutes(None, Some(1.4), 22.5), 0.0);
        assert_eq!(recovery_minutes(Some(19.0), None, 22.5), 0.0);
        assert_eq!(recovery_minutes(Some(19.0), Some(0.0), 22.5), 0.0);
        assert_eq!(recovery_minutes(Some(19.0), Some(-1.0), 22.5), 0.0);
    }

    /// 0C is a temperature, not "unknown": the optional-minutes convention
    /// (unparseable = 0) must not leak into the temperature slot.
    #[test]
    fn a_freezing_house_is_a_reading_not_a_missing_one() {
        let mut raw = RawInputs::default();
        raw.ingest(ENTITY_MAIN_TEMPERATURE, "0");
        raw.ingest(ENTITY_RECOVERY_RATE, "1.0");
        assert_eq!(raw.main_temperature, Some(0.0));
        assert_eq!(
            recovery_minutes(raw.main_temperature, raw.recovery_rate, 22.5),
            1350.0
        );
    }

    #[test]
    fn back_during_recovery_gates_on_the_horizon() {
        // no grid event (or the horizon input missing -> 0) is trivially
        // back: the comfort-safe normal preheat
        assert!(back_during_recovery(0.0, 0.0, 0.0, false));
        assert!(back_during_recovery(0.0, 300.0, 300.0, false));

        // horizon 180 min out. An estimate landing before it = back;
        // a credible estimate past it = absent (bare preheat).
        assert!(back_during_recovery(180.0, 60.0, 0.0, false));
        assert!(!back_during_recovery(180.0, 240.0, 0.0, false));

        // no estimate (eta 0) never proves an estimated absence, but the
        // physical floor past the horizon proves it outright
        assert!(back_during_recovery(180.0, 0.0, 60.0, false));
        assert!(!back_during_recovery(180.0, 0.0, 200.0, false));

        // boundary: arrival exactly at the horizon counts as absent
        assert!(!back_during_recovery(180.0, 180.0, 0.0, false));
    }

    #[test]
    fn overnight_absence_without_return_evidence_is_assumed_absent() {
        // slept away, no estimate: nobody starts driving home at 5AM
        // unannounced - assume the event window passes empty, even though
        // the physical floor alone (60 < 180) proves nothing
        assert!(!back_during_recovery(180.0, 0.0, 60.0, true));
        assert!(!back_during_recovery(180.0, 0.0, 0.0, true));

        // any positive eta outranks the assumption: real return evidence
        // is compared against the horizon like always
        assert!(back_during_recovery(180.0, 60.0, 60.0, true));
        assert!(!back_during_recovery(180.0, 240.0, 240.0, true));

        // no grid event: the flag is irrelevant
        assert!(back_during_recovery(0.0, 0.0, 0.0, true));

        // slept home (or the input missing): the in-doubt preheat stands
        assert!(back_during_recovery(180.0, 0.0, 60.0, false));
    }

    #[test]
    fn missing_horizon_input_defaults_to_normal_preheat() {
        let mut raw = RawInputs::default();
        raw.ingest(ENTITY_OCCUPANCY, "away");
        raw.ingest(ENTITY_ENERGY_PERIOD, "preheat");
        raw.ingest(ENTITY_FORECAST_MAX, "-5");
        raw.ingest(ENTITY_AUX_ZONE_OCCUPIED, "off");
        // the horizon sensor never arrived (0) -> comfort-safe normal preheat
        assert!(raw.complete().unwrap().back_during_recovery);

        // a real horizon that the floor overshoots -> provably absent
        raw.ingest(ENTITY_RETURN_FLOOR, "240");
        raw.ingest(ENTITY_RECOVERY_HORIZON, "180");
        assert!(!raw.complete().unwrap().back_during_recovery);
    }
}
