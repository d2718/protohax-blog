/*!
Protohackers Problem 6: Speed Daemon

Implement an
[automatic ticket-issusing system](https://protohackers.com/problem/6).
*/
mod clients;
mod error;
mod events;
mod message;
mod obs;

use std::collections::BTreeMap;

use tokio::{
    net::TcpListener,
    sync::mpsc,
};
use tracing::{event, Level};
use tracing_subscriber::{
    fmt::layer,
    filter::EnvFilter,
    prelude::*,
};

use crate::{
    clients::Client,
    events::Event,
    message::{LPString, LPU16Array},
    obs::{Car, Infraction, Obs},
};

static LOCAL_ADDR: &str = "0.0.0.0:12321";
/// Size of outgoing channels; ingoing channel is unbounded.
const CHAN_SIZE: usize = 16;

/// Holds a handle to each dispatcher task from the main task.
struct Dispatcher{
    chan: mpsc::Sender<Infraction>,
    roads: LPU16Array,
}

/// Add the given observation to the appropriate record in the cars map,
/// returning a ticket if appropriate.
fn add_observation(
    map: &mut BTreeMap<LPString, Car>,
    plate: LPString,
    road: u16,
    limit: u16,
    pos: Obs
) -> Option<Infraction> {
    // We will ticket at most once for each infraction.
    let mut ticket: [Option<Infraction>; 2] = None;

    if let Some(car) = map.get_mut(&plate) {
        if let Some(inf) = car.observed(road, limit, pos) {
            let start_day = inf.start.day();
            let end_day = inf.end.day();

            if car.ok_to_ticket(start_day) {
                // If the end day is different, the Car can't have gotten a
                // ticket on _either_ day.
                if start_day != end_day {
                    if car.ok_to_ticket(end_day) {
                        ticket = Some(inf);
                        event!(Level::DEBUG,
                            "ticketing {} on days {:?} and {:?}",
                            &plate, &start_day, &end_day
                        );
                    }
                } else {
                    ticket = Some(inf);
                    event!(Level::DEBUG,
                        "ticketing {} on day {:?}",
                        &plate, &inf.start.day()
                    );
                }
            }
        }
    } else {
        map.insert(plate, Car::new(plate, road, pos));
    }

    return ticket;
}

/// Send the ticket Event to an appropriate dispatcher, or queue it for later
/// if none are connected.
async fn dispose_ticket(
    coverage: &BTreeMap<u16, Vec<usize>>,
    dispatchers: &BTreeMap<usize, Dispatcher>,
    tickets: &mut BTreeMap<u16, Vec<Infraction>>,
    ticket: Infraction,
) {
    if let Some(ids) = coverage.get(&ticket.road) {
        for id in ids.iter() {
            if let Some(d) = dispatchers.get(id) {
                if let Err(e) = d.chan.send(ticket).await {
                    event!(Level::ERROR,
                        "can't send to dispatcher {}: {}", id, &e
                    );
                }
                return;
            }
        }
    }

    tickets.entry(ticket.road).or_default().push(ticket);
}

/// Remove all of a given dispatcher's roads from the coverage map.
fn remove_roads(
    coverage: &mut BTreeMap<u16, Vec<usize>>,
    id: usize,
    d: Dispatcher)
{
    for road in d.roads.as_slice().iter() {
        let mut empty_vec = false;
        if let Some(v) = coverage.get_mut(road) {
            if let Some(i) = v.iter().position(|&oid| oid == id) {
                v.swap_remove(i);
                if v.is_empty() {
                    empty_vec = true;
                }
            }
        }
        if empty_vec {
            coverage.remove(&road);
        }
    }
}

