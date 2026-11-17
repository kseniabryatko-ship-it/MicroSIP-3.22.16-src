//! STUN (RFC 5389), as far as a telephone needs it: asking a server on the
//! internet which address and port this phone's audio socket is seen
//! under. Behind a router (NAT) that is not the socket's own address, and
//! it is the one the other side has to send the voice to.
//!
//! The question is asked from the audio socket itself, because the router
//! gives every socket a mapping of its own.

use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, ToSocketAddrs, UdpSocket};
use std::time::Duration;

/// The port STUN servers listen on when none is given.
pub const DEFAULT_PORT: u16 = 3478;

const MAGIC: [u8; 4] = [0x21, 0x12, 0xA4, 0x42];
const BINDING_REQUEST: [u8; 2] = [0x00, 0x01];
const BINDING_SUCCESS: [u8; 2] = [0x01, 0x01];
const MAPPED_ADDRESS: u16 = 0x0001;
const XOR_MAPPED_ADDRESS: u16 = 0x0020;
/// How long one question waits for its answer, and how often it is asked.
const ATTEMPT: Duration = Duration::from_millis(250);
const ATTEMPTS: usize = 3;

/// A Binding request with the given transaction identifier.
pub fn binding_request(transaction: &[u8; 12]) -> [u8; 20] {
    let mut request = [0_u8; 20];
    request[..2].copy_from_slice(&BINDING_REQUEST);
    // Bytes 2..4 are the length of the attributes: none.
    request[4..8].copy_from_slice(&MAGIC);
    request[8..].copy_from_slice(transaction);
    request
}

/// The address a Binding success response reports, when `message` is the
/// answer to the request with this transaction identifier.
pub fn mapped_address(message: &[u8], transaction: &[u8; 12]) -> Option<SocketAddr> {
    if message.len() < 20
        || message[..2] != BINDING_SUCCESS
        || message[4..8] != MAGIC
        || message[8..20] != transaction[..]
    {
        return None;
    }
    let length = usize::from(u16::from_be_bytes([message[2], message[3]]));
    let mut attributes = message.get(20..20 + length)?;
    let mut plain = None;
    while attributes.len() >= 4 {
        let kind = u16::from_be_bytes([attributes[0], attributes[1]]);
        let size = usize::from(u16::from_be_bytes([attributes[2], attributes[3]]));
        let value = attributes.get(4..4 + size)?;
        match kind {
            // The modern form, hidden from routers that rewrite addresses
            // they find inside packets; preferred when both are there.
            XOR_MAPPED_ADDRESS => {
                if let Some(address) = address(value, Some(transaction)) {
                    return Some(address);
                }
            }
            MAPPED_ADDRESS => plain = plain.or(address(value, None)),
            _ => {}
        }
        // Attributes are padded to a multiple of four bytes.
        let step = 4 + size.div_ceil(4) * 4;
        attributes = attributes.get(step..).unwrap_or(&[]);
    }
    plain
}

/// Reads an address attribute; with `transaction` it is the XOR form.
fn address(value: &[u8], transaction: Option<&[u8; 12]>) -> Option<SocketAddr> {
    let mut mask = [0_u8; 16];
    if let Some(transaction) = transaction {
        mask[..4].copy_from_slice(&MAGIC);
        mask[4..].copy_from_slice(transaction);
    }
    let port = u16::from_be_bytes([value.get(2)? ^ mask[0], value.get(3)? ^ mask[1]]);
    let ip = match (value.get(1)?, value.len()) {
        (0x01, 8) => {
            let mut bytes = [0_u8; 4];
            for (index, byte) in bytes.iter_mut().enumerate() {
                *byte = value[4 + index] ^ mask[index];
            }
            IpAddr::V4(Ipv4Addr::from(bytes))
        }
        (0x02, 20) => {
            let mut bytes = [0_u8; 16];
            for (index, byte) in bytes.iter_mut().enumerate() {
                *byte = value[4 + index] ^ mask[index];
            }
            IpAddr::V6(Ipv6Addr::from(bytes))
        }
        _ => return None,
    };
    Some(SocketAddr::new(ip, port))
}

