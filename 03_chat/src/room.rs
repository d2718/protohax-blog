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

    pub async fn run(mut self) -> Result<(), String> {
        log::debug!("Room is running.");

        while let Some(evt) = self.from_clients.recv().await {
            let msg = match evt {
                Event::Text{ id, text } => {
                    // The given `id` should definitely be in the clients map.
                    let name = self.clients.get(&id).unwrap();
                    let text = format!("[{}] {}", name, &text);
                    Rc::new(Message::All{ id, text })
                },

                Event::Join{ id, name } => {
                    let text = self.also_here();
                    self.clients.insert(id, name);
                    // It should be clear why this unwrap() will succeed.
                    let name = self.clients.get(&id).unwrap();
                    let msg = Rc::new(Message::One{ id, text });
                    log::debug!("Room sending {:?}", &msg);
                    // This might return an error, but we don't care.
                    // See the `.send()` at the end of this while let loop.
                    let _ = self.to_clients.send(msg);

                    let text = format!("* {} joins.\n", name);
                    Rc::new(Message::All{ id, text })
                },

                Event::Leave{ id } => {
                    let name = self.clients.remove(&id).unwrap();
                    let text = format!("* {} leaves.\n", &name);
                    Rc::new(Message::All{ id, text })
                },
            };

            log::debug!("Room sending {:?}", &msg);
            match self.to_clients.send(msg) {
                Ok(n) => { log::debug!("    reached {} clients.", &n); },
                // The call to `.send()` returns an error if there are no
                // Receivers subscribed to it, but that isn't necessarily
                // a problem here; it may just be that all the connected
                // clients have left by the time this is being sent.
                Err(_) => { log::debug!("    no subscribed clients."); }
            }
        }

        Ok(())
    }
}