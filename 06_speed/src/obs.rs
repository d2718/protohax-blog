/*!
Types for keeping track of observations of vehicles.
*/
use crate::message::LPString;

/// A struct so we don't get our timestamps and our days confused.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Day(u32);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Obs {
    pub mile: u16,
    pub timestamp: u32,
}

impl Obs {
    pub fn new(mile: u16, timestamp: u32) -> Obs { Obs { mile, timestamp }}

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

        let d0 = self.mile as f32;
        let t0 = self.timestamp as f32;
        let d1 = other.mile as f32;
        let t1 = other.timestamp as f32;

        // hundredths of a mile
        let d_d = (d1 - d0) * 100.0;
        // hours
        let d_t = (t1 - t0) / 3600.0;

        let fspeed = (d_d / d_t).abs();

        // The spec says this will never happen, but we're going to make sure
        // we don't crash just in case it does. We'll consider it UB and just
        // return the easiest thing.
        if fspeed > 65530.0 {
            return 0;
        }

        let uspeed: u16 = fspeed.floor() as u16;
        uspeed
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
    pub road: u16,
    pub start: Obs,
    pub end: Obs,
    pub speed: u16,
}

/// Stores a record of observations and issued tickets.
pub struct Car {
    plate: LPString,
    road: u16,
    observations: Vec<Obs>,
    ticketed: Vec<Day>,
}

impl Car {
    pub fn new(plate: LPString, road: u16, mile: u16, timestamp: u32) -> Car {
        Car {
            plate, road,
            observations: vec![Obs::new(mile, timestamp)],
            ticketed: Vec::new(),
        }
    }

    /// Record this car as being observed under the provided conditions.
    ///
    /// If an Infraction is warranted, return that.
    pub fn observed(&mut self, road: u16, limit: u16, obs: Obs) -> Option<Infraction> {
        let mut r_val: Option<Infraction> = None;

        if self.road == road {
            for &prev in self.observations.iter() {
                let speed = obs.speed_between(&prev);
                if speed > limit {
                    if obs.timestamp > prev.timestamp {
                        r_val = Some(Infraction{
                            road, speed,
                            start: prev,
                            end: obs,
                        });
                    } else {
                        r_val = Some(Infraction{
                            road, speed,
                            start: obs,
                            end: prev
                        });
                    }
                    break;
                }
            }
            self.observations.push(obs);

        } else {
            self.road = road;
            self.observations = vec![obs];
        }

        r_val
    }

    /// Check whether this vehicle was ticketed on the given day. if so,
    /// mark it as having been ticketed.
    pub fn ok_to_ticket(&mut self, d: Day) -> bool {
        if self.ticketed.contains(&d) {
            return false;
        }

        self.ticketed.push(d);
        true
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn between_obs() {
        let o1 = Obs::new(8, 0);
        let o2 = Obs::new(9, 45);
        assert_eq!(o1.speed_between(&o2), 8000);
        assert_eq!(o2.speed_between(&o1), 8000);

        // Same times should be 0 speed.
        let o1 = Obs::new(5, 25);
        let o2 = Obs::new(6, 26);
        assert_eq!(o1.speed_between(&o2), 0);
        assert_eq!(o2.speed_between(&o1), 0);

        // Incredibly fast should also be zero speed.
        let o1 = Obs::new(1000, 25);
        let o2 = Obs::new(12, 26);
        assert_eq!(o1.speed_between(&o2), 0);
        assert_eq!(o2.speed_between(&o1), 0);
    }
}