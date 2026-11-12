use std::io;
use std::net::{IpAddr, SocketAddr, ToSocketAddrs, UdpSocket};
use std::time::Duration;

#[derive(Debug)]
pub struct UdpTransport {
    socket: UdpSocket,
}

impl UdpTransport {
    pub fn bind(local_address: SocketAddr) -> io::Result<Self> {
        let socket = UdpSocket::bind(local_address)?;

        Ok(Self { socket })
    }

    pub fn local_address(&self) -> io::Result<SocketAddr> {
        self.socket.local_addr()
    }

    pub fn send_to(&self, data: &[u8], destination: SocketAddr) -> io::Result<usize> {
        self.socket.send_to(data, destination)
    }

    pub fn receive(&self, timeout: Duration) -> io::Result<(Vec<u8>, SocketAddr)> {
        self.socket.set_read_timeout(Some(timeout))?;

        let mut buffer = [0_u8; 65_535];
        let (received, source) = self.socket.recv_from(&mut buffer)?;

        Ok((buffer[..received].to_vec(), source))
    }
}

pub fn resolve_socket_address(host: &str, port: u16) -> io::Result<SocketAddr> {
    (host, port).to_socket_addrs()?.next().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::AddrNotAvailable,
            format!("Could not resolve SIP server {host}:{port}"),
        )
    })
}

/// The local IP address the system would use to reach `remote`. Nothing is
/// sent: connecting a UDP socket only asks the routing table.
pub fn local_ip_towards(remote: SocketAddr) -> io::Result<IpAddr> {
    let any = if remote.is_ipv4() {
        "0.0.0.0:0"
    } else {
        "[::]:0"
    };
    let socket = UdpSocket::bind(any)?;
    socket.connect(remote)?;
    Ok(socket.local_addr()?.ip())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_is_reached_from_loopback() {
        let ip = local_ip_towards("127.0.0.1:5060".parse().unwrap()).unwrap();
        assert!(ip.is_loopback(), "{ip}");
    }

    #[test]
    fn a_bound_local_socket_is_reached_from_its_own_address() {
        let peer = UdpSocket::bind("127.0.0.1:0").unwrap();
        let ip = local_ip_towards(peer.local_addr().unwrap()).unwrap();
        assert_eq!(ip, peer.local_addr().unwrap().ip());
    }
}
