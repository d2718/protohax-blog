/*!
The Line-reversal application layer.

Types used by the individual tasks responsible for each endpoint.
*/
#![allow(dead_code)]

use std::{
    sync::Arc,
    collections::VecDeque,
    future::Future,
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};

use tokio::{
    sync::mpsc::{Receiver, UnboundedSender},
    time::{Instant, Sleep},
};
use tracing::{event, Level};

use crate::types::{
    BLOCK_SIZE,
    MsgBlock,
    Pkt, PktType,
    Response,
    SessionId,
};

const CONNECTION_TIMEOUT: Duration = Duration::from_secs(60);
const RETRANSMISSION_TIMEOUT: Duration = Duration::from_secs(3);

struct Data {
    data: MsgBlock,
    count: usize,
    text_start: usize,
    length: usize,
}

impl Data {
    fn data(&self) -> &[u8] {
        &self.data.as_ref()[self.text_start..self.length]
    }
}

enum Msg {
    Ack { count: usize },
    Close,
    Connect,
    Data(Data),
}

impl Msg {
    fn get_count(pkt: &Pkt) -> Result<(usize, usize), &'static str> {
        let count_start = pkt.id_end + 1;
        let count_end = pkt.data.as_ref()[count_start..].iter()
            .position(|&b| b == b'/')
            .ok_or("no end of count field")?;
        let count_str = std::str::from_utf8(&pkt.data.as_ref()[count_start..count_end])
            .map_err(|_| "count not valid UTF-8")?;
        let count: usize = count_str.parse()
            .map_err(|_| "count unparseable")?;
        Ok((count, count_end))
    }
    
    pub fn from_pkt(pkt: Pkt) -> Result<Msg, &'static str> {
        match pkt.ptype {
            PktType::Ack => {
                let (count, _) = Msg::get_count(&pkt)?;
                Ok(Msg::Ack{ count })
            },
            PktType::Close => Ok(Msg::Close),
            PktType::Connect => Ok(Msg::Connect),
            PktType::Data => {
                let (count, count_end) = Msg::get_count(&pkt)?;
                let text_start = count_end + 1;
                let data = pkt.data;
                let length = pkt.length - 1;
                if text_start > length {
                    return Err("data ends before it starts");
                }
                let d = Data { data, count, text_start, length };
                Ok(Msg::Data(d)) 
            }
        }
    }
}

struct SleepTimer {
    sleeper: Pin<Box<Sleep>>,
    sleeping: bool,
}

impl SleepTimer {
    fn new() -> SleepTimer {
        SleepTimer { 
            sleeper: Box::pin(tokio::time::sleep(CONNECTION_TIMEOUT)),
            sleeping: false,
        }
    }

    fn sleep(&mut self, d: Duration) {
        (&mut self.sleeper).as_mut().reset(Instant::now() + d);
        self.sleeping = true;
    }
}

impl Future for SleepTimer {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        if self.sleeping {
            match (&mut self.sleeper).as_mut().poll(cx) {
                Poll::Pending => Poll::Pending,
                Poll::Ready(()) => {
                    self.sleeping = false;
                    Poll::Ready(())
                }
            }
        } else {
            Poll::Pending
        }
    }
}

struct LineReverser {
    id: SessionId,
    idstr: String,
    buffer: Vec<u8>,
    queue: VecDeque<Response>,
    in_bytes: usize,
    out_bytes: usize,
    current_out: Option<Arc<Response>>,
}

impl LineReverser {
    fn new(id: SessionId) -> LineReverser {
        let idstr = String::from_utf8_lossy(&id.as_slice()).to_string();
        LineReverser {
            id, idstr,
            buffer: Vec::with_capacity(BLOCK_SIZE),
            queue: VecDeque::new(),
            in_bytes: 0,
            out_bytes: 0,
            current_out: None,
        }
    }

