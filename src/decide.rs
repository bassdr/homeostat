//! Layer 2: the decision matrix as a pure function, and layer 3's write
//! planner. Every temperature in the system lives in `decide()`. The
//! compiler enforces that every (main_mode, energy_period, occupancy)
//! combination is handled — the class of gap that caused the 2026-07-07
//! incident cannot compile.

use crate::state::{EnergyPeriod, HvacMode, Inputs, Occupancy};

/// Bounds for the final main-zone setpoint after the comfort offset.
const SETPOINT_MIN: f64 = 15.0;
const SETPOINT_MAX: f64 = 29.0;
/// Floor for the comfort hold in cool mode: a heating-grade hold reaching
/// a cooling device is the July-7 output shape, whatever triggered it.
const SETPOINT_MIN_COOL: f64 = 20.0;

/// The aux zone is never commanded off, only down to this frost floor
/// (the Sinope minimum): a setpoint persisted in the device keeps
/// defending the house even if daemon, HA and network all die. Only
/// deferrable loads (load_shed) are ever fully off.
const AUX_FROST_FLOOR: f64 = 5.0;
/// Ceiling for the aux comfort hold (matches the preheat boost).
const AUX_SETPOINT_MAX: f64 = 26.0;

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
    /// Always a real setpoint - AUX_FROST_FLOOR when the zone has no
    /// comfort duty (never off; see the constant).
    pub aux_zone_setpoint: f64,
    /// Shed deferrable loads now. Policy, not a device: the wires decide
    /// what hangs off it (water heater off, EV-charging warning, anything
    /// you want forced off during a grid event).
    pub shed_loads: bool,
}

