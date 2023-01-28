/*!
Types to get passed through channels.
*/
use tokio::sync::oneshot;

/// Chunks of information sent from a Client to the Room.
#[derive(Debug)]
pub enum Event {
    Join{ id: usize, name: String, membership: oneshot::Sender<String> },
    Leave{ id: usize },
    Text{ id: usize, text: String },
}

/// Lines of text to be sent from the Room to the Clients.
#[derive(Clone, Debug)]
pub struct Message {
    pub id: usize,
    pub text: String
}