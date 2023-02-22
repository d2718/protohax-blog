title: Protohackers in Rust, Part 06 (d)
subtitle: The Main Task
time: 2023-02-21 13:08:00
og_image: https://d2718.net/blog/images/rust_logo.png
og_desc: More async Rust in action solving the seventh Protohackers problem
mathjax: true

You are reading an element of a series on solving the [Protohackers](https://protohackers.com) problems. Some previous offerings, if you're interested:

  * [Problem 0: Smoke Test](https://d2718.net/blog/posts/protohax_00.html)
  * [Problem 1: Prime Time](https://d2718.net/blog/posts/protohax_01.html)
  * [Problem 2: Means to an End](https://d2718.net/blog/posts/protohax_02.html)
  * [Problem 3: Budget Chat](https://d2718.net/blog/posts/protohax_03.html)
  * [Problem 4: Unusual Database Program](https://d2718.net/blog/posts/protohax_04.html)

This post itself is the fourth (and final) in a _subseries_ on solving [The Seventh Problem](https://protohackers.com/problem/6):

  * [first part](https://d2718.net/blog/posts/protohax_06a.html)
  * [second part](https://d2718.net/blog/posts/protohax_06b.html)
  * [third part](https://d2718.net/blog/posts/protohax_06c.html)

## The Main Task

Let's think about all the things we want the main connection to do:

  * Listen on a socket; when a connection comes in, create a new main -> client channel for it, build the `Client` struct, and `.run()` it in its own task.
  * Receive `Event`s on a central `mpsc` channel:
    + `Event::Plate` => Add the observation to the appropriate `Car` (or insert a new `Car` with this observation), and, if warranted, issue a ticket:
      - If a dispatcher covering the road involved is currently connected, send an `Infraction` down that client task's channel.
      - If there are no connected dispatchers that can deal with it, store it.
    + `Event::Camera` => Drop the channel to the camera's client task; nothing gets sent to those.
    + `Event::Dispatcher` => Register this client as a Dispatcher, and register the roads assigned to it as being assigned to this client task. If there are any stored tickets for any of those roads waiting to be issued, send all of the appropriate `Infraction`s down this new client task's channel.
    + `Event::Gone` => If the client is
      - unidentified => Drop the channel to that client task.
      - a camera => Do nothing, we've already dropped that task's channel.
      - a dispatcher => Unregister that dispatcher, and also remove that dispatcher from responsibility for all of its roads.

There's a lot about state here, so we'll start with some data structures, but first we'll get our "preamble" out of the way.

`src/main.rs`:

```rust
/*!
Protohackers Problem 6: Speed Daemon

Implement an
[automatic ticket-issusing system](https://protohackers.com/problem/6).
*/
mod bio;
mod clients;
mod events;
mod obs;

use std::collections::BTreeMap;

use tokio::{net::TcpListener, sync::mpsc};
use tracing::{event, Level};
use tracing_subscriber::{filter::EnvFilter, fmt::layer, prelude::*};

use crate::{
    bio::{LPString, LPU16Array},
    clients::Client,
    events::Event,
    obs::{Car, Infraction},
};

static LOCAL_ADDR: &str = "0.0.0.0:12321";
/// Size of outgoing channels; return channel is unbounded.
const CHAN_SIZE: usize = 16;
```

### State

The main task is responsible for all[^client-state] the state that needs to be tracked while the program is running. I needs to handle

  * observations of cars, obviously
  * the `Sender` ends of client task channels, and which `id`s and what kinds of clients they feed
  * the road responsibility of connected dispatchers

[^client-state]: Well, each client task had to maintain the state of its own `Heartbeat`, and camera client tasks had to remember the locations of their cameras, but the main task has responsibility for a) the vast majority of the state, and b) all the _shared, mutable_ state.

For tracking individual vehicle observations, we'll have a map that maps plates (`LPString`s) to `Car` structs.

As we've already seen[^past-ids], client tasks will be uniquely identified by a client number. We'll have a map from client ids (`usize`s) to `mpsc::Sender<Event>`s to hold the channels of unidentified clients. These will be dropped when we learn that these clients are cameras, but we will move them to a different map if we learn they are dispatchers. However, we'll also need to keep track of each dispatcher's responsibility (so we can unregister them when they disconnect). To that end, we'll define a struct for the main-task-end of each dispatcher channel:

```rust
/// Holds a handle to each dispatcher task from the main task.
#[derive(Debug)]
struct Dispatcher {
    chan: mpsc::Sender<Infraction>,
    roads: LPU16Array,
}
```
[^past-ids]: And has served us well in previous problems.

So when an unidentified client becomes a dispatcher client, we'll move its channel from the unidentified map to the dispatcher map, which maps client ids to these `Dispatcher` structs. Additionally, so that we can easily select from dispatchers when we need to issue a ticket, we'll store a map from road numbers (`u16`s) to `Vec`s of client ids responsible for each road.

And finally, we need a place to store undelivered tickets that are waiting for the appropriate dispatcher to connect, so we'll also have a map from road ids to `Vec<Infraction>`s.

So the beginning of our `main()` function now looks like this:

```rust
#[tokio::main(flavor = "current_thread")]
async fn main() {
    tracing_subscriber::registry()
        .with(layer())
        .with(EnvFilter::from_default_env())
        .init();

    // Receives Events from clients.
    let (tx, mut rx) = mpsc::unbounded_channel::<Event>();
    // Holds channels to as-of-yet-unidentified clients.
    let mut unid_clients: BTreeMap<usize, mpsc::Sender<Infraction>> = BTreeMap::new();
    // Maps ids to Dispatchers.
    let mut dispatchers: BTreeMap<usize, Dispatcher> = BTreeMap::new();
    // Maps road numbers to dispatcher ids.
    let mut coverage: BTreeMap<u16, Vec<usize>> = BTreeMap::new();
    // Maps roads to undelivered tickets.
    let mut tickets: BTreeMap<u16, Vec<Infraction>> = BTreeMap::new();
    // Maps plates to cars.
    let mut cars: BTreeMap<LPString, Car> = BTreeMap::new();
```

We use `BTreeMap`s instead of `HashMap`s because `BTreeMap` performance is generally better with integer keys and small sizes, both of which we have here.