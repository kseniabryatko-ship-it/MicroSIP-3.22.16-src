//! SIP over a connection: TCP, or TLS on top of it.
//!
//! Over UDP every datagram is one message. A connection is a stream of
//! bytes, so messages are cut out of it by their `Content-Length`
//! ([`Framer`]). The phone keeps one connection, to its SIP server
//! ([`StreamLink`]): everything for the server goes out through it and the
//! server sends its requests and answers back the same way. A thread of the
//! link's own reads it and hands whole messages over a channel, so the
//! phone's loop never waits on the network.

use std::fmt;
use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

/// How SIP messages travel to the server.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SipTransport {
    #[default]
    Udp,
    Tcp,
    Tls,
}

impl SipTransport {
    pub const ALL: [SipTransport; 3] = [SipTransport::Udp, SipTransport::Tcp, SipTransport::Tls];

    /// The name as written in a Via header and kept in the settings.
    pub const fn name(self) -> &'static str {
        match self {
            SipTransport::Udp => "UDP",
            SipTransport::Tcp => "TCP",
            SipTransport::Tls => "TLS",
        }
    }

    /// Reads a name in any letter case; anything unknown is UDP.
    pub fn from_name(name: &str) -> Self {
        Self::ALL
            .into_iter()
            .find(|transport| transport.name().eq_ignore_ascii_case(name.trim()))
            .unwrap_or_default()
    }

    /// What a SIP address of this phone carries so that the server comes
    /// back the same way; nothing for UDP.
    pub const fn uri_parameter(self) -> &'static str {
        match self {
            SipTransport::Udp => "",
            SipTransport::Tcp => ";transport=tcp",
            SipTransport::Tls => ";transport=tls",
        }
    }

    /// The port servers listen on when none is given.
    pub const fn default_port(self) -> u16 {
        match self {
            SipTransport::Tls => 5061,
            _ => 5060,
        }
    }

    pub const fn is_stream(self) -> bool {
        !matches!(self, SipTransport::Udp)
    }
}

impl fmt::Display for SipTransport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.name())
    }
}

/// A message longer than this is not SIP; the connection is given up.
const MESSAGE_LIMIT: usize = 256 * 1024;

/// Cuts SIP messages out of a stream of bytes.
#[derive(Debug, Default)]
pub struct Framer {
    buffer: Vec<u8>,
}

/// The stream holds something that cannot be a SIP message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FramingError;

