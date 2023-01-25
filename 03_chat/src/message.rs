/*!
Types to get passed through channels.
*/

/// Chunks of information sent from a Client to the Room.
#[derive(Clone, Debug)]
pub enum Event {
    Join{ id: usize, name: String },
    Leave{ id: usize },
    Text{ id: usize, text: String },
}

/// Lines of text to be sent from the Room to the Clients.
#[derive(Clone, Debug)]
pub enum Message {
    /// To be shown to everyone _but_ the Client with the given id.
    All{ id: usize, text: String },
    /// To be shown to _only_ the Client with the given id.
    One{ id: usize, text: String },
}