use super::digest::{DigestChallenge, DigestError, authorization_header};
use super::message::{Message, Request, Response, SipMessageError};
use crate::transport::{UdpTransport, resolve_socket_address};
use std::error::Error;
use std::fmt;
use std::io;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[derive(Debug)]
pub enum RegistrationError {
    Io(io::Error),
    Message(SipMessageError),
    Digest(DigestError),
    Timeout,
    InvalidResponse(String),
    AuthenticationRequired,
    AuthenticationRejected { status: u16, reason: String },
}

impl fmt::Display for RegistrationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => {
                write!(formatter, "UDP transport error: {error}")
            }
            Self::Message(error) => {
                write!(formatter, "SIP message error: {error}")
            }
            Self::Digest(error) => {
                write!(formatter, "Digest error: {error}")
            }
            Self::Timeout => formatter.write_str("SIP server response timeout"),
            Self::InvalidResponse(message) => {
                write!(formatter, "Invalid SIP response: {message}")
            }
            Self::AuthenticationRequired => formatter.write_str(
                "SIP server requires authentication but did not send a Digest challenge",
            ),
            Self::AuthenticationRejected { status, reason } => {
                write!(
                    formatter,
                    "SIP authentication was rejected: {status} {reason}"
                )
            }
        }
    }
}

impl Error for RegistrationError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Message(error) => Some(error),
            Self::Digest(error) => Some(error),
            _ => None,
        }
    }
}

impl From<io::Error> for RegistrationError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<SipMessageError> for RegistrationError {
    fn from(error: SipMessageError) -> Self {
        Self::Message(error)
    }
}

impl From<DigestError> for RegistrationError {
    fn from(error: DigestError) -> Self {
        Self::Digest(error)
    }
}

#[derive(Clone)]
pub struct SipAccount {
    pub server_host: String,
    pub server_port: u16,
    pub domain: String,
    pub username: String,
    pub password: String,
    pub contact_host: String,
    pub contact_port: u16,
    pub expires: u32,
    pub timeout: Duration,
}

impl SipAccount {
    pub fn new(
        server_host: impl Into<String>,
        server_port: u16,
        domain: impl Into<String>,
        username: impl Into<String>,
        password: impl Into<String>,
    ) -> Self {
        let server_host = server_host.into();

        Self {
            server_host: server_host.clone(),
            server_port,
            domain: domain.into(),
            username: username.into(),
            password: password.into(),
            contact_host: server_host,
            contact_port: 5060,
            expires: 3600,
            timeout: Duration::from_secs(10),
        }
    }

    pub fn with_contact(mut self, host: impl Into<String>, port: u16) -> Self {
        self.contact_host = host.into();
        self.contact_port = port;
        self
    }

    pub fn with_expires(mut self, seconds: u32) -> Self {
        self.expires = seconds;
        self
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }
}

impl fmt::Debug for SipAccount {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SipAccount")
            .field("server_host", &self.server_host)
            .field("server_port", &self.server_port)
            .field("domain", &self.domain)
            .field("username", &self.username)
            .field("password", &"********")
            .field("contact_host", &self.contact_host)
            .field("contact_port", &self.contact_port)
            .field("expires", &self.expires)
            .field("timeout", &self.timeout)
            .finish()
    }
}

#[derive(Debug, Clone)]
pub struct RegistrationResult {
    pub response: Response,
    pub call_id: String,
    pub expires: Option<u32>,
}

pub struct UdpRegistrationClient {
    transport: UdpTransport,
    server_address: SocketAddr,
}

impl UdpRegistrationClient {
    pub fn new(
        local_address: SocketAddr,
        server_host: &str,
        server_port: u16,
    ) -> Result<Self, RegistrationError> {
        let server_address = resolve_socket_address(server_host, server_port)?;

        let transport = UdpTransport::bind(local_address)?;

        Ok(Self {
            transport,
            server_address,
        })
    }

    pub fn server_address(&self) -> SocketAddr {
        self.server_address
    }

    pub fn register(&self, account: &SipAccount) -> Result<RegistrationResult, RegistrationError> {
        let identifiers = TransactionIdentifiers::new();

        let first_request = build_register(account, &identifiers, 1, None)?;

        let first_response = self.transact(&first_request, account.timeout)?;

        if (200..300).contains(&first_response.status_code) {
            return Ok(registration_result(first_response, &identifiers, account));
        }

        let (challenge_header_name, authorization_header_name) = match first_response.status_code {
            401 => ("www-authenticate", "authorization"),
            407 => ("proxy-authenticate", "proxy-authorization"),
            _ => {
                return Err(RegistrationError::InvalidResponse(format!(
                    "Unexpected REGISTER response: {} {}",
                    first_response.status_code, first_response.reason
                )));
            }
        };

        let challenge_value = first_response
            .headers
            .get(challenge_header_name)
            .ok_or(RegistrationError::AuthenticationRequired)?;

        let challenge = DigestChallenge::parse(challenge_value)?;

        let target = format!("sip:{}", account.domain);

        let authorization = authorization_header(
            &challenge,
            &account.username,
            &account.password,
            "REGISTER",
            &target,
        )?;

        let second_request = build_register(
            account,
            &identifiers,
            2,
            Some((authorization_header_name, authorization)),
        )?;

        let second_response = self.transact(&second_request, account.timeout)?;

        if (200..300).contains(&second_response.status_code) {
            return Ok(registration_result(second_response, &identifiers, account));
        }

        if second_response.status_code == 401
            || second_response.status_code == 403
            || second_response.status_code == 407
        {
            return Err(RegistrationError::AuthenticationRejected {
                status: second_response.status_code,
                reason: second_response.reason,
            });
        }

        Err(RegistrationError::InvalidResponse(format!(
            "Unexpected REGISTER response: {} {}",
            second_response.status_code, second_response.reason
        )))
    }