pub fn decide(i: &Inputs) -> Desired {
    use EnergyPeriod::*;
    use HvacMode::*;
    use Occupancy::*;

    // The demanded mode passes straight through today; policies that
    // override it (e.g. forcing off in a mild away week) would live here.
    let main_mode = i.main_mode;

    let main_setpoint = match i.main_mode {
        Cool => match i.occupancy {
            Home => 25.0,
            HomeAsleep => 24.0,
            // returning = the home target, early: the lead time exists so
            // the house is *at* comfort on arrival, not approaching it
            AwayReturning => 25.0,
            Away | AwayFar => 28.0,
        },
        // On an off day (no conditioning demanded) the setpoint is not
        // applied but is still computed so the published decision stays
        // meaningful in history.
        Heat | Off => match (i.energy_period, i.occupancy) {
            (Normal, Home) => 22.5,
            (Normal, HomeAsleep) => 22.5,
            // returning = the home target, early (see the Cool arm)
            (Normal, AwayReturning) => 22.5,
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
    // is belt and suspenders on top of this. The clamp is mode-aware: a
    // stale heat-season hold (16) landing in cool mode must not command
    // the AC to 16C - that is the July-7 shape, user-triggered.
    let main_setpoint =
        if i.occupancy.is_home() && i.energy_period == Normal && i.comfort_setpoint > 0.0 {
            let min = if main_mode == Cool {
                SETPOINT_MIN_COOL
            } else {
                SETPOINT_MIN
            };
            i.comfort_setpoint.clamp(min, SETPOINT_MAX)
        } else {
            main_setpoint
        };

    // circulation on off days (mode off + fan on) and whenever someone is
    // home; fan is an output-side concept only - see state.rs::HvacMode
    let fan_mode = if i.main_mode == Off || i.occupancy.is_home() {
        FanMode::On
    } else {
        FanMode::Auto
    };

    // The aux zone is heat-only equipment: it heats for comfort only when
    // the day demands heating. Everything else - off days, cool days, deep
    // peaks - gets the frost floor rather than a turn-off: a setpoint
    // persisted in the device defends the house even when daemon, HA and
    // network are all dead, which is exactly when it matters. A shoulder-
    // season basement at 14C stays unheated (the floor never engages above
    // 5C), and a grid preheat on an off day boosts nothing.
    let aux_zone_setpoint = match (i.main_mode, i.energy_period) {
        (Cool | Off, _) => AUX_FROST_FLOOR,
        (Heat, Peak) => AUX_FROST_FLOOR,
        (Heat, Preheat) => 26.0,
        // same hold semantics as the main zone: someone home adjusted a
        // basement thermostat, honor it outside grid events, ignore stale
        // holds when away (the capture automation only records while home
        // and live, the reset-on-away clears both holds)
        (Heat, Normal) => {
            if i.occupancy.is_home() && i.aux_comfort_setpoint > 0.0 {
                i.aux_comfort_setpoint
                    .clamp(AUX_FROST_FLOOR, AUX_SETPOINT_MAX)
            } else if i.aux_zone_occupied {
                19.0
            } else {
                16.0
            }
        }
    };

    Desired {
        main_mode,
        main_setpoint,
        fan_mode,
        aux_zone_setpoint,
        shed_loads: i.energy_period == Peak,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{EnergyPeriod, HvacMode, Occupancy};

    fn inputs(occupancy: Occupancy, energy_period: EnergyPeriod, main_mode: HvacMode) -> Inputs {
        Inputs {
            occupancy,
            energy_period,
            main_mode,
            aux_zone_occupied: false,
            comfort_setpoint: 0.0,
            aux_comfort_setpoint: 0.0,
            back_during_recovery: true,
        }
    }

    /// Hard safety invariant: indoor anywhere near 0C risks breaking the
    /// house (pipes - at -20 outside a drop can run -5C/h once shed). The
    /// protection is that every reachable decision, however aggressive the
    /// shed, commands a setpoint the thermostats will defend: >= 10C on the
    /// main zone whenever heating is possible, >= 5C (the Sinope frost
    /// floor) on the aux zone. Burning peak kWh to hold that line is
    /// always the right trade - breakage costs more than credit.
    #[test]
    fn no_decision_ever_commands_anywhere_near_freezing() {
        use EnergyPeriod::*;
        use HvacMode::*;
        use Occupancy::*;

        for main_mode in [Heat, Off, Cool] {
            for energy_period in [Normal, Preheat, Peak] {
                for occupancy in [Home, HomeAsleep, AwayReturning, Away, AwayFar] {
                    for back_during_recovery in [true, false] {
                        for aux_zone_occupied in [true, false] {
                            for comfort_setpoint in [0.0, 5.0, 29.0] {
                                // hostile aux holds: the clamp must hold the floor
                                for aux_comfort_setpoint in [0.0, 2.0, 30.0] {
                                    let i = Inputs {
                                        occupancy,
                                        energy_period,
                                        main_mode,
                                        aux_zone_occupied,
                                        comfort_setpoint,
                                        aux_comfort_setpoint,
                                        back_during_recovery,
                                    };
                                    let d = decide(&i);
                                    if main_mode != Cool {
                                        assert!(
                                            d.main_setpoint >= 10.0,
                                            "main-zone freeze floor violated: {i:?} -> {d:?}"
                                        );
                                    }
                                    assert!(
                                        d.aux_zone_setpoint >= AUX_FROST_FLOOR,
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

    /// The generalized July-7 invariant: no reachable decision may pair
    /// cool mode with a heating-grade setpoint, whatever produced it -
    /// matrix cell, stale state, or a leftover heat-season comfort hold
    /// punched in from the dashboard. Sweeps every input combination
    /// including hostile hold values.
    #[test]
    fn cool_mode_never_carries_a_heating_grade_setpoint() {
        use EnergyPeriod::*;
        use HvacMode::*;
        use Occupancy::*;

        for main_mode in [Heat, Off, Cool] {
            for energy_period in [Normal, Preheat, Peak] {
                for occupancy in [Home, HomeAsleep, AwayReturning, Away, AwayFar] {
                    for back_during_recovery in [true, false] {
                        for comfort_setpoint in [0.0, 5.0, 16.0, 22.0, 29.0] {
                            let i = Inputs {
                                occupancy,
                                energy_period,
                                main_mode,
                                aux_zone_occupied: false,
                                comfort_setpoint,
                                aux_comfort_setpoint: 16.0,
                                back_during_recovery,
                            };
                            let d = decide(&i);
                            if d.main_mode == Cool {
                                assert!(
                                    d.main_setpoint >= SETPOINT_MIN_COOL,
                                    "heating-grade setpoint under cool mode \
                                     (the July-7 shape): {i:?} -> {d:?}"
                                );
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
        let i = inputs(Occupancy::Away, EnergyPeriod::Normal, HvacMode::Cool);
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
        let i = inputs(Occupancy::Home, EnergyPeriod::Peak, HvacMode::Heat);
        let d = decide(&i);
        assert!(d.shed_loads);
        assert_eq!(d.main_setpoint, 16.0);
        assert_eq!(d.aux_zone_setpoint, AUX_FROST_FLOOR);

        // shedding is a peak thing only - preheat and normal keep loads on
        for period in [EnergyPeriod::Normal, EnergyPeriod::Preheat] {
            let d = decide(&inputs(Occupancy::Home, period, HvacMode::Heat));
            assert!(!d.shed_loads);
        }
    }

    #[test]
    fn returning_gets_milder_peak_and_richer_preheat_than_away() {
        let away_peak = decide(&inputs(Occupancy::Away, EnergyPeriod::Peak, HvacMode::Heat));
        let ret_peak = decide(&inputs(
            Occupancy::AwayReturning,
            EnergyPeriod::Peak,
            HvacMode::Heat,
        ));
        assert!(ret_peak.main_setpoint > away_peak.main_setpoint);

        let away_pre = decide(&inputs(
            Occupancy::Away,
            EnergyPeriod::Preheat,
            HvacMode::Heat,
        ));
        let ret_pre = decide(&inputs(
            Occupancy::AwayReturning,
            EnergyPeriod::Preheat,
            HvacMode::Heat,
        ));
        assert!(ret_pre.main_setpoint > away_pre.main_setpoint);
    }

    #[test]
    fn comfort_hold_honored_at_home_but_never_during_grid_events_or_away() {
        // hold value chosen to differ from every matrix cell it meets, so
        // each assertion can only pass for the right reason
        let mut i = inputs(Occupancy::Home, EnergyPeriod::Normal, HvacMode::Heat);
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
        let mut i = inputs(Occupancy::Home, EnergyPeriod::Normal, HvacMode::Heat);
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
        let mut i = inputs(Occupancy::Away, EnergyPeriod::Preheat, HvacMode::Heat);
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
    fn off_day_turns_mode_off_but_keeps_fan_on() {
        let i = inputs(Occupancy::Home, EnergyPeriod::Normal, HvacMode::Off);
        let d = decide(&i);
        assert_eq!(d.main_mode, HvacMode::Off);
        assert_eq!(d.fan_mode, FanMode::On);
    }

    #[test]
    fn aux_zone_follows_occupancy_and_cool_days_turn_it_off() {
        let mut i = inputs(Occupancy::Home, EnergyPeriod::Normal, HvacMode::Heat);
        assert_eq!(decide(&i).aux_zone_setpoint, 16.0);
        i.aux_zone_occupied = true;
        assert_eq!(decide(&i).aux_zone_setpoint, 19.0);
        i.main_mode = HvacMode::Cool;
        assert_eq!(decide(&i).aux_zone_setpoint, AUX_FROST_FLOOR);
    }

    #[test]
    fn aux_hold_honored_at_home_but_never_during_grid_events_or_away() {
        let mut i = inputs(Occupancy::Home, EnergyPeriod::Normal, HvacMode::Heat);
        i.aux_comfort_setpoint = 21.0;
        assert_eq!(decide(&i).aux_zone_setpoint, 21.0, "hold applied at home");

        i.aux_zone_occupied = true;
        assert_eq!(
            decide(&i).aux_zone_setpoint,
            21.0,
            "hold outranks occupancy"
        );

        i.energy_period = EnergyPeriod::Peak;
        assert_eq!(
            decide(&i).aux_zone_setpoint,
            AUX_FROST_FLOOR,
            "peak overrides the hold"
        );

        i.energy_period = EnergyPeriod::Preheat;
        assert_eq!(
            decide(&i).aux_zone_setpoint,
            26.0,
            "preheat overrides the hold"
        );

        i.energy_period = EnergyPeriod::Normal;
        i.occupancy = Occupancy::Away;
        assert_eq!(
            decide(&i).aux_zone_setpoint,
            19.0,
            "a stale hold is ignored when away (occupied base applies)"
        );

        i.occupancy = Occupancy::Home;
        i.aux_comfort_setpoint = 2.0;
        assert_eq!(
            decide(&i).aux_zone_setpoint,
            AUX_FROST_FLOOR,
            "a lowball hold clamps to the frost floor"
        );
    }

    /// Without heat demand the aux zone (basement baseboards) holds
    /// exactly the frost floor: no comfort heating (caught in shadow on an
    /// off day - the old (_, Normal) arm armed the basement at 16C in
    /// July, and (Off, Preheat) would have boosted it to 26C), but never
    /// off either - the persisted 5C setpoint is the passive backstop
    /// that still defends the house if the main source, the daemon or HA
    /// itself is dysfunctional, peak or no peak.
    #[test]
    fn aux_zone_holds_only_the_frost_floor_without_heat_demand() {
        use EnergyPeriod::*;
        use HvacMode::*;
        use Occupancy::*;

        for main_mode in [Off, Cool] {
            for energy_period in [Normal, Preheat, Peak] {
                for occupancy in [Home, HomeAsleep, AwayReturning, Away, AwayFar] {
                    for back_during_recovery in [true, false] {
                        for aux_zone_occupied in [true, false] {
                            let i = Inputs {
                                occupancy,
                                energy_period,
                                main_mode,
                                aux_zone_occupied,
                                comfort_setpoint: 0.0,
                                // a lingering hold must not leak into days
                                // with no heat demand
                                aux_comfort_setpoint: 22.0,
                                back_during_recovery,
                            };
                            let d = decide(&i);
                            assert_eq!(
                                d.aux_zone_setpoint, AUX_FROST_FLOOR,
                                "aux zone must hold exactly the frost floor \
                                 without heat demand: {i:?} -> {d:?}"
                            );
                        }
                    }
                }
            }
        }
    }
}
