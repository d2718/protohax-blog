title: Protohackers in Rust, Part 06 (b)
subtitle: Observations and Architecture
time: 2023-02-21 10:55:00
og_image: https://d2718.net/blog/images/rust_logo.png
og_desc: More async Rust in action solving the seventh Protohackers problem
mathjax: true

You are reading an element of a series on solving the [Protohackers](https://protohackers.com) problems. Some previous offerings, if you're interested:

  * [Problem 0: Smoke Test](https://d2718.net/blog/posts/protohax_00.html)
  * [Problem 1: Prime Time](https://d2718.net/blog/posts/protohax_01.html)
  * [Problem 2: Means to an End](https://d2718.net/blog/posts/protohax_02.html)
  * [Problem 3: Budget Chat](https://d2718.net/blog/posts/protohax_03.html)
  * [Problem 4: Unusual Database Program](https://d2718.net/blog/posts/protohax_04.html)

This post itself is the second in a _subseries_ on solving [The Seventh Problem](https://protohackers.com/problem/6). [Here is the first part.](https://d2718.net/blog/posts/protohax_06a.html).

## Vehicles, Observations, and Tickets

The pseudoreal goal of this exercise is to determine when vehicles have committed speeding violations, whether a vehicle should be ticketed for any given infraction, and if so, to send the information about that infraction to a ticket dispatcher client that has responsibility for the road on which the violation took place. We must make these decisions from reports that come from camera clients.

Each camera sends messages with licence plates and the timestamps of when the vehicle with said plate passed the given camera's position on its given road. Each data point is essentially `(plate, road, position, time)`. Any time two of these observations show the same vehicle on the same road such that the $\frac{\Delta x}{\Delta t}$ between them is greater than the speed limit on that road, we know by the [mean value theorem](https://en.wikipedia.org/wiki/Mean_value_theorem) that the vehicle has exceeded the speed limit at some point.

We will maintain a set of `Car` structs, indexed by license plate.[^plate-indexed] Each `Car` will maintain a set of vectors of observations, indexed by road. Each time an observation comes in, we'll check it against all other observations of that vehicle on the same road to see if any are far apart enough in space and close together enough in time to warrant an infraction. If the vehicle has not already been earned an infraction for that day,[^day-days] we will issue one.

[^plate-indexed]: Because that's how they're identified.

[^day-days]: Or _days_; if the two observations that precipitate an infraction occur on different days, the infraction counts for _both_ of those days.

Let's lay some ground work. `src/obs.rs`:

```rust
/*!
Types for keeping track of observations of vehicles.
*/
use std::collections::{BTreeMap, BTreeSet};

use tracing::{event, Level};

use crate::bio::LPString;
```

The first thing we'll do is create a [newtype struct](https://rust-unofficial.github.io/patterns/patterns/behavioural/newtype.html)[^type-alias] to represent _days_, so we don't get them confused with the timestamps the cameras give us (and that we use when issuing tickets):

[^type-alias]: Declaring types like `type Day = u32; type Timestamp = u32;` doesn't help here. This only creates _type aliases_ for `u32`, not new types themselves. Nothing would stop you from using a `Day` value as a `Timestamp` argument for a function, because those both just mean `u32` to the compiler.

```rust
/// A struct so we don't get our timestamps and our days confused.
///
/// We need to derive all these traits because we're going to be storing
/// them in a BTreeSet.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct Day(u32);
```

If this were a more serious engineering project, we would probably generate newtypes for _all_ of our numerical types: roads, miles, seconds, and speeds, as well as days. But days and timestamps are both times that infractions occurred, and are both represented by `u32`s, so I figured I'd be more likely to mix them up than any of my other numeric types.

Observations of any given vehicle will be grouped by road, so we only need to store positions and times.

```rust
/// A single observation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Obs {
    pub mile: u16,
    pub timestamp: u32,
}
```

We will add a method that determines the average speed between two observations, and also a method to return the `Day` on which a given observation occurred.

```rust
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
```

The `Obs::speed_between()` method reports speed in _hundredths of a mile per hour_. This is because

  1. This is how the specification requires we report speeds when issuing tickets.
  2. Working in hundredths makes us less likely to fall victim to rounding errors than we would if we were working in integer numbers of whole miles per hour.

We will also define a struct to hold the parameters of a ticket.[^not-message]

[^not-message]: We'll talk about why we don't just use a `bio::ServerMesage::Ticket` for this later, but you can probably hazard a guess now.

```rust
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
```

And here is our `Car` struct.[^car-vehicle] It has a `BTreeMap` of observations (indexed by road number), and a `BTreeSet` of the days on which this vehicle has been ticketed. It probably doesn't need to hold its own plate number, but having it there will make it easier to print debugging messages.

[^car-vehicle]: `Car` is shorter than the undoubtedly more accurate `RoadVehicle` while sufficiently conveying the point. Also, the problem statement itself uses the term "car" quite a bit.

```rust
/// Stores a record of observations and issued tickets.
#[derive(Debug)]
pub struct Car {
    plate: LPString,
    /// Keys are road numbers; values are vectors of observations.
    observations: BTreeMap<u16, Vec<Obs>>,
    ticketed: BTreeSet<Day>,
}
```

The `Car` will have a single constructor and a single method to register observations. The constructor will take observation information as arguments, because there's no use in creating a record for a vehicle before it's been observed. The observation registration method will check the new observation against any others on the same road and return an `Infraction` if one is warranted. In this case, it will also record that the `Car` has been ticketed on the day (or days, if the later observation is on the following day from the earlier observation) listed therein. It will also remove all prior observations from that day (or those days), because we can no longer use observations from those days to generate tickets. This will reduce the total amount of data that we have to store, and also the amount of iteration and comparison we'll have to do on the insertion of each subsequent observation.

```rust
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
```

We now have all the machinery we need in order to determine when we should issue a ticket to a given vehicle based on our camera clients' reports of it.

## Architecture

Now that we

  * have our data structures worked out
  * are free from thinking about the details of communicating over the wire or comparing individual observations
  * thought quite a bit about our goal while acutally working toward it
  
we are well-armed to consider the structure of our program at a high level.

As in the prior [Budget Chat](https://d2718.net/blog/posts/protohax_03.html) problem, we will have to manage a number of simultaneously-connected clients, and as in the [UDP Database](https://d2718.net/blog/posts/protohax_04.html) problem, we will have to store some state based on the input we have received from clients, and respond to other clients based on that state. It makes sense, then, to have our architecture be a sort of combination of the two. We will have a "main" task that will store, synthesize, and analyze our data, and then we will have a separate task for managing each of our client connections. The main task will communicate with the client tasks through channels. The client tasks will pass messages[^not-messages] to the main task through a single [`mpsc`](https://docs.rs/tokio/latest/tokio/sync/mpsc/index.html) channel; the main task will use individual `mpsc`s to send `Infraction` information to tasks managing dispatcher clients. (Camera clients will not need incoming channels, but unidentified clients will, because once identified they may become dispatchers.)

[^not-messages]: Although we will not use `bio::ClientMessage` for this.

As before, having a single task manage all the state and communicate via channels allows us to not have to worry about locking or ownership or passing references between tasks or threads.[^golang]

[^golang]: I'm not a _huge_ fan of Golang (obviously, I much prefer Rust, and I prefer it even for domains firmly in Go's wheelhouse), but I do think this is one place where the Go team really hit the mark: Go strongly encourages structuring one's application as a group of cooperating tasks communicating via channels. It's an elegant, satisfying design pattern.

Let's do some more ASCII art.

```
┌-----------------------┐                        ┌--------------┐
|                       |                  +--<--| Unidentified |ReadHalf--<┐
|                       |                  |     |              |           |===| TcpStream |===
|                       |-->--| mpsc |-->--|-->--|    Client    |WriteHalf->┘
|                       |                  |     └--------------┘
|                       |                  V
|                       |                  |     ┌--------------┐
|                       |                  +--<--| Unidentified |ReadHalf--<┐
|                       |                  |     |              |           |===| TcpStream |===
|                       |-->--| mpsc |-->--|-->--|    Client    |WriteHalf->┘
|                       |                  |     └--------------┘
|                       |                  |
|                       |                  /            etc...
|                       |
|                       |                  /
|                       |                  |     ┌--------------┐
|                       |                  +--<--|    Camera    |ReadHalf--<┐
|       Main Task       |                  |     |              |           |===| TcpStream |===
|       ---------       |                  V     |    Client    |WriteHalf->┘
|                       |                  |     └--------------┘
| stores observations   |--<--| mpsc |--<--+
|                       |     (Events)     |     ┌--------------┐
| makes decisions about |                  +--<--|    Camera    |ReadHalf--<┐
| issuing infractions   |                  |     |              |           |===| TcpStream |===
|                       |                  |     |    Client    |WriteHalf->┘
|                       |                  A     └--------------┘
|                       |                  |
|                       |                  /            etc...
|                       |
|                       |                  /
|                       |                  |     ┌--------------┐
|                       |                  +--<--|  Dispatcher  |ReadHalf--<┐
|                       |                  |     |              |           |===| TcpStream |===
|                       |-->--| mpsc |-->--|-->--|    Client    |WriteHalf->┘
|                       |  (Infractions)   |     └--------------┘
|                       |                  A
|                       |                  |     ┌--------------┐
|                       |                  +--<--|  Dispatcher  |ReadHalf--<┐
|                       |                  |     |              |           |===| TcpStream |===
|                       |-->--| mpsc |-->--|-->--|    Client    |WriteHalf->┘
|                       |  (Infractions)   |     └--------------┘
|                       |                  |
/                       /                  A            etc...
                                           |
/                       /                  /
|                       |
└-----------------------┘
```

The only information that needs to be communicated _from_ the main task _to_ the clients is ticket information to dispatcher clients, so the `mpsc` channels flowing that direction will carry the `obs::Infraction` type. The pieces of information that needs to be communicated _to_ the main task are:

  * a client becoming a camera or a dispatcher
  * a client disconnecting
  * vehicular observations

We thus define an `Event` type, to be carried by the channel flowing back to the main task from the client tasks.

The entirety of `src/events.rs`:

```rust
/*!
The types of messages that can be sent from a client task to the central
server task.
*/
use crate::{
    bio::{LPString, LPU16Array},
    obs::Obs,
};

#[derive(Clone, Debug)]
pub enum Event {
    /// The client with the given id is a camera.
    Camera { id: usize },
    /// The client with the given id is a dispatcher in charge of the
    /// given roads.
    Dispatcher { id: usize, roads: LPU16Array },
    /// The client with the given id has disconnected.
    Gone { id: usize },
    /// The given car was observed on the given road at the given
    /// pos coordinates.
    Observation {
        plate: LPString,
        road: u16,
        limit: u16,
        pos: Obs,
    },
}
```

So now we have our plan. Client tasks will communicate with connected devices using the types in the `obs` module, and pass the required information through a channel to the main task. The main task will store all the state, decide when to issue tickets, and communicate those details to the client tasks managing the appropriate dispatchers. Next time we'll start with slogging through the nitty-gritty of the client-handling implementation.