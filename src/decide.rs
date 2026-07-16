//! Layer 2: the decision matrix as a pure function, and layer 3's write
//! planner. Every temperature in the system lives in `decide()`. The
//! compiler enforces that every (season, energy_period, occupancy)
//! combination is handled — the class of gap that caused the 2026-07-07
//! incident cannot compile.

use crate::state::{EnergyPeriod, Inputs, Occupancy, Season};

/// Bounds for the final main-zone setpoint after the comfort offset.
const SETPOINT_MIN: f64 = 15.0;
const SETPOINT_MAX: f64 = 29.0;

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
    pub aux_zone_setpoint: Option<f64>,
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
            AwayReturning => 26.0,
            Away | AwayFar => 28.0,
        },
        // In fan season the setpoint is not applied (mode off) but is still
        // computed so the published decision stays meaningful in history.
        Heat | Fan => match (i.energy_period, i.occupancy) {
            (Normal, Home) => 22.5,
            (Normal, HomeAsleep) => 22.5,
            (Normal, AwayReturning) => 21.0,
            (Normal, Away) => 19.0,
            (Normal, AwayFar) => 17.0,
            (Preheat, Home) => 25.0,
            (Preheat, HomeAsleep) => 24.0,
            (Preheat, AwayReturning) => 25.0,
            // The preheat boost only pays if someone is home during the
            // peak or before the house recovers on its own afterwards.
            // Provably absent past that horizon = bare preheat: hold the
            // normal away baseline and let the peak fall deep (~10C,
            // verified livable-when-empty). Whether an absent house should
            // instead keep a small boost (19->24->12 vs 19->19->10 cycles)
            // is empirically unresolved - compare peak-day kWh once this
            // runs and tune these two cells.
            (Preheat, Away) => {
                if i.back_during_recovery {
                    23.0
                } else {
                    19.0
                }
            }
            (Preheat, AwayFar) => {
                if i.back_during_recovery {
                    21.0
                } else {
                    17.0
                }
            }
            (Peak, Home) => 16.0,
            (Peak, HomeAsleep) => 16.0,
            (Peak, AwayReturning) => 13.0,
            (Peak, Away) => 12.0,
            (Peak, AwayFar) => 10.0,
        },
    };

    // Honor a manual hold (absolute setpoint, standard thermostat
    // semantics) on the main zone when someone is home and no grid event is
    // running (peak/preheat always win). The reset-on-away automation in HA
    // is belt and suspenders on top of this.
    let main_setpoint =
        if i.occupancy.is_home() && i.energy_period == Normal && i.comfort_setpoint > 0.0 {
            i.comfort_setpoint.clamp(SETPOINT_MIN, SETPOINT_MAX)
        } else {
            main_setpoint
        };

    let fan_mode = if i.season == Fan || i.occupancy.is_home() {
        FanMode::On
    } else {
        FanMode::Auto
    };

    let aux_zone_setpoint = match (i.season, i.energy_period) {
        (Cool, _) => None,
        (_, Peak) => Some(5.0),
        (_, Preheat) => Some(26.0),
        (_, Normal) => Some(if i.aux_zone_occupied { 19.0 } else { 16.0 }),
    };

    Desired {
        main_mode,
        main_setpoint,
        fan_mode,
        aux_zone_setpoint,
        water_heater_on: i.energy_period != Peak,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{EnergyPeriod, Occupancy, Season};

    fn inputs(occupancy: Occupancy, energy_period: EnergyPeriod, season: Season) -> Inputs {
        Inputs {
            occupancy,
            energy_period,
            season,
            aux_zone_occupied: false,
            comfort_setpoint: 0.0,
            back_during_recovery: true,
        }
    }

    /// Hard safety invariant: indoor anywhere near 0C risks breaking the
    /// house (pipes - at -20 outside a drop can run -5C/h once shed). The
    /// protection is that every reachable decision, however aggressive the
    /// shed, commands a setpoint the thermostats will defend: >= 10C on the
    /// main zone in any heating-capable season, >= 5C (the Sinope frost
    /// floor) on the aux zone. Burning peak kWh to hold that line is
    /// always the right trade - breakage costs more than credit.
    #[test]
    fn no_decision_ever_commands_anywhere_near_freezing() {
        use EnergyPeriod::*;
        use Occupancy::*;
        use Season::*;

        for season in [Heat, Fan, Cool] {
            for energy_period in [Normal, Preheat, Peak] {
                for occupancy in [Home, HomeAsleep, AwayReturning, Away, AwayFar] {
                    for back_during_recovery in [true, false] {
                        for aux_zone_occupied in [true, false] {
                            for comfort_setpoint in [0.0, 5.0, 29.0] {
                                let i = Inputs {
                                    occupancy,
                                    energy_period,
                                    season,
                                    aux_zone_occupied,
                                    comfort_setpoint,
                                    back_during_recovery,
                                };
                                let d = decide(&i);
                                if season != Cool {
                                    assert!(
                                        d.main_setpoint >= 10.0,
                                        "main-zone freeze floor violated: {i:?} -> {d:?}"
                                    );
                                }
                                if let Some(aux) = d.aux_zone_setpoint {
                                    assert!(
                                        aux >= 5.0,
                                        "aux-zone freeze floor violated: {i:?} -> {d:?}"
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    /// Regression test for the 2026-07-07 incident: leaving home on a hot day
    /// with the physical HVAC manually set to cool@26. The old system pushed
    /// a 19C *heat* setpoint onto the device without changing its mode,
    /// cooling the house to 19C all afternoon. The correct decision is cool
    /// mode with a conservative high setpoint. The other half of the fix -
    /// mode written before setpoint, always together - lives in the single
    /// main-zone wire automation in HA (see the perception package).
    #[test]
    fn july_7_away_in_summer_never_cools_below_conservative() {
        let i = inputs(Occupancy::Away, EnergyPeriod::Normal, Season::Cool);
        let d = decide(&i);

        assert_eq!(d.main_mode, HvacMode::Cool);
        assert_eq!(d.main_setpoint, 28.0);
        assert!(
            d.main_setpoint >= 26.0,
            "away cool setpoint must be conservative"
        );
    }

    #[test]
    fn peak_sheds_all_loads() {
        let i = inputs(Occupancy::Home, EnergyPeriod::Peak, Season::Heat);
        let d = decide(&i);
        assert!(!d.water_heater_on);
        assert_eq!(d.main_setpoint, 16.0);
        assert_eq!(d.aux_zone_setpoint, Some(5.0));
    }

    #[test]
    fn returning_gets_milder_peak_and_richer_preheat_than_away() {
        let away_peak = decide(&inputs(Occupancy::Away, EnergyPeriod::Peak, Season::Heat));
        let ret_peak = decide(&inputs(
            Occupancy::AwayReturning,
            EnergyPeriod::Peak,
            Season::Heat,
        ));
        assert!(ret_peak.main_setpoint > away_peak.main_setpoint);

        let away_pre = decide(&inputs(
            Occupancy::Away,
            EnergyPeriod::Preheat,
            Season::Heat,
        ));
        let ret_pre = decide(&inputs(
            Occupancy::AwayReturning,
            EnergyPeriod::Preheat,
            Season::Heat,
        ));
        assert!(ret_pre.main_setpoint > away_pre.main_setpoint);
    }

    #[test]
    fn comfort_hold_honored_at_home_but_never_during_grid_events_or_away() {
        // hold value chosen to differ from every matrix cell it meets, so
        // each assertion can only pass for the right reason
        let mut i = inputs(Occupancy::Home, EnergyPeriod::Normal, Season::Heat);
        i.comfort_setpoint = 23.5;
        assert_eq!(
            decide(&i).main_setpoint,
            23.5,
            "manual hold applied at home"
        );

        i.occupancy = Occupancy::HomeAsleep;
        assert_eq!(
            decide(&i).main_setpoint,
            23.5,
            "hold survives schedule transitions"
        );

        i.energy_period = EnergyPeriod::Peak;
        assert_eq!(
            decide(&i).main_setpoint,
            16.0,
            "peak overrides the hold's effect"
        );

        i.energy_period = EnergyPeriod::Preheat;
        assert_eq!(
            decide(&i).main_setpoint,
            24.0,
            "preheat overrides the hold's effect"
        );

        i.energy_period = EnergyPeriod::Normal;
        assert_eq!(
            decide(&i).main_setpoint,
            23.5,
            "the hold's value survives grid events and resumes after them"
        );

        i.occupancy = Occupancy::Away;
        assert_eq!(
            decide(&i).main_setpoint,
            19.0,
            "a stale hold is ignored when away"
        );
    }

    #[test]
    fn comfort_hold_zero_means_automatic_and_values_are_clamped() {
        let mut i = inputs(Occupancy::Home, EnergyPeriod::Normal, Season::Heat);
        assert_eq!(
            decide(&i).main_setpoint,
            22.5,
            "0 = no hold, matrix applies"
        );
        i.comfort_setpoint = 5.0;
        assert_eq!(
            decide(&i).main_setpoint,
            15.0,
            "a lowball hold clamps to SETPOINT_MIN"
        );
    }

    #[test]
    fn bare_preheat_when_provably_absent_past_the_recovery_horizon() {
        let mut i = inputs(Occupancy::Away, EnergyPeriod::Preheat, Season::Heat);
        assert_eq!(decide(&i).main_setpoint, 23.0, "assume back = boost");
        i.back_during_recovery = false;
        assert_eq!(
            decide(&i).main_setpoint,
            19.0,
            "absent = hold the normal away baseline, no boost"
        );

        i.occupancy = Occupancy::AwayFar;
        assert_eq!(decide(&i).main_setpoint, 17.0);

        // only the away preheat cells listen to it
        i.occupancy = Occupancy::Home;
        assert_eq!(decide(&i).main_setpoint, 25.0);
        i.occupancy = Occupancy::AwayReturning;
        assert_eq!(decide(&i).main_setpoint, 25.0);
        i.occupancy = Occupancy::Away;
        i.energy_period = EnergyPeriod::Peak;
        assert_eq!(decide(&i).main_setpoint, 12.0, "peak cells unchanged");
    }

    #[test]
    fn fan_season_turns_mode_off_but_keeps_fan_on() {
        let i = inputs(Occupancy::Home, EnergyPeriod::Normal, Season::Fan);
        let d = decide(&i);
        assert_eq!(d.main_mode, HvacMode::Off);
        assert_eq!(d.fan_mode, FanMode::On);
    }

    #[test]
    fn aux_zone_follows_occupancy_and_cool_season_turns_it_off() {
        let mut i = inputs(Occupancy::Home, EnergyPeriod::Normal, Season::Heat);
        assert_eq!(decide(&i).aux_zone_setpoint, Some(16.0));
        i.aux_zone_occupied = true;
        assert_eq!(decide(&i).aux_zone_setpoint, Some(19.0));
        i.season = Season::Cool;
        assert_eq!(decide(&i).aux_zone_setpoint, None);
    }
}