    fn transact(
        &self,
        request: &Request,
        timeout: Duration,
    ) -> Result<Response, RegistrationError> {
        let bytes = Message::Request(request.clone()).serialize()?;

        self.transport.send_to(&bytes, self.server_address)?;

        let deadline = Instant::now() + timeout;

        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());

            if remaining.is_zero() {
                return Err(RegistrationError::Timeout);
            }

            let (data, source) = match self.transport.receive(remaining) {
                Ok(value) => value,
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
                    ) =>
                {
                    return Err(RegistrationError::Timeout);
                }
                Err(error) => return Err(RegistrationError::Io(error)),
            };

            if source != self.server_address {
                continue;
            }

            let text = std::str::from_utf8(&data).map_err(|error| {
                RegistrationError::InvalidResponse(format!(
                    "SIP response is not valid UTF-8: {error}"
                ))
            })?;

            let message = Message::parse(text)?;

            let response = match message {
                Message::Response(response) => response,
                Message::Request(_) => {
                    return Err(RegistrationError::InvalidResponse(
                        "Received a SIP request while waiting for a response".to_owned(),
                    ));
                }
            };

            if !response_matches_request(request, &response) {
                continue;
            }

            if response.status_code < 200 {
                continue;
            }

            return Ok(response);
        }
    }
}

#[derive(Debug, Clone)]
struct TransactionIdentifiers {
    call_id: String,
    from_tag: String,
}

impl TransactionIdentifiers {
    fn new() -> Self {
        Self {
            call_id: format!("{}@microsip-rust", unique_hex()),
            from_tag: unique_hex(),
        }
    }

    fn branch(&self) -> String {
        format!("z9hG4bK{}", unique_hex())
    }
}

fn build_register(
    account: &SipAccount,
    identifiers: &TransactionIdentifiers,
    cseq: u32,
    authorization: Option<(&str, String)>,
) -> Result<Request, RegistrationError> {
    let target = format!("sip:{}", account.domain);

    let address_of_record = format!("sip:{}@{}", account.username, account.domain);

    let contact = format!(
        "sip:{}@{}",
        account.username,
        format_host_port(&account.contact_host, account.contact_port)
    );

    let via_sent_by = format_host_port(&account.contact_host, account.contact_port);

    let mut request = super::message::RegisterRequest::new(
        target,
        address_of_record,
        contact,
        via_sent_by,
        identifiers.call_id.clone(),
        identifiers.branch(),
        identifiers.from_tag.clone(),
    )
    .cseq(cseq)
    .expires(account.expires)
    .build()?;

    if let Some((name, value)) = authorization {
        request.headers.insert(name, value)?;
    }

    Ok(request)
}

fn registration_result(
    response: Response,
    identifiers: &TransactionIdentifiers,
    account: &SipAccount,
) -> RegistrationResult {
    let expires = response
        .headers
        .get("expires")
        .and_then(|value| value.parse::<u32>().ok())
        .or(Some(account.expires));

    RegistrationResult {
        response,
        call_id: identifiers.call_id.clone(),
        expires,
    }
}

fn response_matches_request(request: &Request, response: &Response) -> bool {
    let Some(expected_call_id) = request.headers.get("call-id") else {
        return false;
    };

    let Some(actual_call_id) = response.headers.get("call-id") else {
        return false;
    };

    if expected_call_id != actual_call_id {
        return false;
    }

    let Some(expected_cseq) = request.headers.get("cseq") else {
        return false;
    };

    let Some(actual_cseq) = response.headers.get("cseq") else {
        return false;
    };

    if !expected_cseq.eq_ignore_ascii_case(actual_cseq) {
        return false;
    }

    if let Some(expected_branch) = request.headers.get("via").and_then(via_branch) {
        let branch_matches = response.headers.get_all("via").iter().any(|via| {
            via_branch(via).is_some_and(|branch| branch.eq_ignore_ascii_case(expected_branch))
        });

        if !branch_matches {
            return false;
        }
    }

    true
}

fn via_branch(via: &str) -> Option<&str> {
    via.split(';').skip(1).find_map(|part| {
        let (name, value) = part.split_once('=')?;

        if name.trim().eq_ignore_ascii_case("branch") {
            Some(value.trim().trim_matches('"'))
        } else {
            None
        }
    })
}

