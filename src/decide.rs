//! Layer 2: the decision matrix as a pure function, and layer 3's write
//! planner. Every temperature in the system lives in this file — the
//! setpoint matrix in `decide()`, the mode thresholds in `demanded_mode()`.
//! Perception hands over facts only; nothing outside this file chooses a
//! number. The compiler enforces that every (main_mode, energy_period,
//! occupancy) combination is handled — the class of gap that caused the
//! 2026-07-07 incident cannot compile.

use crate::state::{EnergyPeriod, HvacMode, Inputs, Occupancy};

/// The aux zone is never commanded off, only down to this frost floor
/// (the Sinope minimum): a setpoint persisted in the device keeps
/// defending the house even if daemon, HA and network all die. Only
/// deferrable loads (load_shed) are ever fully off.
const AUX_FROST_FLOOR: f64 = 5.0;

/// Today's forecast high at or below which the day demands heating, and at
/// or above which it demands cooling. Strictly between them the house is
/// conditioned in neither direction - only circulated (`HvacMode::Circulate`),
/// whose band still cools if the day turns out hotter than forecast.
/// Inherited unchanged from the pre-daemon automation these replaced.
const HEAT_AT_OR_BELOW: f64 = 16.0;
const COOL_AT_OR_ABOVE: f64 = 25.0;

/// The parked side of the band. Every decision commands *both* a heat and
/// a cool setpoint; the mode says which one is doing the work, and the
/// other is pushed out here where the house will not reach it.
///
/// They are not "off" values - they are defended limits. A heating day
/// whose cool ceiling is 30C will still cool a house that somehow reaches
/// 30C, and a cooling day whose heat floor is 16C will still heat a house
/// that falls to 16C. That is the same principle the aux zone has always
/// used (`AUX_FROST_FLOOR`): never command off, command a limit the device
/// keeps defending on its own even if daemon, HA and network are all dead.
const IDLE_COOL_CEILING: f64 = 30.0;
const IDLE_HEAT_FLOOR: f64 = 16.0;

/// The cool ceiling on a mild day: high enough that 26C with air moving is
/// left alone (David: "26 is OK if the fan is ON"), low enough that the
/// house does not become unbearable when the forecast was wrong about how
/// mild the day would be. This is where the mild-day rescue ended up -
/// once the band is published as a pair, "start cooling if it gets too hot
/// anyway" is just this number, and the thermostat supplies the hysteresis
/// that would otherwise have needed a latch.
const MILD_COOL_CEILING: f64 = 27.0;

/// The narrowest band any decision may command. This is the same-day
/// heat/cool interlock, and unlike the forecast dead band it is a property
/// of what we *command* rather than an argument about how far a forecast
/// can move: to be paid to heat and to cool within one day the house must
/// cross the whole band, twice. The device enforces its own minimum
/// (`heat_cool_setpoint_delta`, 2C on the TH6500WF) and would silently
/// widen anything tighter; this sits well above it. The property test over
/// every reachable decision is what keeps it true.
///
/// Test-only, like `MAX_INTRADAY_FORECAST_REVISION` and for the same
/// reason: enforcing it at runtime (clamping a too-narrow band) would hide
/// the matrix bug that produced it. The matrix should be right, and the
/// test is what says so.
#[cfg(test)]
const MIN_BAND_SPREAD: f64 = 5.0;

/// The widest revision of *today's* forecast high that can happen while the
/// day is running. Not the day-to-day swing, which is far larger: a forecast
/// for a maximum only sharpens as that day progresses. Exists solely to be
/// compared against the dead band - see the test named after it. Test-only
/// because it describes the weather, not the house: nothing reads it at
/// runtime, it only has to hold for the interlock to be sound.
#[cfg(test)]
const MAX_INTRADAY_FORECAST_REVISION: f64 = 9.0;

