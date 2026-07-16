//! Layer-1 inputs: the perception enums computed by HA template sensors
//! (configuration.d/hvac2.yaml). The daemon consumes them as entities and
//! parses them into real types at the boundary — an unknown/unavailable
//! state clears the slot and suspends decisions instead of acting on junk.

/// Entities the daemon subscribes to (perception layer, computed in HA).
pub const ENTITY_OCCUPANCY: &str = "sensor.hvac2_occupancy";
pub const ENTITY_ENERGY_PERIOD: &str = "sensor.hvac2_energy_period";
pub const ENTITY_SEASON: &str = "sensor.hvac2_season";
pub const ENTITY_BASEMENT_OCCUPIED: &str = "binary_sensor.hvac2_basement_occupied";
pub const ENTITY_COMFORT_OVERRIDE: &str = "input_select.hvac2_comfort_override";

/// Entities the daemon writes to (actuation layer).
pub const ENTITY_MAIN_HVAC: &str = "climate.neviweb130_climate_hvac";
pub const ENTITY_WATER_HEATER: &str = "switch.waterheater_commutateur";
pub const BASEMENT_THERMOSTATS: [&str; 3] = [
    "climate.basementbathroom_thermostat",
    "climate.basementhall_thermostat",
    "climate.basementmainroom_thermostat",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Occupancy {
    Home,
    HomeAsleep,
    Returning,
    Away,
    AwayFar,
}

impl Occupancy {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "home" => Some(Self::Home),
            "home_asleep" => Some(Self::HomeAsleep),
            "returning" => Some(Self::Returning),
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ComfortOverride {
    #[default]
    None,
    TooCold,
    TooHot,
}

impl ComfortOverride {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "none" => Some(Self::None),
            "too_cold" => Some(Self::TooCold),
            "too_hot" => Some(Self::TooHot),
            _ => None,
        }
    }

    pub fn is_active(self) -> bool {
        !matches!(self, Self::None)
    }
}

/// Complete, validated snapshot of the perception layer.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Inputs {
    pub occupancy: Occupancy,
    pub energy_period: EnergyPeriod,
    pub season: Season,
    pub basement_occupied: bool,
    pub comfort_override: ComfortOverride,
}

/// Accumulates entity states as they arrive; yields `Inputs` only once every
/// slot holds a valid value. `unknown`/`unavailable` clears the slot.
#[derive(Debug, Default)]
pub struct RawInputs {
    occupancy: Option<Occupancy>,
    energy_period: Option<EnergyPeriod>,
    season: Option<Season>,
    basement_occupied: Option<bool>,
    comfort_override: Option<ComfortOverride>,
}

impl RawInputs {
    /// Ingest a state update. Returns true if the update was for an entity we
    /// track and changed its slot.
    pub fn ingest(&mut self, entity_id: &str, state: &str) -> bool {
        match entity_id {
            ENTITY_OCCUPANCY => Self::set(&mut self.occupancy, Occupancy::parse(state)),
            ENTITY_ENERGY_PERIOD => Self::set(&mut self.energy_period, EnergyPeriod::parse(state)),
            ENTITY_SEASON => Self::set(&mut self.season, Season::parse(state)),
            ENTITY_BASEMENT_OCCUPIED => Self::set(
                &mut self.basement_occupied,
                match state {
                    "on" => Some(true),
                    "off" => Some(false),
                    _ => None,
                },
            ),
            ENTITY_COMFORT_OVERRIDE => {
                Self::set(&mut self.comfort_override, ComfortOverride::parse(state))
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
            basement_occupied: self.basement_occupied?,
            comfort_override: self.comfort_override?,
        })
    }
}