#[tokio::main]
async fn main() {
    tracing_subscriber::registry()
    .with(layer())
    .with(EnvFilter::from_default_env())
    .init();

    // Receives Events from clients.
    let (tx, mut rx) = mpsc::unbounded_channel::<Event>();
    // Holds channels to as-of-yet-unidentified clients.
    let mut unid_clients: BTreeMap<usize, mpsc::Sender<Infraction>>
        = BTreeMap::new();
    // Maps ids to Dispatchers.
    let mut dispatchers: BTreeMap<usize, Dispatcher> = BTreeMap::new();
    // Maps road numbers to dispatcher ids.
    let mut coverage: BTreeMap<u16, Vec<usize>> = BTreeMap::new();
    // Maps roads to undelivered tickets.
    let mut tickets: BTreeMap<u16, Vec<Infraction>> = BTreeMap::new();
    // Maps plates to cars.
    let mut cars: BTreeMap<LPString, Car> = BTreeMap::new();

    let listener = TcpListener::bind(LOCAL_ADDR).await.unwrap();
    event!(Level::DEBUG, "listening on {:?}", &listener.local_addr().unwrap());

    let mut client_n: usize = 0;
    loop {
        tokio::select!{
            res = listener.accept() => match res {
                Ok((stream, addr)) => {
                    event!(Level::DEBUG,
                        "accpted client {} from {:?}", &client_n, &addr
                    );
                    let (out_tx, out_rx) = mpsc::channel::<Infraction>(CHAN_SIZE);
                    let client = Client::new(
                        client_n, stream, tx.clone(), out_rx
                    );
                    unid_clients.insert(client_n, out_tx);
                    tokio::spawn(async move {
                        client.run().await;
                    });
                    client_n += 1;
                },
                Err(e) => {
                    event!(Level::WARN,
                        "error accepting client: {}", &e
                    );
                }
            },

            evt = rx.recv() => {
                event!(Level::TRACE,
                    "main rec'd {:?}", &evt
                );

                let evt = evt.expect("main channel received None");
                match evt {
                    Event::Observation{ plate, road, limit, pos } => {
                        if let Some(ticket) = add_observation(
                            &mut cars, plate, road, limit, pos
                        ) {
                            dispose_ticket(
                                &coverage,
                                &dispatchers,
                                &mut tickets,
                                ticket
                            ).await;
                        }
                    },

                    Event::Gone{ id } => {
                        if let Some(d) = dispatchers.remove(&id) {
                            remove_roads(&mut coverage, id, d);
                            event!(Level::TRACE,
                                "removed Dispatcher id {}", &id
                            );
                        } else {
                            if unid_clients.remove(&id).is_some() {
                                event!(Level::TRACE,
                                    "removed unidentified client {}", &id
                                );
                            } else {
                                event!(Level::TRACE,
                                    "no client with id {} (was Camera?)", &id
                                );
                            }
                        }
                    },

                    Event::Camera{ id } => {
                        if unid_clients.remove(&id).is_some() {
                            event!(Level::TRACE,
                                "removed {}'s channel", &id
                            );
                        } else {
                            event!(Level::WARN,
                                "no client with id {}", &id
                            );
                        }
                    },

                    Event::Dispatcher{ id, roads } => {
                        if let Some(chan) = unid_clients.remove(&id) {
                            for &road in roads.as_slice().iter() {
                                coverage.entry(road).or_default().push(id);
                                if let Some(ticket_vec) = tickets.remove(&road) {
                                    event!(Level::TRACE,
                                        "sending {} pending tickets", &ticket_vec.len()
                                    );
                                    for ticket in ticket_vec.into_iter() {
                                        if let Err(e) = chan.send(ticket).await {
                                            event!(Level::ERROR,
                                                "error sending backlogged ticket to Dispatcher {}: {}",
                                                &id, &e
                                            )
                                        }
                                    }
                                }
                            }

                            let d = Dispatcher { chan, roads };
                            dispatchers.insert(id, d);
                            event!(Level::TRACE,
                                "moved chan {} and inserted Dispatcher", &id
                            );
                        } else {
                            event!(Level::ERROR,
                                "no client with id {}", &id
                            );
                        }
                    },
                }
            }
        }
    }
}
