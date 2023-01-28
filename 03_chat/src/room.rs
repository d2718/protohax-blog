/*!
The chat server's driving nexus.
*/
use std::{
    collections::BTreeMap,
    rc::Rc,
};

use tokio::{
    sync::{broadcast::Sender, mpsc::Receiver}
};

use crate::message::{Event, Message};

pub struct Room {
    /// Stores the names of connected clients.
    clients: BTreeMap<usize, String>,
    from_clients: Receiver<Event>,
    to_clients: Sender<Rc<Message>>,
}

impl Room {
    pub fn new(
        from_clients: Receiver<Event>,
        to_clients: Sender<Rc<Message>>
    ) -> Room {
        let clients: BTreeMap<usize, String> = BTreeMap::new();

        Room { clients, from_clients, to_clients }
    }

    // Generate a list of names of clients currently connected.
    fn also_here(&self) -> String {
        if self.clients.is_empty() {
            return "* No one else is here.\n".into();
        }

        let mut name_iter = self.clients.iter();
        // This is safe because `clients` has at least 1 member.
        let (_, name) = name_iter.next().unwrap();
        let mut names = format!("* Also here: {}", name);

        for (_, name) in name_iter {
            names.push_str(", ");
            names.push_str(name.as_str())
        }
        names.push('\n');

        names
    }

    // Broadcast `msg` to all joined `Client`s, and deal with the non-error
    // if there aren't any.
    fn send(&self, msg: Message) {
        log::debug!("Room sending {:?}", &msg);
        match self.to_clients.send(Rc::new(msg)) {
            Ok(n) => { log::debug!("    reached {} clients.", &n); },
            Err(_) => { log::debug!("    no subscribed clients."); }
        }
    }

    pub async fn run(mut self) -> Result<(), String> {
        log::debug!("Room is running.");

        while let Some(evt) = self.from_clients.recv().await {
            match evt {
                Event::Text{ id, text } => {
                    // The given `id` should definitely be in the clients map.
                    let name = self.clients.get(&id).unwrap();
                    let text = format!("[{}] {}", name, &text);
                    self.send(Message{ id, text });
                },

                Event::Join{ id, name, membership } => {
                    let text = self.also_here();
                    self.clients.insert(id, name);
                    if let Err(e) = membership.send(text) {
                        log::error!(
                            "Failed to send membership message to Client {}: {:?}",
                            id, &e
                        );
                    }
                    // It should be clear why this unwrap() will succeed.
                    let name = self.clients.get(&id).unwrap();
                    let text = format!("* {} joins.\n", name);
                    self.send(Message{ id, text });
                },

                Event::Leave{ id } => {
                    if let Some(name) = self.clients.remove(&id) {
                        let text = format!("* {} leaves.\n", &name);
                        self.send(Message{ id, text });
                    }
                },
            }
        }

        Ok(())
    }
}