    fn next_response(&mut self) -> Option<Arc<Response>> {
        if self.current_out.is_some() {
            return None;
        }

        if let Some(r) = self.queue.pop_front() {
            let a = Arc::new(r);
            self.current_out = Some(a.clone());
            Some(a)
        } else {
            None
        }
    }

    fn ack(&mut self, count: usize) -> Result<(), Response> {
        if count == self.out_bytes {
            self.current_out = None;
        } else if count > self.out_bytes {
            event!(Level::WARN,
                "{}: ack count ({}) too high (current: {})",
                &self.idstr, &count, &self.out_bytes
            );
            let r = Response::close(self.id.as_slice());
            return Err(r);
        }
        Ok(())
    }

    fn push(&mut self, r: Response) {
        self.queue.push_back(r);
    }

    fn read_data(&mut self, d: Data) -> Result<(), Response> {
        if d.count > self.in_bytes {
            let r = Response::ack(&self.id.as_slice(), self.in_bytes);
            self.push(r);
            return Ok(());
        } else if d.count < self.in_bytes {
            // Presumably they are resending something we've already received.
            return Ok(());
        }

        let mut count = 0;
        let data = d.data();
        if data.len() > 0 {
            if data[0] != b'\\' {
                self.buffer.push(data[0]);
                count += 1;
            }
        }
        for w in data.windows(2) {
            match unsafe {
                (w.get_unchecked(0), w.get_unchecked(1))
            }{
                (b'\\', b'\\') => {
                    self.buffer.push(b'\\');
                    count += 1;
                },
                (_, b'\\') => continue,
                (b'\\', b'/') => {
                    self.buffer.push(b'/');
                    count += 1;
                },
                (b'\\', &c) => {
                    self.buffer.push(b'\\');
                    self.buffer.push(c);
                    count += 2;
                },
                (_, &c) => {
                    self.buffer.push(c);
                    count += 1;
                },
            }
        }

        self.in_bytes += count;
        let r = Response::ack(self.id.as_slice(), self.in_bytes);
        self.push(r);
        Ok(())
    }
  
}

pub async fn run(id: SessionId, tx: UnboundedSender<Arc<Response>>, mut rx: Receiver<Pkt>) {
    let mut cx_timeout = SleepTimer::new();
    cx_timeout.sleep(CONNECTION_TIMEOUT);
    let mut tx_timeout = SleepTimer::new();

    let mut lr = LineReverser::new(id);

    loop {
        if let Some(a) = lr.next_response() {
            tx.send(a).expect("channel to main task closed");
            tx_timeout.sleep(RETRANSMISSION_TIMEOUT);
        }

        tokio::select!{
            pkt = rx.recv() => {
                let pkt = match pkt {
                    Some(pkt) => pkt,
                    None => {
                        event!(Level::ERROR,
                            "{}: channel from main task unexpectedly dropped",
                            &lr.idstr,
                        );
                        return;
                    },
                };
                cx_timeout.sleep(CONNECTION_TIMEOUT);

                match Msg::from_pkt(pkt) {
                    Err(e) => {
                        event!(Level::WARN, "{}: invalid Pkt: {}", &lr.idstr, &e);
                        continue;
                    },
                    Ok(Msg::Ack{ count }) => {
                        if let Err(r) = lr.ack(count) {
                            tx.send(Arc::new(r)).expect("channel to main task closed");
                            return;
                        }
                    },
                    Ok(Msg::Close) => {
                        let r = Response::close(lr.id.as_slice());
                        tx.send(Arc::new(r)).expect("channel to main task closed");
                        return;
                    },
                    Ok(Msg::Connect) => {
                        let r = Response::ack(lr.id.as_slice(), 0);
                        lr.push(r);
                    },
                    Ok(Msg::Data(d)) => {
                        if let Err(r) = lr.read_data(d) {
                            tx.send(Arc::new(r)).expect("channel to main task closed");
                            return;
                        }
                    },
                }
            }
        }
    }
}
