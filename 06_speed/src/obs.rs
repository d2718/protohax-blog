/*!
Types for keeping track of observations of vehicles.
*/
use std::collections::{BTreeMap, BTreeSet};

use tracing::{event, Level};

use crate::bio::LPString;

/// A struct so we don't get our timestamps and our days confused.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct Day(u32);

/// A single observation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Obs {
    pub mile: u16,
    pub timestamp: u32,
}

impl Obs {
    /// Determing the average speed between two observations
    /// in miles per hour x100.
    pub fn speed_between(&self, other: &Obs) -> u16 {
        // I'm just going to arbitrarily return 0 here to avoid division
        // by zero.
        //
        // If the locations are _different_, then the vehicle is moving
        // faster than a u16 can represent, which the spec says it won't
        // be, so we'll consider it UB and return something that shouldn't
        // have an effect. (A speed of 0 shouldn't generate a ticket.)
        if self.timestamp == other.timestamp {
            return 0;
        }

        // We cast to a signed integer type in case d0 > d1 or t0 > t1;
        // we cast to a larger integer type so we don't overflow.
        let d0 = self.mile as i64;
        let t0 = self.timestamp as i64;
        let d1 = other.mile as i64;
        let t1 = other.timestamp as i64;

        // We multiply our numerator by 3600 instead of dividing our
        // denominator by 3600; that way we only ever truncate on
        // the final division operation.
        let d_d = (d1 - d0) * 100 * 3600;
        let d_t = t1 - t0;
        let ispeed = (d_d / d_t).abs();

        // The spec says this will never happen, but we're going to make sure
        // we don't crash just in case it does. We'll consider it UB and just
        // return the easiest thing.
        if ispeed > 65535 {
            return 0;
        }

        ispeed as u16
    }

    /// Number of days since epoch on which this observation occurred.
    pub fn day(&self) -> Day {
        // number of seconds in a day
        Day(self.timestamp / 86400)
    }
}

/// The coordinates that go along with a speeding ticket.
#[derive(Clone, Copy, Debug)]
pub struct Infraction {
    pub plate: LPString,
    pub road: u16,
    pub start: Obs,
    pub end: Obs,
    pub speed: u16,
}

/// Stores a record of observations and issued tickets.
pub struct Car {
    plate: LPString,
    observations: BTreeMap<u16, Vec<Obs>>,
    ticketed: BTreeSet<Day>,
}

impl Car {
    pub fn new(plate: LPString, road: u16, obs: Obs) -> Car {
        let mut observations = BTreeMap::new();
        observations.insert(road, vec![obs]);
        Car {
            plate,
            observations,
            ticketed: BTreeSet::new(),
        }
    }

    /// Record this car as being observed under the provided conditions.
    ///
    /// If an Infraction is warranted, mark the car as having been ticketed
    /// on that day (or those days), and return the Infraction.
    pub fn observed(&mut self, road: u16, limit: u16, obs: Obs) -> Option<Infraction> {
        let d = obs.day();
        if self.ticketed.contains(&d) {
            event!(
                Level::DEBUG,
                "{} already ticketed on {:?}; ignorning",
                &self.plate,
                &d
            );
            return None;
        }

        if let Some(list) = self.observations.get_mut(&road) {
            for &prev in list.iter().filter(|o| !self.ticketed.contains(&o.day())) {
                let speed = obs.speed_between(&prev);
                if speed > limit {
                    let ticket = if obs.timestamp > prev.timestamp {
                        Infraction {
                            plate: self.plate,
                            road,
                            speed,
                            start: prev,
                            end: obs,
                        }
                    } else {
                        Infraction {
                            plate: self.plate,
                            road,
                            speed,
                            start: obs,
                            end: prev,
                        }
                    };
                    self.set_ticketed(&ticket);
                    return Some(ticket);
                }
            }
            list.push(obs);
        } else {
            self.observations.insert(road, vec![obs]);
        }

        None
    }

    /// Record that this vehicle was ticketed on the given day(s) of
    /// the given Infraction.
    ///
    /// Also removes any observations from days on which the vehicle has
    /// been ticketed; they are no longer of use.
    fn set_ticketed(&mut self, i: &Infraction) {
        self.ticketed.insert(i.start.day());
        // If both days are the same, this is a no-op for a BTreeSet.
        self.ticketed.insert(i.end.day());
        for (_, obs_v) in self.observations.iter_mut() {
            obs_v.retain(|o| !self.ticketed.contains(&o.day()));
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn between_obs() {
        let o1 = Obs {
            mile: 8,
            timestamp: 0,
        };
        let o2 = Obs {
            mile: 9,
            timestamp: 45,
        };
        assert_eq!(o1.speed_between(&o2), 8000);
        assert_eq!(o2.speed_between(&o1), 8000);

        // Same times should be 0 speed.
        let o1 = Obs {
            mile: 5,
            timestamp: 25,
        };
        let o2 = Obs {
            mile: 6,
            timestamp: 26,
        };
        assert_eq!(o1.speed_between(&o2), 0);
        assert_eq!(o2.speed_between(&o1), 0);

        // Incredibly fast should also be zero speed.
        let o1 = Obs {
            mile: 1000,
            timestamp: 25,
        };
        let o2 = Obs {
            mile: 12,
            timestamp: 26,
        };
        assert_eq!(o1.speed_between(&o2), 0);
        assert_eq!(o2.speed_between(&o1), 0);
    }
}
