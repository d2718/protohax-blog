/*!
The types of messages that can be sent from a client task to the central
server task.
*/
use crate::{
    message::{LPU16Array, LPString},
    obs::Obs,
};

#[derive(Clone, Debug)]
pub enum Event {
    /// The client with the given id is a camera.
    Camera{ id: usize },
    /// The client with the given id is a dispatcher in charge of the
    /// given roads.
    Dispatcher{ id: usize, roads: LPU16Array },
    /// The client with the given id has disconnected.
    Gone{ id: usize },
    /// The given car was observed on the given road at the given
    /// pos coordinates.
    Observation {
        plate: LPString,
        road: u16,
        limit: u16,
        pos: Obs,
    },
}