//! Layer 2: the decision matrix as a pure function, and layer 3's write
//! planner. Every temperature in the system lives in `decide()`. The
//! compiler enforces that every (season, energy_period, occupancy)
//! combination is handled — the class of gap that caused the 2026-07-07
//! incident cannot compile.

use crate::state::{
    EnergyPeriod, Inputs, Occupancy, Season, BASEMENT_THERMOSTATS, ENTITY_MAIN_HVAC,
    ENTITY_WATER_HEATER,
};
use serde_json::json;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HvacMode {
    Heat,
    Cool,
    Off,
}

impl HvacMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Heat => "heat",
            Self::Cool => "cool",
            Self::Off => "off",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FanMode {
    On,
    Auto,
}

impl FanMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::On => "on",
            Self::Auto => "auto",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Desired {
    pub main_mode: HvacMode,
    pub main_setpoint: f64,
    pub fan_mode: FanMode,
    /// None = basement thermostats off (cooling season).
    pub basement_setpoint: Option<f64>,
    pub water_heater_on: bool,
}

pub fn decide(i: &Inputs) -> Desired {
    use EnergyPeriod::*;
    use Occupancy::*;
    use Season::*;

    let main_mode = match i.season {
        Heat => HvacMode::Heat,
        Cool => HvacMode::Cool,
        Fan => HvacMode::Off, // mode off + fan on = circulation only
    };

    let main_setpoint = match i.season {
        Cool => match i.occupancy {
            Home => 25.0,
            HomeAsleep => 24.0,
            Returning => 26.0,
            Away | AwayFar => 28.0,
        },
        // In fan season the setpoint is not applied (mode off) but is still
        // computed so the published decision stays meaningful in history.
        Heat | Fan => match (i.energy_period, i.occupancy) {
            (Normal, Home) => 22.5,
            (Normal, HomeAsleep) => 22.5,
            (Normal, Returning) => 21.0,
            (Normal, Away) => 19.0,
            (Normal, AwayFar) => 17.0,
            (Preheat, Home) => 25.0,
            (Preheat, HomeAsleep) => 24.0,
            (Preheat, Returning) => 25.0,
            (Preheat, Away) => 23.0,
            (Preheat, AwayFar) => 21.0,
            (Peak, Home) => 16.0,
            (Peak, HomeAsleep) => 16.0,
            (Peak, Returning) => 13.0,
            (Peak, Away) => 12.0,
            (Peak, AwayFar) => 10.0,
        },
    };

    let fan_mode = if i.season == Fan || i.occupancy.is_home() {
        FanMode::On
    } else {
        FanMode::Auto
    };

    let basement_setpoint = match (i.season, i.energy_period) {
        (Cool, _) => None,
        (_, Peak) => Some(5.0),
        (_, Preheat) => Some(26.0),
        (_, Normal) => Some(if i.basement_occupied { 19.0 } else { 16.0 }),
    };

    Desired {
        main_mode,
        main_setpoint,
        fan_mode,
        basement_setpoint,
        water_heater_on: i.energy_period != Peak,
    }
}

/// One HA service call. Calls are executed strictly in `Vec` order — the
/// planner encodes the mode-before-setpoint contract in the plan itself.
#[derive(Debug, Clone, PartialEq)]
pub struct ServiceCall {
    pub domain: &'static str,
    pub service: &'static str,
    pub entity_ids: Vec<&'static str>,
    pub data: serde_json::Value,
}

impl ServiceCall {
    fn climate(service: &'static str, entities: Vec<&'static str>, data: serde_json::Value) -> Self {
        Self { domain: "climate", service, entity_ids: entities, data }
    }
}

/// Plan the writes for the main HVAC. Mode, setpoint and fan always travel
/// together; a setpoint can never land on a stale mode (the 2026-07-07 bug).
/// Returns an empty plan when a human comfort override is being respected.
pub fn plan_main_hvac(d: &Desired, i: &Inputs) -> Vec<ServiceCall> {
    let respect_override = i.comfort_override.is_active()
        && i.energy_period == EnergyPeriod::Normal
        && i.occupancy.is_home();
    if respect_override {
        return vec![];
    }

    let mut plan = vec![ServiceCall::climate(
        "set_hvac_mode",
        vec![ENTITY_MAIN_HVAC],
        json!({ "hvac_mode": d.main_mode.as_str() }),
    )];
    if d.main_mode != HvacMode::Off {
        plan.push(ServiceCall::climate(
            "set_temperature",
            vec![ENTITY_MAIN_HVAC],
            json!({ "temperature": d.main_setpoint }),
        ));
    }
    plan.push(ServiceCall::climate(
        "set_fan_mode",
        vec![ENTITY_MAIN_HVAC],
        json!({ "fan_mode": d.fan_mode.as_str() }),
    ));
    plan
}