fn format_host_port(host: &str, port: u16) -> String {
    if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

fn unique_hex() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);

    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();

    let counter = COUNTER.fetch_add(1, Ordering::Relaxed);

    format!("{timestamp:032x}{counter:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sip::message::ResponseBuilder;
    use std::net::UdpSocket;
    use std::thread;

    #[test]
    fn performs_register_digest_cycle_over_udp() {
        let server_socket = UdpSocket::bind("127.0.0.1:0").expect("server should bind");

        let server_address = server_socket
            .local_addr()
            .expect("server address should be available");

        server_socket
            .set_read_timeout(Some(Duration::from_secs(3)))
            .expect("timeout should be set");

        let server_thread = thread::spawn(move || {
            let mut buffer = [0_u8; 65_535];

            let (length, first_peer) = server_socket
                .recv_from(&mut buffer)
                .expect("first REGISTER should arrive");

            let first_text =
                std::str::from_utf8(&buffer[..length]).expect("first REGISTER should be UTF-8");

            let first_message = Message::parse(first_text).expect("first REGISTER should parse");

            let first_request = match first_message {
                Message::Request(request) => request,
                Message::Response(_) => {
                    panic!("first message should be a request")
                }
            };

            let via = first_request
                .headers
                .get("via")
                .expect("Via should exist")
                .to_owned();

            let from = first_request
                .headers
                .get("from")
                .expect("From should exist")
                .to_owned();

            let to = first_request
                .headers
                .get("to")
                .expect("To should exist")
                .to_owned();

            let call_id = first_request
                .headers
                .get("call-id")
                .expect("Call-ID should exist")
                .to_owned();

            let cseq = first_request
                .headers
                .get("cseq")
                .expect("CSeq should exist")
                .to_owned();

            let unauthorized = ResponseBuilder::new(401, "Unauthorized")
                .header("via", via)
                .expect("Via should be valid")
                .header("from", from)
                .expect("From should be valid")
                .header("to", to)
                .expect("To should be valid")
                .header("call-id", call_id)
                .expect("Call-ID should be valid")
                .header("cseq", cseq)
                .expect("CSeq should be valid")
                .header(
                    "www-authenticate",
                    "Digest realm=\"testrealm\", \
                     nonce=\"test-nonce-123\", \
                     qop=\"auth\", algorithm=MD5",
                )
                .expect("challenge should be valid")
                .build()
                .expect("401 response should build");

            server_socket
                .send_to(
                    &Message::Response(unauthorized)
                        .serialize()
                        .expect("401 should serialize"),
                    first_peer,
                )
                .expect("401 should be sent");

            let (length, second_peer) = server_socket
                .recv_from(&mut buffer)
                .expect("second REGISTER should arrive");

            let second_text =
                std::str::from_utf8(&buffer[..length]).expect("second REGISTER should be UTF-8");

            let second_message = Message::parse(second_text).expect("second REGISTER should parse");

            let second_request = match second_message {
                Message::Request(request) => request,
                Message::Response(_) => {
                    panic!("second message should be a request")
                }
            };

            let has_authorization = second_request.headers.get("authorization").is_some();

            let correct_cseq = second_request
                .headers
                .get("cseq")
                .is_some_and(|value| value == "2 REGISTER");

            let success = ResponseBuilder::new(200, "OK")
                .header(
                    "via",
                    second_request.headers.get("via").expect("Via should exist"),
                )
                .expect("Via should be valid")
                .header(
                    "from",
                    second_request
                        .headers
                        .get("from")
                        .expect("From should exist"),
                )
                .expect("From should be valid")
                .header(
                    "to",
                    second_request.headers.get("to").expect("To should exist"),
                )
                .expect("To should be valid")
                .header(
                    "call-id",
                    second_request
                        .headers
                        .get("call-id")
                        .expect("Call-ID should exist"),
                )
                .expect("Call-ID should be valid")
                .header(
                    "cseq",
                    second_request
                        .headers
                        .get("cseq")
                        .expect("CSeq should exist"),
                )
                .expect("CSeq should be valid")
                .header("expires", "3600")
                .expect("Expires should be valid")
                .build()
                .expect("200 response should build");

            server_socket
                .send_to(
                    &Message::Response(success)
                        .serialize()
                        .expect("200 should serialize"),
                    second_peer,
                )
                .expect("200 should be sent");

            has_authorization && correct_cseq
        });

        let client = UdpRegistrationClient::new(
            "127.0.0.1:0".parse().expect("address should be valid"),
            "127.0.0.1",
            server_address.port(),
        )
        .expect("client should start");

        let account = SipAccount::new(
            "127.0.0.1",
            server_address.port(),
            "example.com",
            "alice",
            "secret",
        )
        .with_contact("127.0.0.1", 5060)
        .with_timeout(Duration::from_secs(3));

        let result = client
            .register(&account)
            .expect("registration should succeed");

        assert_eq!(result.response.status_code, 200);
        assert_eq!(result.expires, Some(3600));

        assert!(server_thread.join().expect("server thread should finish"));
    }
}