impl Framer {
    /// Takes the bytes just read and returns the messages now complete.
    pub fn push(&mut self, bytes: &[u8]) -> Result<Vec<Vec<u8>>, FramingError> {
        self.buffer.extend_from_slice(bytes);
        let mut messages = Vec::new();
        loop {
            // Empty lines between messages are keep-alives.
            let blank = self
                .buffer
                .iter()
                .take_while(|byte| matches!(byte, b'\r' | b'\n'))
                .count();
            self.buffer.drain(..blank);

            let Some(head) = find(&self.buffer, b"\r\n\r\n") else {
                if self.buffer.len() > MESSAGE_LIMIT {
                    return Err(FramingError);
                }
                break;
            };
            let body = content_length(&self.buffer[..head]).ok_or(FramingError)?;
            let total = head + 4 + body;
            if total > MESSAGE_LIMIT {
                return Err(FramingError);
            }
            if self.buffer.len() < total {
                break;
            }
            messages.push(self.buffer.drain(..total).collect());
        }
        Ok(messages)
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// The body length a message head announces. A head without the header has
/// no body; a value that is not a number is an error (`None`).
fn content_length(head: &[u8]) -> Option<usize> {
    let text = String::from_utf8_lossy(head);
    for line in text.split("\r\n").skip(1) {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let name = name.trim();
        if name.eq_ignore_ascii_case("content-length") || name.eq_ignore_ascii_case("l") {
            return value.trim().parse().ok();
        }
    }
    Some(0)
}

/// A request as it must leave over `transport`. The phone builds its
/// requests for UDP; the transport named in the first Via, which is the
/// phone's own, is corrected here, at the one place where bytes leave.
/// Responses carry the Via of the request they answer and are not touched.
pub fn with_via_transport(message: &[u8], transport: SipTransport) -> Vec<u8> {
    const OURS: &[u8] = b"SIP/2.0/UDP ";
    if transport == SipTransport::Udp || message.starts_with(b"SIP/2.0 ") {
        return message.to_vec();
    }
    let head_end = find(message, b"\r\n\r\n").unwrap_or(message.len());
    let head = &message[..head_end];
    // The first Via line, by its full or its short name.
    let mut line_start = 0;
    while line_start < head.len() {
        let line_end = find(&head[line_start..], b"\r\n").map_or(head.len(), |at| line_start + at);
        let line = &head[line_start..line_end];
        let name_end = line.iter().position(|byte| *byte == b':').unwrap_or(0);
        let name = String::from_utf8_lossy(&line[..name_end]);
        let name = name.trim();
        if name.eq_ignore_ascii_case("via") || name.eq_ignore_ascii_case("v") {
            if let Some(at) = find(line, OURS) {
                let at = line_start + at;
                let mut changed = message[..at].to_vec();
                changed.extend_from_slice(format!("SIP/2.0/{} ", transport.name()).as_bytes());
                changed.extend_from_slice(&message[at + OURS.len()..]);
                return changed;
            }
            break;
        }
        line_start = line_end + 2;
    }
    message.to_vec()
}

/// What the link's reading thread reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkEvent {
    /// One whole SIP message from the server.
    Message(Vec<u8>),
    /// The connection is gone; the text says why.
    Closed(String),
}

type Tls = native_tls::TlsStream<TcpStream>;

enum Writer {
    Tcp(Mutex<TcpStream>),
    /// The reading thread shares the stream: TLS cannot be split in two.
    Tls(Arc<Mutex<Tls>>),
}

/// The connection to the SIP server.
pub struct StreamLink {
    writer: Writer,
    events: Receiver<LinkEvent>,
    peer: SocketAddr,
    local: SocketAddr,
    transport: SipTransport,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

/// How long connecting and the TLS greeting may take.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// How long a write may wait on a connection that stopped taking data.
const WRITE_TIMEOUT: Duration = Duration::from_secs(3);
/// How long the reading thread waits for bytes before looking at `stop`.
const READ_SLICE: Duration = Duration::from_millis(100);
/// The same for TLS, where a write has to wait for the read to let go.
const TLS_READ_SLICE: Duration = Duration::from_millis(20);

impl StreamLink {
    /// Connects to `peer`. For TLS, `server_name` is the name the server's
    /// certificate must be for; with `verify` off any certificate is taken
    /// (the conversation is still encrypted, but the server is not proven
    /// to be who it says).
    pub fn connect(
        peer: SocketAddr,
        transport: SipTransport,
        server_name: &str,
        verify: bool,
    ) -> io::Result<Self> {
        let stream = TcpStream::connect_timeout(&peer, CONNECT_TIMEOUT)?;
        stream.set_nodelay(true)?;
        stream.set_write_timeout(Some(WRITE_TIMEOUT))?;
        let local = stream.local_addr()?;
        let (sender, events) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = Arc::clone(&stop);

        let (writer, thread) = match transport {
            SipTransport::Tls => {
                stream.set_read_timeout(Some(CONNECT_TIMEOUT))?;
                let connector = native_tls::TlsConnector::builder()
                    .danger_accept_invalid_certs(!verify)
                    .danger_accept_invalid_hostnames(!verify)
                    .build()
                    .map_err(io::Error::other)?;
                let tls = connector
                    .connect(server_name, stream)
                    .map_err(|error| io::Error::other(format!("TLS: {error}")))?;
                tls.get_ref().set_read_timeout(Some(TLS_READ_SLICE))?;
                let shared = Arc::new(Mutex::new(tls));
                let for_thread = Arc::clone(&shared);
                let thread = thread::spawn(move || {
                    read_loop(&sender, &stopped, |buffer| {
                        let read = match for_thread.lock() {
                            Ok(mut tls) => tls.read(buffer),
                            Err(_) => return Err(io::Error::other("the connection is unusable")),
                        };
                        // Let a waiting write take the stream.
                        if read.as_ref().is_err_and(is_timeout) {
                            thread::sleep(Duration::from_millis(2));
                        }
                        read
                    });
                });
                (Writer::Tls(shared), thread)
            }
            _ => {
                let mut reader = stream.try_clone()?;
                reader.set_read_timeout(Some(READ_SLICE))?;
                let thread = thread::spawn(move || {
                    read_loop(&sender, &stopped, |buffer| reader.read(buffer));
                });
                (Writer::Tcp(Mutex::new(stream)), thread)
            }
        };
        Ok(Self {
            writer,
            events,
            peer,
            local,
            transport,
            stop,
            thread: Some(thread),
        })
    }