pub fn plan_basement(d: &Desired) -> Vec<ServiceCall> {
    let thermostats = BASEMENT_THERMOSTATS.to_vec();
    match d.basement_setpoint {
        None => vec![ServiceCall::climate("turn_off", thermostats, json!({}))],
        Some(t) => vec![
            ServiceCall::climate("turn_on", thermostats.clone(), json!({})),
            ServiceCall::climate("set_temperature", thermostats, json!({ "temperature": t })),
        ],
    }
}

pub fn plan_water_heater(d: &Desired) -> Vec<ServiceCall> {
    vec![ServiceCall {
        domain: "switch",
        service: if d.water_heater_on { "turn_on" } else { "turn_off" },
        entity_ids: vec![ENTITY_WATER_HEATER],
        data: json!({}),
    }]
}

/// Full actuation plan for a decision.
pub fn plan_all(d: &Desired, i: &Inputs) -> Vec<ServiceCall> {
    let mut plan = plan_main_hvac(d, i);
    plan.extend(plan_basement(d));
    plan.extend(plan_water_heater(d));
    plan
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{ComfortOverride, EnergyPeriod, Occupancy, Season};

    fn inputs(occupancy: Occupancy, energy_period: EnergyPeriod, season: Season) -> Inputs {
        Inputs {
            occupancy,
            energy_period,
            season,
            basement_occupied: false,
            comfort_override: ComfortOverride::None,
        }
    }

    /// Regression test for the 2026-07-07 incident: leaving home on a hot day
    /// with the physical HVAC manually set to cool@26. The old system pushed
    /// a 19C *heat* setpoint onto the device without changing its mode,
    /// cooling the house to 19C all afternoon. The correct decision is cool
    /// mode with a conservative high setpoint, and any plan must write the
    /// mode before the setpoint so a manual mode can never go stale.
    #[test]
    fn july_7_away_in_summer_never_cools_below_conservative() {
        let i = inputs(Occupancy::Away, EnergyPeriod::Normal, Season::Cool);
        let d = decide(&i);

        assert_eq!(d.main_mode, HvacMode::Cool);
        assert_eq!(d.main_setpoint, 28.0);
        assert!(d.main_setpoint >= 26.0, "away cool setpoint must be conservative");

        let plan = plan_main_hvac(&d, &i);
        let mode_pos = plan.iter().position(|c| c.service == "set_hvac_mode").unwrap();
        let temp_pos = plan.iter().position(|c| c.service == "set_temperature").unwrap();
        assert!(mode_pos < temp_pos, "mode must be written before setpoint");
    }

    #[test]
    fn peak_sheds_all_loads() {
        let i = inputs(Occupancy::Home, EnergyPeriod::Peak, Season::Heat);
        let d = decide(&i);
        assert!(!d.water_heater_on);
        assert_eq!(d.main_setpoint, 16.0);
        assert_eq!(d.basement_setpoint, Some(5.0));
    }

    #[test]
    fn returning_gets_milder_peak_and_richer_preheat_than_away() {
        let away_peak = decide(&inputs(Occupancy::Away, EnergyPeriod::Peak, Season::Heat));
        let ret_peak = decide(&inputs(Occupancy::Returning, EnergyPeriod::Peak, Season::Heat));
        assert!(ret_peak.main_setpoint > away_peak.main_setpoint);

        let away_pre = decide(&inputs(Occupancy::Away, EnergyPeriod::Preheat, Season::Heat));
        let ret_pre = decide(&inputs(Occupancy::Returning, EnergyPeriod::Preheat, Season::Heat));
        assert!(ret_pre.main_setpoint > away_pre.main_setpoint);
    }

    #[test]
    fn comfort_override_pauses_main_hvac_but_not_during_peak() {
        let mut i = inputs(Occupancy::Home, EnergyPeriod::Normal, Season::Heat);
        i.comfort_override = ComfortOverride::TooCold;
        let d = decide(&i);
        assert!(plan_main_hvac(&d, &i).is_empty(), "override respected at home, normal period");

        i.energy_period = EnergyPeriod::Peak;
        let d = decide(&i);
        assert!(!plan_main_hvac(&d, &i).is_empty(), "peak always wins over comfort");
    }

    #[test]
    fn fan_season_turns_mode_off_but_keeps_fan_on() {
        let i = inputs(Occupancy::Home, EnergyPeriod::Normal, Season::Fan);
        let d = decide(&i);
        assert_eq!(d.main_mode, HvacMode::Off);
        assert_eq!(d.fan_mode, FanMode::On);
        let plan = plan_main_hvac(&d, &i);
        assert!(
            !plan.iter().any(|c| c.service == "set_temperature"),
            "no setpoint write when mode is off"
        );
    }

    #[test]
    fn basement_follows_occupancy_and_cool_season_turns_it_off() {
        let mut i = inputs(Occupancy::Home, EnergyPeriod::Normal, Season::Heat);
        assert_eq!(decide(&i).basement_setpoint, Some(16.0));
        i.basement_occupied = true;
        assert_eq!(decide(&i).basement_setpoint, Some(19.0));
        i.season = Season::Cool;
        assert_eq!(decide(&i).basement_setpoint, None);
    }
}
