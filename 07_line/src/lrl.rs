/*!
The Line-reversal application layer.

Types used by the individual tasks responsible for each endpoint.
*/

use std::{
    sync::Arc,
    collections::{BTreeMap, VecDeque},
    future::Future,
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};

use futures::stream::{
    FusedStream,
    futures_unordered::FuturesUnordered,
    StreamExt,
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

/// If we receive no data from a connection for this long, we will close it.
const CONNECTION_TIMEOUT: Duration = Duration::from_secs(20);
/// If we don't receive an ack of a data packet after this amount of time,
/// we will send it again.
const RETRANSMISSION_TIMEOUT: Duration = Duration::from_secs(3);

/// Message with which to panic if sending on the channel to the main task fails.
static CHAN_CLOSED: &str = "channel to main task closed";
/// Error message returned when copying data into our buffer if we encounter
/// an unescaped '/' before the end of the packet/message.
static TOO_MANY_FIELDS: &str = "unescaped '/' in text segment (too many fields?)";

/// Struct that wraps a parsed data packet.
struct Data {
    data: MsgBlock,
    /// Position in the incoming byte stream where this message fits.
    count: usize,
    /// Offset of the beginning of the payload text.
    text_start: usize,
    length: usize,
}

impl Data {
    /// Return the data of the actual message (the text to be reversed).
    fn data(&self) -> &[u8] {
        &self.data.as_ref()[self.text_start..self.length]
    }
}

/// Represents a fully-parsed message packet.
enum Msg {
    Ack { count: usize },
    Close,
    Connect,
    Data(Data),
}

impl Msg {
    /// Parse the position in the bytestream.
    ///
    ///  * the outgoing bytestream if it's an ack packet
    ///  * the incoming bytestream if it's a data packet
    fn get_count(pkt: &Pkt) -> Result<(usize, usize), &'static str> {
        let count_start = pkt.id_end + 1;
        let count_end = count_start + pkt.data.as_ref()[count_start..].iter()
            .position(|&b| b == b'/')
            .ok_or("no end of count field")?;
        let count_str = std::str::from_utf8(&pkt.data.as_ref()[count_start..count_end])
            .map_err(|_| "count not valid UTF-8")?;
        let count: usize = count_str.parse()
            .map_err(|_| "count unparseable")?;
        Ok((count, count_end))
    }
    
    /// Parse the remainder of an incoming packet.
    pub fn from_pkt(pkt: Pkt) -> Result<Msg, &'static str> {
        match pkt.ptype {
            PktType::Ack => {
                let (count, count_end) = Msg::get_count(&pkt)?;
                if count_end + 1 != pkt.length {
                    Err("ack packet has too many fields")
                } else {
                    Ok(Msg::Ack{ count })
                }
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

/// A sleeping future that will return a specific number when it resolves.
///
/// These are used to track whether retransmission is necessary.
struct SleepTimer {
    sleeper: Pin<Box<Sleep>>,
    count: usize,
}

impl SleepTimer {
    fn new(count: usize) -> SleepTimer {
        SleepTimer { 
            sleeper: Box::pin(tokio::time::sleep(RETRANSMISSION_TIMEOUT)),
            count,
        }
    }
}

impl Future for SleepTimer {
    type Output = usize;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<usize> {
        match (&mut self.sleeper).as_mut().poll(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(()) => Poll::Ready(self.count),
        }
    }
}

/// A container to hold "retransmission timeout" futures and yield them
/// as they resolve.
///
/// We aren't just using a bare `FuturesUnordered` here because when
/// emptied, they "fuse" and can't have anything else added to them. So the
/// `MultiTimer` takes care of discarding fused and instantiating fresh
/// `FuturesUnordered`s when necessary.
struct MultiTimer {
    timers: FuturesUnordered<SleepTimer>,
}

impl MultiTimer {
    fn new() -> MultiTimer {
        MultiTimer { timers: FuturesUnordered::new() }
    }
    
    /// Push a new retransmission timer with the given associated count.
    fn push(&mut self, count: usize) {
        if self.timers.is_terminated() {
            self.timers = FuturesUnordered::new();
        }
        self.timers.push(SleepTimer::new(count))
    }

    /// Resolve to the count of the next data packet to time out.
    ///
    /// Will never resolve if there aren't any unacknowleged data packet
    /// timers to wait on. This also seems to be canecellation-safe,
    /// so we can select! on it.
    async fn next(&mut self) -> usize {
        if self.timers.is_terminated() || self.timers.is_empty() {
            return std::future::pending().await;
        } else {
            match self.timers.next().await {
                Some(n) => n,
                None => std::future::pending().await,
            }
        }
    }
}

/// Envelope for return data messages.
///
/// These store the stream position in them, so they can be easily compared to
/// incoming ack messages.
struct DataResponse {
    response: Arc<Response>,
    count: usize,
}

/// As much of the state required to run a single LRCP session as we can
/// collect together in one struct.
///
/// We've kept the things upon which we need to select! separate, because
/// we need to borrow all of them mutably at the same time.
struct LineReverser {
    id: SessionId,
    /// The SessionId of this session rendered as a UTF-8 string; this helps
    /// in printing log messages so we don't have to format it over and
    /// over again.
    idstr: String,
    /// Holds the incoming byte stream.
    buffer: Vec<u8>,
    /// 
    queue: VecDeque<DataResponse>,
    in_bytes: usize,
    out_bytes: usize,
    buffptr: usize,
    current_out: BTreeMap<usize, Arc<Response>>,
}

impl LineReverser {
    fn new(id: SessionId) -> LineReverser {
        let idstr = String::from_utf8_lossy(id.as_ref()).to_string();
        LineReverser {
            id, idstr,
            buffer: Vec::with_capacity(BLOCK_SIZE),
            queue: VecDeque::new(),
            in_bytes: 0,
            out_bytes: 0,
            buffptr: 0,
            current_out: BTreeMap::new(),
        }
    }

    fn current_response(&self, count: usize) -> Option<Arc<Response>> {
        Some(self.current_out.get(&count)?.clone())
    }

    fn next_response(&mut self) -> Option<DataResponse> {
        let dr = self.queue.pop_front()?;
        self.current_out.insert(dr.count, dr.response.clone());
        Some(dr)
    }

    fn ack(&mut self, count: usize) -> Result<(), Response> {
        if let Some(_) = self.current_out.remove(&count) {
            Ok(())
        } else if count <= self.out_bytes {
            Ok(())
        } else {
            event!(Level::ERROR,
                "ack count value ({}) too high (current max {})",
                &count, &self.out_bytes
            );
            Err(Response::close(&self.id))
        }
    }

    fn analyze_buffer(&mut self) {
        event!(Level::DEBUG,
            "{} analyzing buffer [{}..{}]",
            &self.idstr, &self.buffptr, &self.buffer.len()
        );
        if let Some(idx) = &self.buffer[self.buffptr..].iter()
            .position(|&b| b == b'\n')
        {
            let mut new_v = self.buffer.split_off(self.buffptr + idx + 1);
            std::mem::swap(&mut self.buffer, &mut new_v);
            let n_bytes = new_v.len();
            self.buffptr = 0;
            
            new_v.pop();
            let mut bytes = new_v.drain(..).rev().chain(Some(b'\n'));
            let mut bytes_packed = 0;

            while bytes.size_hint().0 > 0 {
                let count = self.out_bytes + bytes_packed;
                let response = Response::data(&self.id, count, &mut bytes);
                let response = Arc::new(response);
                bytes_packed = n_bytes - bytes.size_hint().0;
                let count = self.out_bytes + bytes_packed;
                let dr = DataResponse{ response, count };
                self.queue.push_back(dr);
            }
            self.out_bytes = self.out_bytes + n_bytes;

            self.analyze_buffer()
        } else {
            self.buffptr = self.buffer.len();
        }
    }

    fn read_data(&mut self, d: Data) -> Result<Option<Response>, Response> {
        event!(Level::DEBUG,
            "{}: read_data([{} bytes]): in: {} out: {}, buffer: {}",
            &self.idstr, &d.data().len(), &self.in_bytes, &self.out_bytes,
            &String::from_utf8_lossy(&self.buffer)
        );
        if d.count > self.in_bytes {
            // We're missing a Data message; we'll ack with the last one we
            // got to try to get them to send it again.
            let r = Response::ack(&self.id, self.in_bytes);
            return Ok(Some(r));
        } else if d.count + d.data().len() <= self.in_bytes {
            // We are guaranteed to already have this entire message (and
            // maybe a few bytes more, if there are escaped bytes), so
            // we can safely ignore this.
            return Ok(None);
        }

        let initial_buff_len = self.buffer.len();
        let mut count = d.count;
        let data = d.data();
        if data.len() > 0 {
            if data[0] != b'\\' {
                if count == self.in_bytes {
                    self.buffer.push(data[0]);
                }
                count += 1;
            }
        }
        for w in data.windows(2) {
            if count < self.in_bytes {
                count += match unsafe {
                    (w.get_unchecked(0), w.get_unchecked(1))
                }{
                    (b'\\', b'\\') => 1,
                    (_,     b'\\') => 0,
                    (b'\\', b'/' ) => 1,
                    (_,     b'/' ) => {
                        event!(Level::WARN,
                            "{}: {}", &self.idstr, TOO_MANY_FIELDS
                        );
                        self.buffer.truncate(initial_buff_len);
                        // This isn't "okay", exactly, but we're ignoring
                        // malformed packets.
                        return Ok(None);
                    },
                    (b'\\', _    ) => 1, //2,
                    _ => 1,
                }
            } else {
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
                    (_,     b'/' ) => {
                        event!(Level::WARN,
                            "{}: {}", &self.idstr, TOO_MANY_FIELDS
                        );
                        self.buffer.truncate(initial_buff_len);
                        // This isn't "okay", exactly, but we're ignoring
                        // malformed packets.
                        return Ok(None);
                    },
                    (b'\\', &c) => {
                        //self.buffer.push(b'\\');
                        self.buffer.push(c);
                        //count += 2;
                        count += 1;
                    },
                    (_, &c) => {
                        self.buffer.push(c);
                        count += 1;
                    },
                }
            }
        }

        self.in_bytes = count;
        let r = Response::ack(&self.id, self.in_bytes);
        self.analyze_buffer();
        Ok(Some(r))
    }
}

pub async fn run(id: SessionId, tx: UnboundedSender<Arc<Response>>, mut rx: Receiver<Pkt>) {
    // let mut cx_timeout = SleepTimer::new(0);
    // cx_timeout.sleep(CONNECTION_TIMEOUT);
    let mut cx_timeout = Box::pin(tokio::time::sleep(CONNECTION_TIMEOUT));
    let mut timeouts = MultiTimer::new();
    let mut lr = LineReverser::new(id);
    
    loop {
        while let Some(dr) = lr.next_response() {
            tx.send(dr.response.clone()).expect(CHAN_CLOSED);
            timeouts.push(dr.count);
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
                //cx_timeout.sleep(CONNECTION_TIMEOUT);
                cx_timeout.as_mut().reset(Instant::now() + CONNECTION_TIMEOUT);

                event!(Level::TRACE, "{} rec'd {:?}", &lr.idstr, &pkt);

                match Msg::from_pkt(pkt) {
                    Err(e) => {
                        event!(Level::WARN, "{}: invalid Pkt: {}", &lr.idstr, &e);
                        continue;
                    },
                    Ok(Msg::Ack{ count }) => {
                        if let Err(r) = lr.ack(count) {
                            tx.send(Arc::new(r)).expect(CHAN_CLOSED);
                            return;
                        }
                    },
                    Ok(Msg::Close) => {
                        let r = Response::close(&lr.id);
                        tx.send(Arc::new(r)).expect(CHAN_CLOSED);
                        return;
                    },
                    Ok(Msg::Connect) => {
                        let r = Response::ack(&lr.id, 0);
                        tx.send(Arc::new(r)).expect(CHAN_CLOSED);
                    },
                    Ok(Msg::Data(d)) => {
                       let res = lr.read_data(d);
                        match res {
                            Ok(Some(r)) => {
                                tx.send(Arc::new(r)).expect(CHAN_CLOSED);
                            },
                            Err(r) => {
                                tx.send(Arc::new(r)).expect(CHAN_CLOSED);
                                return;
                            },
                            _ => { /* continue */ }
                        }
                    },
                }
            },

            count = timeouts.next() => {
                if let Some(a) = lr.current_response(count) {
                    event!(Level::TRACE, "{}: {} timed out, retransmitting", &lr.idstr, &count);
                    tx.send(a).expect(CHAN_CLOSED);
                    timeouts.push(count);
                }
            },

            _ = &mut cx_timeout => {
                event!(Level::TRACE, "{} connection timed out; closing", &lr.idstr);
                let r = Response::close(&lr.id);
                tx.send(Arc::new(r)).expect(CHAN_CLOSED);
                return;
            }
        }
    }
}