    /// Writes one whole message.
    pub fn send(&self, bytes: &[u8]) -> io::Result<()> {
        let unusable = || io::Error::other("the connection is unusable");
        match &self.writer {
            Writer::Tcp(stream) => stream.lock().map_err(|_| unusable())?.write_all(bytes),
            Writer::Tls(stream) => {
                let mut tls = stream.lock().map_err(|_| unusable())?;
                tls.write_all(bytes)?;
                tls.flush()
            }
        }
    }

    /// The next thing the reading thread has; never waits.
    pub fn try_event(&self) -> Option<LinkEvent> {
        self.events.try_recv().ok()
    }

    /// The server's end of the connection.
    pub fn peer(&self) -> SocketAddr {
        self.peer
    }

    /// This phone's end of the connection.
    pub fn local(&self) -> SocketAddr {
        self.local
    }

    pub fn transport(&self) -> SipTransport {
        self.transport
    }
}

impl Drop for StreamLink {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Writer::Tcp(stream) = &self.writer
            && let Ok(stream) = stream.lock()
        {
            let _ = stream.shutdown(std::net::Shutdown::Both);
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn is_timeout(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
    )
}

/// Reads until told to stop or the connection ends, and reports each whole
/// message.
fn read_loop(
    sender: &Sender<LinkEvent>,
    stop: &AtomicBool,
    mut read: impl FnMut(&mut [u8]) -> io::Result<usize>,
) {
    let mut framer = Framer::default();
    let mut buffer = vec![0_u8; 16 * 1024];
    let closed = |reason: String| {
        let _ = sender.send(LinkEvent::Closed(reason));
    };
    while !stop.load(Ordering::Acquire) {
        match read(&mut buffer) {
            Ok(0) => return closed("the server closed the connection".to_owned()),
            Ok(length) => match framer.push(&buffer[..length]) {
                Ok(messages) => {
                    for message in messages {
                        if sender.send(LinkEvent::Message(message)).is_err() {
                            return;
                        }
                    }
                }
                Err(FramingError) => {
                    return closed("the server sent something that is not SIP".to_owned());
                }
            },
            Err(error) if is_timeout(&error) || error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return closed(error.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::time::Instant;

    const OPTIONS: &str = "OPTIONS sip:a@b SIP/2.0\r\nVia: SIP/2.0/UDP h;branch=z9hG4bK1\r\n\
                           Content-Length: 0\r\n\r\n";

    fn invite(body: &str) -> String {
        format!(
            "INVITE sip:a@b SIP/2.0\r\nv: SIP/2.0/UDP h;branch=z9hG4bK2\r\nl: {}\r\n\r\n{body}",
            body.len()
        )
    }

    #[test]
    fn transport_names_are_read_and_written() {
        for transport in SipTransport::ALL {
            assert_eq!(SipTransport::from_name(transport.name()), transport);
        }
        assert_eq!(SipTransport::from_name(" tls "), SipTransport::Tls);
        assert_eq!(SipTransport::from_name("sctp"), SipTransport::Udp);
        assert_eq!(SipTransport::from_name(""), SipTransport::Udp);
        assert_eq!(SipTransport::Udp.uri_parameter(), "");
        assert_eq!(SipTransport::Tcp.uri_parameter(), ";transport=tcp");
        assert_eq!(SipTransport::Tls.default_port(), 5061);
        assert_eq!(SipTransport::Tcp.default_port(), 5060);
        assert!(SipTransport::Tls.is_stream() && !SipTransport::Udp.is_stream());
    }

    #[test]
    fn messages_are_cut_out_however_the_bytes_arrive() {
        let first = invite("v=0\r\ns=x\r\n");
        let all = format!("{first}\r\n\r\n{OPTIONS}{}", invite(""));
        // All at once.
        let mut framer = Framer::default();
        let messages = framer.push(all.as_bytes()).unwrap();
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[0], first.as_bytes());
        assert_eq!(messages[1], OPTIONS.as_bytes());

        // One byte at a time gives the same messages.
        let mut framer = Framer::default();
        let mut one_by_one = Vec::new();
        for byte in all.as_bytes() {
            one_by_one.extend(framer.push(&[*byte]).unwrap());
        }
        assert_eq!(one_by_one, messages);

        // A body that has not fully arrived is waited for.
        let mut framer = Framer::default();
        let (head, tail) = first.split_at(first.len() - 4);
        assert!(framer.push(head.as_bytes()).unwrap().is_empty());
        assert_eq!(framer.push(tail.as_bytes()).unwrap(), [first.as_bytes()]);
        // Keep-alive line ends alone are nothing.
        assert!(framer.push(b"\r\n\r\n\r\n").unwrap().is_empty());
    }

    #[test]
    fn what_is_not_sip_ends_the_stream() {
        let mut framer = Framer::default();
        let bad = "INVITE sip:a@b SIP/2.0\r\nContent-Length: many\r\n\r\n";
        assert_eq!(framer.push(bad.as_bytes()), Err(FramingError));
        let mut framer = Framer::default();
        let huge = "INVITE sip:a@b SIP/2.0\r\nContent-Length: 99999999\r\n\r\n";
        assert_eq!(framer.push(huge.as_bytes()), Err(FramingError));
        let mut framer = Framer::default();
        assert_eq!(
            framer.push(&vec![b'x'; MESSAGE_LIMIT + 1]),
            Err(FramingError)
        );
    }

    #[test]
    fn only_our_own_via_of_a_request_names_the_transport() {
        let request = "BYE sip:a@b SIP/2.0\r\nMax-Forwards: 70\r\n\
                       Via: SIP/2.0/UDP 10.0.0.1:5060;branch=z9hG4bK1;rport\r\n\
                       Via: SIP/2.0/UDP other;branch=z9hG4bK0\r\n\r\nSIP/2.0/UDP in a body";
        let sent = with_via_transport(request.as_bytes(), SipTransport::Tls);
        let sent = String::from_utf8(sent).unwrap();
        assert!(sent.contains("Via: SIP/2.0/TLS 10.0.0.1:5060;branch=z9hG4bK1;rport\r\n"));
        assert!(sent.contains("Via: SIP/2.0/UDP other;branch=z9hG4bK0\r\n"));
        assert!(sent.ends_with("SIP/2.0/UDP in a body"));
        assert_eq!(sent.len(), request.len());

        // The short header name.
        let short = with_via_transport(invite("").as_bytes(), SipTransport::Tcp);
        assert!(
            String::from_utf8(short)
                .unwrap()
                .contains("v: SIP/2.0/TCP h;")
        );
        // UDP and responses stay as they are.
        assert_eq!(
            with_via_transport(request.as_bytes(), SipTransport::Udp),
            request.as_bytes()
        );
        let response = "SIP/2.0 200 OK\r\nVia: SIP/2.0/UDP h;branch=z9hG4bK1\r\n\r\n";
        assert_eq!(
            with_via_transport(response.as_bytes(), SipTransport::Tcp),
            response.as_bytes()
        );
    }

    fn next_event(link: &StreamLink) -> LinkEvent {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if let Some(event) = link.try_event() {
                return event;
            }
            thread::sleep(Duration::from_millis(5));
        }
        panic!("the link reported nothing");
    }

    #[test]
    fn tcp_link_carries_messages_both_ways_and_reports_the_end() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let link = StreamLink::connect(address, SipTransport::Tcp, "", true).unwrap();
        let (mut server, from) = listener.accept().unwrap();
        assert_eq!(from, link.local());
        assert_eq!(link.peer(), address);
        assert_eq!(link.transport(), SipTransport::Tcp);

        link.send(OPTIONS.as_bytes()).unwrap();
        let mut received = vec![0_u8; OPTIONS.len()];
        server.read_exact(&mut received).unwrap();
        assert_eq!(received, OPTIONS.as_bytes());

        // Two messages in pieces, with a keep-alive between them.
        let first = invite("body");
        server.write_all(&first.as_bytes()[..10]).unwrap();
        server.flush().unwrap();
        thread::sleep(Duration::from_millis(30));
        server.write_all(&first.as_bytes()[10..]).unwrap();
        server.write_all(b"\r\n\r\n").unwrap();
        server.write_all(OPTIONS.as_bytes()).unwrap();
        assert_eq!(next_event(&link), LinkEvent::Message(first.into_bytes()));
        assert_eq!(
            next_event(&link),
            LinkEvent::Message(OPTIONS.as_bytes().to_vec())
        );

        drop(server);
        assert!(matches!(next_event(&link), LinkEvent::Closed(_)));
    }

    #[test]
    fn nobody_listening_is_an_error_at_once() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        assert!(StreamLink::connect(address, SipTransport::Tcp, "", true).is_err());
    }
}
