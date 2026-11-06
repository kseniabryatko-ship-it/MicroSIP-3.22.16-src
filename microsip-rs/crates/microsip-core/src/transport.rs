use std::io;
use std::net::{SocketAddr, ToSocketAddrs, UdpSocket};
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