/// What the day demands of the main zone, from today's forecast high.
///
/// **The gap between the two thresholds is load-bearing.** It is the entire
/// same-day heat/cool interlock: the house must never be paid to heat and to
/// cool within one calendar day, and nothing enforces that at runtime. It
/// holds because crossing both thresholds inside one day would take a
/// forecast revision wider than any that occurs. Keep them far apart - the
/// invariant test below is what makes a narrowing fail here rather than on
/// the electricity bill. See `docs/where-policy-lives.md` for the incident
/// that motivated writing this down: a reader could not see the interlock,
/// because an invariant that is only the distance between two constants is
/// invisible.
///
/// `forced` is the drill knob (simulate winter in July). It wins outright,
/// which is what makes the matrix exercisable out of season.
///
/// Note what this no longer decides: whether the house may cool on a mild
/// day. Every mode commands a full band, so a mild day carries its own
/// cool ceiling (`MILD_COOL_CEILING`) and the thermostat starts cooling if
/// the house reaches it. That used to need an indoor-temperature input, a
/// midnight-reset running maximum, and a written argument about
/// hysteresis; it is now one number in the matrix.
pub fn demanded_mode(forecast_max: f64, forced: Option<HvacMode>) -> HvacMode {
    if let Some(mode) = forced {
        return mode;
    }
    if forecast_max >= COOL_AT_OR_ABOVE {
        HvacMode::Cool
    } else if forecast_max <= HEAT_AT_OR_BELOW {
        HvacMode::Heat
    } else {
        HvacMode::Circulate
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
    /// What the day demands. Advisory for equipment that can decide for
    /// itself: a `heat_cool` thermostat is given the band below and picks
    /// the direction, so heat/cool/circulate all reach it as `heat_cool`.
    /// A device without that mode uses this to choose which side of the
    /// band to command. The daemon states the intent either way and does
    /// not assume which kind of equipment is listening.
    pub main_mode: HvacMode,
    /// The band, always both. Never crosses, never narrower than
    /// `MIN_BAND_SPREAD`, and never off - the parked side is a defended
    /// limit, not an absence (see `IDLE_COOL_CEILING`).
    pub heat_setpoint: f64,
    pub cool_setpoint: f64,
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

    // The mode passes through untouched. There used to be a downgrade here
    // turning circulation into `Off` when away or during a peak; publishing
    // a band made it unnecessary and worse. An empty house is expressed as
    // a wide band (19/28), a grid peak as a wider one (12/30) - both let
    // the device keep defending a limit on its own, which `off` does not.
    // The blower question, which is what the downgrade was really about,
    // belongs to `fan_mode` and is answered there independently.
    let main_mode = i.main_mode;

    // Every cell is a band. The mode names the side doing the work; the
    // other is parked at a defended limit. Reading down a column shows what
    // the house is allowed to do, not merely what it is aiming at.
    let (heat_setpoint, cool_setpoint) = match main_mode {
        // A mild day. Nothing runs between the two, which is the point -
        // 26C with the fan on is left alone. MILD_COOL_CEILING is the old
        // "rescue": if the forecast was wrong about the day, cooling
        // starts on its own without any latch to arrange it.
        Circulate => (
            IDLE_HEAT_FLOOR,
            match (i.energy_period, i.occupancy) {
                (Peak, _) => IDLE_COOL_CEILING,
                (_, Home | HomeAsleep | AwayReturning) => MILD_COOL_CEILING,
                (_, Away | AwayFar) => 28.0,
            },
        ),
        Cool => (
            IDLE_HEAT_FLOOR,
            match (i.energy_period, i.occupancy) {
                // A grid peak sheds in both directions. The heating arm has
                // always done this (its Peak row falls to 16/12/10); the
                // cooling side used to ignore energy_period entirely, so a
                // summer peak kept running the compressor while
                // `peak_sheds_all_loads` claimed otherwise. NOTE: there is
                // still no pre-COOL before a summer peak, the mirror of the
                // winter preheat - Normal and Preheat are deliberately the
                // same cell until that policy exists.
                (Peak, _) => IDLE_COOL_CEILING,
                (_, Home) => 25.0,
                (_, HomeAsleep) => 24.0,
                // returning = the home target, early: the lead time exists so
                // the house is *at* comfort on arrival, not approaching it
                (_, AwayReturning) => 25.0,
                (_, Away | AwayFar) => 28.0,
            },
        ),
        // `Off` shares the heating band: it is only reachable through the
        // drill knob now, and a forced-off house still deserves a
        // meaningful published decision.
        Heat | Off => (
            match (i.energy_period, i.occupancy) {
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
            IDLE_COOL_CEILING,
        ),
    };

    // Manual comfort holds are NOT applied here: a hold is enforced by the
    // HA override (the wire stands down and the human's setpoint persists
    // in the device), so `desired` stays the pure matrix decision - what
    // homeostat *would* do. The gap between desired and the device is the
    // override's visible cost. The held value lives in HA only, as a record
    // (dashboard + tuning collector); the daemon does not read it.

    // David's rule, verbatim: always on while home, on demand during a
    // peak or while away. Note this no longer keys off the mode at all.
    // It used to force the fan on whenever the mode was `Off`, which was
    // both useless (an off system ignores the fan setting - the bug that
    // produced `Circulate`) and backwards, since `Off` now means an empty
    // house or a grid peak, the two cases that want the fan on demand.
    let fan_mode = if i.occupancy.is_home() && i.energy_period != Peak {
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
    let aux_zone_setpoint = match (main_mode, i.energy_period) {
        (Cool | Circulate | Off, _) => AUX_FROST_FLOOR,
        (Heat, Peak) => AUX_FROST_FLOOR,
        (Heat, Preheat) => 26.0,
        // aux holds, like main holds, are enforced by the HA override, not
        // here - this stays the pure matrix decision
        (Heat, Normal) => {
            if i.aux_zone_occupied {
                19.0
            } else {
                16.0
            }
        }
    };

    Desired {
        main_mode,
        heat_setpoint,
        cool_setpoint,
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
            back_during_recovery: true,
        }
    }

    /// The same-day heat/cool interlock, and the reason it needs no
    /// machinery. Paying to heat and to cool within one calendar day is
    /// never right, and the protection against it is not a latch or a
    /// state machine - it is that the two thresholds are further apart than
    /// today's forecast high can move while today is happening. Narrow the
    /// dead band and this fails, which is the whole point: the property is
    /// otherwise invisible, being an emergent consequence of two constants
    /// rather than any code you can read.
    #[test]
    fn the_dead_band_makes_a_same_day_reversal_unreachable() {
        let dead_band = COOL_AT_OR_ABOVE - HEAT_AT_OR_BELOW;
        assert!(
            dead_band >= MAX_INTRADAY_FORECAST_REVISION,
            "dead band is {dead_band}C, narrower than a reachable intraday \
             forecast revision ({MAX_INTRADAY_FORECAST_REVISION}C): one \
             calendar day could then demand heating and cooling in turn"
        );
    }

    /// Both thresholds are inclusive, and the drill knob outranks the
    /// forecast so the matrix stays exercisable out of season.
    #[test]
    fn demanded_mode_reads_the_forecast_and_yields_to_the_drill_knob() {
        use HvacMode::*;

        assert_eq!(demanded_mode(25.0, None), Cool, "cool is inclusive");
        assert_eq!(demanded_mode(24.9, None), Circulate);
        assert_eq!(demanded_mode(16.0, None), Heat, "heat is inclusive");
        assert_eq!(demanded_mode(16.1, None), Circulate);
        assert_eq!(demanded_mode(20.0, None), Circulate, "dead band circulates");

        assert_eq!(demanded_mode(30.0, Some(Heat)), Heat, "drill wins");
        assert_eq!(demanded_mode(-20.0, Some(Cool)), Cool);
        assert_eq!(demanded_mode(30.0, Some(Off)), Off);
    }

    /// The same-day heat/cool interlock, restated as a property of what we
    /// command rather than an argument about weather. The old form -
    /// "crossing both forecast thresholds in one day would take a revision
    /// wider than any that occurs" - still holds and still has its test,
    /// but it only ever constrained the *mode*. Now that every decision
    /// carries both setpoints, the house can only be paid to heat and then
    /// to cool if it crosses the whole band twice, and that is checkable
    /// directly, on every reachable cell, without believing anything about
    /// forecasts.
    #[test]
    fn no_decision_ever_commands_a_band_narrow_enough_to_fight_itself() {
        use EnergyPeriod::*;
        use HvacMode::*;
        use Occupancy::*;

        for main_mode in [Heat, Cool, Circulate, Off] {
            for energy_period in [Normal, Preheat, Peak] {
                for occupancy in [Home, HomeAsleep, AwayReturning, Away, AwayFar] {
                    for back_during_recovery in [true, false] {
                        for aux_zone_occupied in [true, false] {
                            let i = Inputs {
                                occupancy,
                                energy_period,
                                main_mode,
                                aux_zone_occupied,
                                back_during_recovery,
                            };
                            let d = decide(&i);
                            let spread = d.cool_setpoint - d.heat_setpoint;
                            assert!(
                                spread >= MIN_BAND_SPREAD,
                                "band {}..{} is {spread}C, narrower than \
                                 MIN_BAND_SPREAD ({MIN_BAND_SPREAD}C): the house \
                                 could be paid to heat and to cool in one day. \
                                 {i:?} -> {d:?}",
                                d.heat_setpoint,
                                d.cool_setpoint,
                            );
                        }
                    }
                }
            }
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
                            let i = Inputs {
                                occupancy,
                                energy_period,
                                main_mode,
                                aux_zone_occupied,
                                back_during_recovery,
                            };
                            let d = decide(&i);
                            {
                                // stronger than it used to be: the old
                                // single setpoint meant a cooling target in
                                // Cool mode, so the floor could not be
                                // checked there. Every decision now carries
                                // a heating side, so every decision is
                                // checked - Cool included.
                                assert!(
                                    d.heat_setpoint >= 10.0,
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

    /// The generalized July-7 invariant: no reachable decision may pair
    /// cool mode with a heating-grade setpoint (the incident's output
    /// shape). Sweeps every input combination.
    #[test]
    fn cool_mode_never_carries_a_heating_grade_setpoint() {
        use EnergyPeriod::*;
        use HvacMode::*;
        use Occupancy::*;

        for main_mode in [Heat, Off, Cool] {
            for energy_period in [Normal, Preheat, Peak] {
                for occupancy in [Home, HomeAsleep, AwayReturning, Away, AwayFar] {
                    for back_during_recovery in [true, false] {
                        for aux_zone_occupied in [true, false] {
                            let i = Inputs {
                                occupancy,
                                energy_period,
                                main_mode,
                                aux_zone_occupied,
                                back_during_recovery,
                            };
                            let d = decide(&i);
                            if d.main_mode == Cool {
                                assert!(
                                    d.cool_setpoint >= 20.0,
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
        assert_eq!(d.cool_setpoint, 28.0);
        assert!(
            d.cool_setpoint >= 26.0,
            "away cool setpoint must be conservative"
        );
    }

    #[test]
    fn peak_sheds_all_loads() {
        let i = inputs(Occupancy::Home, EnergyPeriod::Peak, HvacMode::Heat);
        let d = decide(&i);
        assert!(d.shed_loads);
        assert_eq!(d.heat_setpoint, 16.0);
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
        assert!(ret_peak.heat_setpoint > away_peak.heat_setpoint);

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
        assert!(ret_pre.heat_setpoint > away_pre.heat_setpoint);
    }

    #[test]
    fn bare_preheat_when_provably_absent_past_the_recovery_horizon() {
        let mut i = inputs(Occupancy::Away, EnergyPeriod::Preheat, HvacMode::Heat);
        assert_eq!(decide(&i).heat_setpoint, 23.0, "assume back = boost");
        i.back_during_recovery = false;
        assert_eq!(
            decide(&i).heat_setpoint,
            19.0,
            "absent = hold the normal away baseline, no boost"
        );

        i.occupancy = Occupancy::AwayFar;
        assert_eq!(decide(&i).heat_setpoint, 17.0);

        // only the away preheat cells listen to it
        i.occupancy = Occupancy::Home;
        assert_eq!(decide(&i).heat_setpoint, 25.0);
        i.occupancy = Occupancy::AwayReturning;
        assert_eq!(decide(&i).heat_setpoint, 25.0);
        i.occupancy = Occupancy::Away;
        i.energy_period = EnergyPeriod::Peak;
        assert_eq!(decide(&i).heat_setpoint, 12.0, "peak cells unchanged");
    }

    /// The bug this replaced: a mild day commanded `Off` and asked for the
    /// fan separately, which the equipment ignores because `off` stops the
    /// air handler too. A mild day at home must never command `Off`.
    #[test]
    fn a_mild_day_at_home_circulates_rather_than_going_off() {
        let d = decide(&inputs(
            Occupancy::Home,
            EnergyPeriod::Normal,
            HvacMode::Circulate,
        ));
        assert_eq!(d.main_mode, HvacMode::Circulate);
        assert_eq!(d.fan_mode, FanMode::On);
        assert_eq!(
            (d.heat_setpoint, d.cool_setpoint),
            (IDLE_HEAT_FLOOR, MILD_COOL_CEILING),
            "a mild day is a wide band, not a forced direction"
        );
    }

    /// David's rule: the fan is always on while home, on demand during a
    /// peak or while away. The mode is no longer part of that answer - an
    /// empty house and a grid peak are expressed as a wide band, not as
    /// `Off`, so the device keeps defending a limit either way.
    #[test]
    fn away_and_peak_widen_the_band_and_put_the_fan_on_demand() {
        use EnergyPeriod::*;
        use Occupancy::*;

        for occupancy in [Away, AwayFar] {
            let d = decide(&inputs(occupancy, Normal, HvacMode::Circulate));
            assert_eq!(d.main_mode, HvacMode::Circulate, "intent is unchanged");
            assert_eq!((d.heat_setpoint, d.cool_setpoint), (IDLE_HEAT_FLOOR, 28.0));
            assert_eq!(d.fan_mode, FanMode::Auto, "no blower for an empty house");
        }

        // A grid peak sheds in both directions, cooling included.
        let d = decide(&inputs(Home, Peak, HvacMode::Circulate));
        assert_eq!(
            d.cool_setpoint, IDLE_COOL_CEILING,
            "no cooling through a peak"
        );
        assert_eq!(d.fan_mode, FanMode::Auto);
        let d = decide(&inputs(Home, Peak, HvacMode::Cool));
        assert_eq!(d.cool_setpoint, IDLE_COOL_CEILING, "summer peak sheds too");

        // Returning and asleep are both home: full band, blower running.
        let d = decide(&inputs(AwayReturning, Normal, HvacMode::Circulate));
        assert_eq!(d.cool_setpoint, MILD_COOL_CEILING);
        let d = decide(&inputs(HomeAsleep, Normal, HvacMode::Circulate));
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

    /// Full-path "returning home" scenarios, in David's own vocabulary:
    /// realistic perception minutes -> occupancy bucket -> setpoint, run
    /// through `RawInputs::complete` + `decide` (the only tests that exercise
    /// the whole chain rather than a hand-built `Inputs`). These lock the
    /// three cases he cares about; where the daemon *cannot* express a
    /// distinction (morning vs evening peak), the comment says so.
    #[test]
    fn returning_home_scenarios() {
        use crate::state::{
            RawInputs, ENTITY_AUX_ZONE_OCCUPIED, ENTITY_ENERGY_PERIOD, ENTITY_FORECAST_MAX,
            ENTITY_OCCUPANCY, ENTITY_RECOVERY_HORIZON, ENTITY_RECOVERY_MINUTES, ENTITY_RETURN_ETA,
            ENTITY_RETURN_FLOOR, ENTITY_SLEPT_AWAY,
        };

        /// Representative forecast highs either side of the dead band, so
        /// these scenarios read as seasons rather than as numbers.
        const WINTER_DAY: f64 = -5.0;
        const SUMMER_DAY: f64 = 30.0;

        // occupancy, period, forecast high, return_eta, return_floor,
        // recovery, recovery_horizon (minutes), slept_away -> decided Inputs
        #[allow(clippy::too_many_arguments)]
        fn perceive(
            occ: &str,
            period: &str,
            forecast_max: f64,
            eta: f64,
            floor: f64,
            recovery: f64,
            horizon: f64,
            slept_away: &str,
        ) -> Inputs {
            let mut raw = RawInputs::default();
            raw.ingest(ENTITY_OCCUPANCY, occ);
            raw.ingest(ENTITY_ENERGY_PERIOD, period);
            raw.ingest(ENTITY_FORECAST_MAX, &forecast_max.to_string());
            raw.ingest(ENTITY_AUX_ZONE_OCCUPIED, "off");
            raw.ingest(ENTITY_RETURN_ETA, &eta.to_string());
            raw.ingest(ENTITY_RETURN_FLOOR, &floor.to_string());
            raw.ingest(ENTITY_RECOVERY_MINUTES, &recovery.to_string());
            raw.ingest(ENTITY_RECOVERY_HORIZON, &horizon.to_string());
            raw.ingest(ENTITY_SLEPT_AWAY, slept_away);
            raw.complete().expect("optional inputs never suspend")
        }

        // Case 1 - Winter, heading home, NO peak. Heading home 20 min out
        // (return_eta = floor = 20), warm baseline (recovery 0), no grid
        // event (horizon 0). The 20-min comfort pre-start: returning gets the
        // full home target early, not the away setback.
        let i = perceive("away", "normal", WINTER_DAY, 20.0, 20.0, 0.0, 0.0, "off");
        assert_eq!(i.occupancy, Occupancy::AwayReturning);
        assert_eq!(decide(&i).heat_setpoint, 22.5, "winter returning, no peak");

        // Case 3 - Summer, heading home. Same 20-min lead, cool day. The
        // house should be AT comfort on arrival (25), never the deep 28
        // away setback. (Previously only guarded by the >=20 cool sweep.)
        let i = perceive("away", "normal", SUMMER_DAY, 20.0, 20.0, 0.0, 0.0, "off");
        assert_eq!(i.occupancy, Occupancy::AwayReturning);
        let d = decide(&i);
        assert_eq!(d.main_mode, HvacMode::Cool);
        assert_eq!(d.cool_setpoint, 25.0, "summer returning = comfort early");

        // Case 2, EVENING peak - the must-preheat one. At work, ~45 min out,
        // slept home last night; the recovery horizon (peak end + window) is
        // hours away, so someone is credibly back before the house recovers
        // -> boost the preheat. This is the scenario whose failure mode is
        // "cold for a very long time".
        let i = perceive("away", "preheat", WINTER_DAY, 45.0, 45.0, 0.0, 540.0, "off");
        assert!(i.back_during_recovery, "back before horizon -> boost");
        assert_eq!(decide(&i).heat_setpoint, 23.0, "evening peak preheats");

        // Case 2, thrift end - PROVABLY absent past the horizon. Genuinely
        // far (floor 240) with the horizon only 180 out: no one can be back
        // before the house recovers, so drop the boost and let the peak fall.
        let i = perceive("away", "preheat", WINTER_DAY, 0.0, 240.0, 0.0, 180.0, "off");
        assert!(!i.back_during_recovery, "provably absent -> no boost");
        assert_eq!(i.occupancy, Occupancy::AwayFar);
        assert_eq!(decide(&i).heat_setpoint, 17.0, "bare preheat, far & absent");

        // Case 2, MORNING peak, slept ~2h away, no nav (eta 0, floor 120).
        // Nobody starts driving home at 5AM unannounced: the overnight
        // absence with zero return evidence reads as "not back during the
        // event", so the morning preheat is skipped - hold the far setback.
        // Same symmetric rule as the evening; the slept_away FACT (mornings
        // follow nights) is what distinguishes them, not a wall clock.
        let i = perceive("away", "preheat", WINTER_DAY, 0.0, 120.0, 0.0, 540.0, "on");
        assert!(!i.back_during_recovery, "slept away, no evidence -> skip");
        assert_eq!(i.occupancy, Occupancy::AwayFar);
        assert_eq!(decide(&i).heat_setpoint, 17.0, "morning slept-away skips");

        // Case 2, morning counter-case: slept away but ACTUALLY heading
        // home (nav estimate 60 min, well inside the horizon). Real return
        // evidence outranks the overnight assumption - preheat resumes.
        let i = perceive("away", "preheat", WINTER_DAY, 60.0, 60.0, 0.0, 540.0, "on");
        assert!(
            i.back_during_recovery,
            "evidence of return beats slept_away"
        );
        assert_eq!(decide(&i).heat_setpoint, 23.0, "driving home = boost");
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

        for main_mode in [Off, Cool, Circulate] {
            for energy_period in [Normal, Preheat, Peak] {
                for occupancy in [Home, HomeAsleep, AwayReturning, Away, AwayFar] {
                    for back_during_recovery in [true, false] {
                        for aux_zone_occupied in [true, false] {
                            let i = Inputs {
                                occupancy,
                                energy_period,
                                main_mode,
                                aux_zone_occupied,
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