/// Asks `server` under which address it sees `socket`. Waits up to three
/// quarters of a second in all. The socket's read timeout is left
/// switched off afterwards.
pub fn query(socket: &UdpSocket, server: SocketAddr) -> io::Result<SocketAddr> {
    let mut transaction = [0_u8; 12];
    getrandom::fill(&mut transaction).map_err(io::Error::other)?;
    let request = binding_request(&transaction);
    socket.set_read_timeout(Some(ATTEMPT))?;
    let mut answer = Err(io::Error::new(
        io::ErrorKind::TimedOut,
        "the STUN server did not answer",
    ));
    let mut buffer = [0_u8; 1_500];
    'attempts: for _ in 0..ATTEMPTS {
        if let Err(error) = socket.send_to(&request, server) {
            answer = Err(error);
            break;
        }
        // Anything else arriving on the socket meanwhile is skipped.
        for _ in 0..16 {
            match socket.recv_from(&mut buffer) {
                Ok((length, source)) if source == server => {
                    if let Some(address) = mapped_address(&buffer[..length], &transaction) {
                        answer = Ok(address);
                        break 'attempts;
                    }
                }
                Ok(_) => {}
                Err(_) => break,
            }
        }
    }
    socket.set_read_timeout(None)?;
    answer
}

/// Resolves a STUN server written as `host` or `host:port`.
pub fn resolve(server: &str) -> io::Result<SocketAddr> {
    let server = server.trim();
    // Checked here, not left to the system: Windows makes an address even
    // out of an empty name.
    let no_host = server.starts_with(':') && server.parse::<SocketAddr>().is_err();
    if server.is_empty() || no_host || server.contains(char::is_whitespace) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("invalid STUN server name: {server:?}"),
        ));
    }
    let with_port = if server.parse::<SocketAddr>().is_ok()
        || server
            .rsplit_once(':')
            .is_some_and(|(host, port)| !host.contains(':') && port.parse::<u16>().is_ok())
    {
        server.to_owned()
    } else {
        format!("{server}:{DEFAULT_PORT}")
    };
    with_port.to_socket_addrs()?.next().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::AddrNotAvailable,
            format!("could not resolve the STUN server {server}"),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const TRANSACTION: [u8; 12] = [
        0xB7, 0xE7, 0xA7, 0x01, 0xBC, 0x34, 0xD6, 0x86, 0xFA, 0x87, 0xDF, 0xAE,
    ];

    fn response(attributes: &[u8]) -> Vec<u8> {
        let mut message = BINDING_SUCCESS.to_vec();
        message.extend_from_slice(&(attributes.len() as u16).to_be_bytes());
        message.extend_from_slice(&MAGIC);
        message.extend_from_slice(&TRANSACTION);
        message.extend_from_slice(attributes);
        message
    }

    #[test]
    fn request_is_twenty_bytes_with_the_magic_number() {
        let request = binding_request(&TRANSACTION);
        assert_eq!(request[..8], [0, 1, 0, 0, 0x21, 0x12, 0xA4, 0x42]);
        assert_eq!(request[8..], TRANSACTION);
    }

    #[test]
    fn addresses_of_the_standards_sample_responses_are_read() {
        // RFC 5769, 2.2: XOR-MAPPED-ADDRESS for 192.0.2.1:32853, after a
        // SOFTWARE attribute whose length needs padding.
        let mut attributes = vec![0x80, 0x22, 0x00, 0x0B];
        attributes.extend_from_slice(b"test vector ");
        attributes.extend_from_slice(&[
            0x00, 0x20, 0x00, 0x08, 0x00, 0x01, 0xA1, 0x47, 0xE1, 0x12, 0xA6, 0x43,
        ]);
        assert_eq!(
            mapped_address(&response(&attributes), &TRANSACTION),
            Some("192.0.2.1:32853".parse().unwrap())
        );

        // RFC 5769, 2.3: the IPv6 form, [2001:db8:1234:5678:11:2233:4455:6677]:32853.
        let v6 = [
            0x00, 0x20, 0x00, 0x14, 0x00, 0x02, 0xA1, 0x47, 0x01, 0x13, 0xA9, 0xFA, 0xA5, 0xD3,
            0xF1, 0x79, 0xBC, 0x25, 0xF4, 0xB5, 0xBE, 0xD2, 0xB9, 0xD9,
        ];
        assert_eq!(
            mapped_address(&response(&v6), &TRANSACTION),
            Some(
                "[2001:db8:1234:5678:11:2233:4455:6677]:32853"
                    .parse()
                    .unwrap()
            )
        );
    }

    #[test]
    fn the_old_plain_attribute_is_used_only_without_the_new_one() {
        let plain = [0x00, 0x01, 0x00, 0x08, 0x00, 0x01, 0x13, 0x88, 10, 0, 0, 7];
        assert_eq!(
            mapped_address(&response(&plain), &TRANSACTION),
            Some("10.0.0.7:5000".parse().unwrap())
        );
        let mut both = plain.to_vec();
        both.extend_from_slice(&[
            0x00, 0x20, 0x00, 0x08, 0x00, 0x01, 0xA1, 0x47, 0xE1, 0x12, 0xA6, 0x43,
        ]);
        assert_eq!(
            mapped_address(&response(&both), &TRANSACTION),
            Some("192.0.2.1:32853".parse().unwrap())
        );
    }

    #[test]
    fn foreign_and_damaged_answers_are_not_believed() {
        let good = response(&[
            0x00, 0x20, 0x00, 0x08, 0x00, 0x01, 0xA1, 0x47, 0xE1, 0x12, 0xA6, 0x43,
        ]);
        assert!(mapped_address(&good, &TRANSACTION).is_some());
        // The answer to somebody else's question.
        assert_eq!(mapped_address(&good, &[0; 12]), None);
        // An error response, a request, a cut message, no address at all.
        let mut error = good.clone();
        error[..2].copy_from_slice(&[0x01, 0x11]);
        assert_eq!(mapped_address(&error, &TRANSACTION), None);
        assert_eq!(
            mapped_address(&binding_request(&TRANSACTION), &TRANSACTION),
            None
        );
        assert_eq!(mapped_address(&good[..good.len() - 2], &TRANSACTION), None);
        assert_eq!(mapped_address(&response(&[]), &TRANSACTION), None);
        // An attribute that claims to be longer than the message.
        let lying = response(&[0x00, 0x20, 0x00, 0x40, 0x00, 0x01, 0xA1, 0x47]);
        assert_eq!(mapped_address(&lying, &TRANSACTION), None);
        assert_eq!(mapped_address(b"not stun", &TRANSACTION), None);
    }

    /// A STUN server of one answer on the loopback interface.
    fn tiny_server(answer_with: Option<SocketAddr>) -> SocketAddr {
        let server = UdpSocket::bind("127.0.0.1:0").unwrap();
        let address = server.local_addr().unwrap();
        std::thread::spawn(move || {
            let mut buffer = [0_u8; 64];
            server
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let Ok((length, source)) = server.recv_from(&mut buffer) else {
                return;
            };
            let Some(reported) = answer_with.or(Some(source)) else {
                return;
            };
            assert_eq!(length, 20);
            let SocketAddr::V4(reported) = reported else {
                return;
            };
            let mut message = BINDING_SUCCESS.to_vec();
            message.extend_from_slice(&[0, 12]);
            message.extend_from_slice(&buffer[4..20]);
            message.extend_from_slice(&[0x00, 0x20, 0x00, 0x08, 0x00, 0x01]);
            message.extend_from_slice(&(reported.port() ^ 0x2112).to_be_bytes());
            for (byte, mask) in reported.ip().octets().iter().zip(MAGIC) {
                message.push(byte ^ mask);
            }
            // Something that is not the answer comes first.
            server.send_to(b"noise", source).unwrap();
            server.send_to(&message, source).unwrap();
        });
        address
    }

    #[test]
    fn a_socket_learns_the_address_it_is_seen_under() {
        let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
        // Without a router in between that is the socket's own address.
        let seen = query(&socket, tiny_server(None)).unwrap();
        assert_eq!(seen, socket.local_addr().unwrap());
        // Whatever the server reports is what is returned.
        let public: SocketAddr = "203.0.113.9:40000".parse().unwrap();
        assert_eq!(query(&socket, tiny_server(Some(public))).unwrap(), public);
        assert_eq!(socket.read_timeout().unwrap(), None);
    }

    #[test]
    fn a_silent_server_is_given_up_within_a_second() {
        let silent = UdpSocket::bind("127.0.0.1:0").unwrap();
        let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
        let started = std::time::Instant::now();
        assert!(query(&socket, silent.local_addr().unwrap()).is_err());
        assert!(started.elapsed() < Duration::from_millis(1_500));
        assert_eq!(socket.read_timeout().unwrap(), None);
    }

    #[test]
    fn server_names_get_the_usual_port() {
        assert_eq!(
            resolve("127.0.0.1").unwrap(),
            "127.0.0.1:3478".parse().unwrap()
        );
        assert_eq!(
            resolve(" 127.0.0.1:19302 ").unwrap(),
            "127.0.0.1:19302".parse().unwrap()
        );
        assert_eq!(resolve("localhost").unwrap().port(), 3478);
        assert!(resolve("").is_err());
        assert!(resolve("   ").is_err());
        assert!(resolve(":3478").is_err());
        assert!(resolve("no such host.invalid").is_err());
    }
}
