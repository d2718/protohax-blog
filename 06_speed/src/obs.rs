/*!
Types for keeping track of observations of vehicles.
*/
use std::collections::{BTreeMap, BTreeSet};

use tracing::{event, Level};

use crate::bio::LPString;

/// A struct so we don't get our timestamps and our days confused.
///
/// We need to derive all these traits because we're going to be storing
/// them in a BTreeSet.
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
    ///
    /// Obviously, it only makes sense to use this to compare two observations
    /// of the same vehicle on the same road.
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
    /// In hundredths of miles per hour.
    pub speed: u16,
}

/// Stores a record of observations and issued tickets.
#[derive(Debug)]
pub struct Car {
    plate: LPString,
    /// Keys are road numbers; values are vectors of observations.
    observations: BTreeMap<u16, Vec<Obs>>,
    ticketed: BTreeSet<Day>,
}

impl Car {
    /// Instantiate a new car with the given plate and observation parameters.
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

                    // We're going to issue a ticket for this vehicle, and
                    // don't want to issue another one for the day (or days)
                    // of the observations for this one.
                    self.ticketed.insert(ticket.start.day());
                    // Inserting into a BTreeSet is idempotent, so it doesn't
                    // matter if the start and end days are the same.
                    self.ticketed.insert(ticket.end.day());
                    // Remove all records of observations on ticketed days.
                    // These observations can't be used to generate more
                    // tickets, and it'll reduce the amount of checking
                    // we'll have to do on future insertions.
                    for (_, obs_v) in self.observations.iter_mut() {
                        obs_v.retain(|o| !self.ticketed.contains(&o.day()));
                    }

                    return Some(ticket);
                }
            }
            list.push(obs);
        } else {
            self.observations.insert(road, vec![obs]);
        }

        None
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
