//! REGISTER client logic for one account, without any input or output.
//!
//! [`Registration`] decides what to send and what to tell the interface; the
//! executor in `call_runner` owns the socket, the retransmissions and the
//! clock. Every method that depends on time takes `now`, so the whole module
//! is tested without sleeping.
//!
//! Scope: UDP, one account, Digest MD5 as implemented by `sip::digest`. Each
//! refresh is sent without credentials and answers the fresh challenge, so a
//! nonce is never reused. No outbound proxy.
//!
//! NAT: every REGISTER asks for `rport` (RFC 3581). When the server's answer
//! shows that it sees this phone at another address, the registration is
//! repeated with that address as Contact, and [`Registration::keepalive_due`]
//! tells the executor when to send a keep-alive so the router keeps the
//! mapping open. No STUN: the address in SDP stays the local one.

use crate::sip::digest::{DigestChallenge, authorization_header};
use crate::sip::message::{Headers, RegisterRequest, Response};
use crate::transaction_headers::TransactionHeaders;
use std::fmt;
use std::net::SocketAddr;
use std::time::{Duration, Instant};

pub const DEFAULT_SIP_PORT: u16 = 5060;
pub const DEFAULT_EXPIRES: u32 = 300;

/// Pause before a failed registration is tried again.
pub const RETRY_DELAY: Duration = Duration::from_secs(30);

/// Pause between keep-alive packets while registered. Home routers drop an
/// idle UDP mapping after roughly 30 to 60 seconds.
pub const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(15);

/// The Contact follows the address the server reports at most this many
/// times per registration, so two disagreeing answers cannot loop forever.
const MAX_CONTACT_CHANGES: u8 = 3;

/// A challenge is answered at most this many times per attempt; the second
/// answer is only given to a `stale=true` challenge.
const MAX_CHALLENGES: u8 = 2;

/// Account settings as the user enters them.
#[derive(Clone, PartialEq, Eq)]
pub struct Account {
    /// `host`, `host:port`, `ip:port` or `[ipv6]:port`.
    pub server: String,
    /// SIP domain; empty means the host part of `server`.
    pub domain: String,
    pub username: String,
    /// Digest user name; empty means `username`.
    pub auth_username: String,
    pub password: String,
    /// Requested registration lifetime in seconds.
    pub expires: u32,
}

impl Account {
    pub fn new(
        server: impl Into<String>,
        username: impl Into<String>,
        password: impl Into<String>,
    ) -> Self {
        Self {
            server: server.into(),
            domain: String::new(),
            username: username.into(),
            auth_username: String::new(),
            password: password.into(),
            expires: DEFAULT_EXPIRES,
        }
    }

    pub fn with_domain(mut self, domain: impl Into<String>) -> Self {
        self.domain = domain.into();
        self
    }

    pub fn with_auth_username(mut self, auth_username: impl Into<String>) -> Self {
        self.auth_username = auth_username.into();
        self
    }

    pub fn with_expires(mut self, seconds: u32) -> Self {
        self.expires = seconds;
        self
    }

    /// Host and port of the registrar, for name resolution.
    pub fn server_host_port(&self) -> Result<(String, u16), String> {
        let server = self.server.trim();
        if server.is_empty() {
            return Err("the SIP server is empty".to_owned());
        }
        if let Ok(address) = server.parse::<SocketAddr>() {
            return Ok((address.ip().to_string(), address.port()));
        }
        if let Some(inner) = server
            .strip_prefix('[')
            .and_then(|rest| rest.strip_suffix(']'))
        {
            return Ok((inner.to_owned(), DEFAULT_SIP_PORT));
        }
        let (host, port) = match server.rsplit_once(':') {
            // More than one colon without brackets is a bare IPv6 address.
            Some((host, _)) if host.contains(':') => (server, DEFAULT_SIP_PORT),
            Some((host, port)) => {
                let port = port
                    .parse::<u16>()
                    .ok()
                    .filter(|port| *port != 0)
                    .ok_or_else(|| format!("invalid SIP server port: {port}"))?;
                (host, port)
            }
            None => (server, DEFAULT_SIP_PORT),
        };
        if host.is_empty() || host.contains(char::is_whitespace) || host.contains('@') {
            return Err(format!("invalid SIP server: {server}"));
        }
        Ok((host.to_owned(), port))
    }

    /// The SIP domain used in the address of record.
    pub fn effective_domain(&self) -> Result<String, String> {
        let domain = self.domain.trim();
        if domain.is_empty() {
            let (host, _) = self.server_host_port()?;
            return Ok(bracket_ipv6(&host));
        }
        if domain.contains(char::is_whitespace) || domain.contains('@') {
            return Err(format!("invalid SIP domain: {domain}"));
        }
        Ok(domain.to_owned())
    }

    pub fn effective_auth_username(&self) -> &str {
        let auth = self.auth_username.trim();
        if auth.is_empty() {
            self.username.trim()
        } else {
            auth
        }
    }

    fn validated_username(&self) -> Result<&str, String> {
        let username = self.username.trim();
        if username.is_empty() {
            return Err("the SIP user name is empty".to_owned());
        }
        if username.contains(|character: char| {
            character.is_whitespace() || matches!(character, '@' | ':' | '<' | '>' | ';')
        }) {
            return Err(format!("invalid SIP user name: {username}"));
        }
        Ok(username)
    }
}

impl fmt::Debug for Account {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Account")
            .field("server", &self.server)
            .field("domain", &self.domain)
            .field("username", &self.username)
            .field("auth_username", &self.auth_username)
            .field("password", &"********")
            .field("expires", &self.expires)
            .finish()
    }
}

fn bracket_ipv6(host: &str) -> String {
    if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]")
    } else {
        host.to_owned()
    }
}

/// What the interface shows about the account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegistrationState {
    Registering,
    /// The server accepted the registration for this many seconds.
    Registered {
        expires: u32,
    },
    Unregistered,
    Failed(String),
}

impl fmt::Display for RegistrationState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Registering => formatter.write_str("registering"),
            Self::Registered { expires } => write!(formatter, "registered ({expires} s)"),
            Self::Unregistered => formatter.write_str("not registered"),
            Self::Failed(reason) => write!(formatter, "registration failed: {reason}"),
        }
    }
}

/// What the executor must do, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Output {
    /// Send this REGISTER to the server and retransmit it until a final
    /// response or [`Registration::on_timeout`]. Replaces any earlier one.
    Send(Vec<u8>),
    State(RegistrationState),
}

struct Transaction {
    headers: TransactionHeaders,
    removing: bool,
    challenges: u8,
    interval_adjusted: bool,
}

pub struct Registration {
    account: Account,
    username: String,
    domain: String,
    server: SocketAddr,
    local: SocketAddr,
    /// Address the server reaches this phone at; `local` unless behind NAT.
    contact: SocketAddr,
    contact_changes: u8,
    next_keepalive: Option<Instant>,
    call_id: String,
    from_tag: String,
    seed: String,
    counter: u64,
    cseq: u32,
    expires: u32,
    transaction: Option<Transaction>,
    /// The server holds (or may hold) a binding for our contact.
    bound: bool,
    next_attempt: Option<Instant>,
    reported: Option<RegistrationState>,
    finished: bool,
}

impl Registration {
    /// `server` is the resolved registrar address, `local` the SIP socket
    /// address used as Contact and `seed` a string unique to this process.
    pub fn new(
        account: Account,
        server: SocketAddr,
        local: SocketAddr,
        seed: &str,
    ) -> Result<Self, String> {
        let username = account.validated_username()?.to_owned();
        let domain = account.effective_domain()?;
        if account.expires == 0 {
            return Err("the registration lifetime must not be zero".to_owned());
        }
        if server.is_ipv4() != local.is_ipv4() {
            return Err(
                "the SIP server and the local address use different IP families".to_owned(),
            );
        }
        Ok(Self {
            expires: account.expires,
            account,
            username,
            domain,
            server,
            local,
            contact: local,
            contact_changes: 0,
            next_keepalive: None,
            call_id: format!("reg-{seed}@{}", local.ip()),
            from_tag: format!("reg-{seed}"),
            seed: seed.to_owned(),
            counter: 0,
            cseq: 0,
            transaction: None,
            bound: false,
            next_attempt: None,
            reported: None,
            finished: false,
        })
    }

    pub fn server(&self) -> SocketAddr {
        self.server
    }

    pub fn call_id(&self) -> &str {
        &self.call_id
    }

    pub fn username(&self) -> &str {
        &self.username
    }

    /// Address to put into Contact headers: the local socket address, or the
    /// public one the server reported when this phone is behind NAT.
    pub fn contact_address(&self) -> SocketAddr {
        self.contact
    }

    /// True when a keep-alive packet should be sent to the server now. The
    /// next one is then due after [`KEEPALIVE_INTERVAL`].
    pub fn keepalive_due(&mut self, now: Instant) -> bool {
        if !self.bound || self.finished {
            self.next_keepalive = None;
            return false;
        }
        match self.next_keepalive {
            Some(due) if now < due => false,
            Some(_) => {
                self.next_keepalive = Some(now + KEEPALIVE_INTERVAL);
                true
            }
            None => {
                self.next_keepalive = Some(now + KEEPALIVE_INTERVAL);
                false
            }
        }
    }

    /// The SIP domain of the address of record.
    pub fn domain(&self) -> &str {
        &self.domain
    }

    /// Answers a 401/407 challenge for another request of this account, for
    /// example INVITE. Returns the header name and value to add.
    pub fn authorize(
        &self,
        response: &Response,
        method: &str,
        uri: &str,
    ) -> Result<(&'static str, String), String> {
        let (challenge_name, answer_name) = challenge_headers(response.status_code);
        let value = response
            .headers
            .get(challenge_name)
            .ok_or("the SIP server asked for a password without a Digest challenge")?;
        let challenge = DigestChallenge::parse(value).map_err(|error| error.to_string())?;
        let answer = authorization_header(
            &challenge,
            self.account.effective_auth_username(),
            &self.account.password,
            method,
            uri,
        )
        .map_err(|error| error.to_string())?;
        Ok((answer_name, answer))
    }

    /// True while the server may hold a binding that should be removed.
    pub fn is_bound(&self) -> bool {
        self.bound
    }

    /// True once the registration was removed; the object is then useless.
    pub fn is_finished(&self) -> bool {
        self.finished
    }

    /// Sends the first REGISTER.
    pub fn start(&mut self) -> Vec<Output> {
        let mut outputs = Vec::new();
        self.begin(false, &mut outputs);
        outputs
    }

    /// Removes the registration (REGISTER with `Expires: 0`).
    pub fn unregister(&mut self) -> Vec<Output> {
        let mut outputs = Vec::new();
        self.next_attempt = None;
        if self.finished {
            return outputs;
        }
        let registering = self
            .transaction
            .as_ref()
            .is_some_and(|transaction| !transaction.removing);
        if self.bound || registering {
            if !self
                .transaction
                .as_ref()
                .is_some_and(|transaction| transaction.removing)
            {
                self.begin(true, &mut outputs);
            }
        } else {
            self.finish(&mut outputs);
        }
        outputs
    }

    /// Starts a refresh or a retry when it is due.
    pub fn poll(&mut self, now: Instant) -> Vec<Output> {
        let mut outputs = Vec::new();
        if self.transaction.is_none()
            && !self.finished
            && self.next_attempt.is_some_and(|due| now >= due)
        {
            self.next_attempt = None;
            self.begin(false, &mut outputs);
        }
        outputs
    }

    /// The executor gave up retransmitting the current REGISTER.
    pub fn on_timeout(&mut self, now: Instant) -> Vec<Output> {
        let mut outputs = Vec::new();
        let Some(transaction) = self.transaction.take() else {
            return outputs;
        };
        if transaction.removing {
            self.finish(&mut outputs);
        } else {
            self.fail(
                "no response from the SIP server".to_owned(),
                Some(now + RETRY_DELAY),
                &mut outputs,
            );
        }
        outputs
    }

    /// Handles a response to REGISTER. Returns `None` when it does not belong
    /// to the current request, so the executor keeps retransmitting.
    pub fn on_response(
        &mut self,
        response: &Response,
        headers: &TransactionHeaders,
        now: Instant,
    ) -> Option<Vec<Output>> {
        let matches = self
            .transaction
            .as_ref()
            .is_some_and(|transaction| transaction.headers.matches_response(headers));
        if !matches {
            return None;
        }
        let mut outputs = Vec::new();
        let status = response.status_code;
        if status < 200 {
            return Some(outputs);
        }
        let transaction = self.transaction.take()?;

        if transaction.removing {
            // Removal is best effort, except that a challenge is answered.
            if matches!(status, 401 | 407) && transaction.challenges == 0 {
                match self.authorization(response) {
                    Ok(authorization) => {
                        self.send(true, Some(authorization), 1, false, &mut outputs);
                    }
                    Err(_) => self.finish(&mut outputs),
                }
            } else {
                self.finish(&mut outputs);
            }
            return Some(outputs);
        }

        match status {
            200..=299 => {
                let seen = reported_address(&response.headers, self.local);
                if let Some(seen) = seen.filter(|seen| *seen != self.contact)
                    && self.contact_changes < MAX_CONTACT_CHANGES
                {
                    // Behind NAT: register the address the server really sees.
                    // The binding made a moment ago simply expires.
                    self.contact_changes += 1;
                    self.contact = seen;
                    self.bound = true;
                    self.send(false, None, 0, transaction.interval_adjusted, &mut outputs);
                    return Some(outputs);
                }
                let granted =
                    granted_expires(&response.headers, &self.contact_uri()).unwrap_or(self.expires);
                if granted == 0 {
                    self.fail(
                        "the SIP server did not keep the registration".to_owned(),
                        Some(now + RETRY_DELAY),
                        &mut outputs,
                    );
                } else {
                    self.bound = true;
                    self.next_attempt = Some(now + refresh_delay(granted));
                    self.set_state(
                        RegistrationState::Registered { expires: granted },
                        &mut outputs,
                    );
                }
            }
            401 | 407 => {
                let stale = challenge_is_stale(response, status);
                let allowed = transaction.challenges == 0
                    || (transaction.challenges < MAX_CHALLENGES && stale);
                if !allowed {
                    self.fail(
                        format!("wrong user name or password ({status})"),
                        None,
                        &mut outputs,
                    );
                } else {
                    match self.authorization(response) {
                        Ok(authorization) => self.send(
                            false,
                            Some(authorization),
                            transaction.challenges + 1,
                            transaction.interval_adjusted,
                            &mut outputs,
                        ),
                        Err(reason) => self.fail(reason, None, &mut outputs),
                    }
                }
            }
            423 => {
                let minimum = response
                    .headers
                    .get("min-expires")
                    .and_then(|value| value.trim().parse::<u32>().ok())
                    .filter(|minimum| *minimum > self.expires);
                match minimum {
                    Some(minimum) if !transaction.interval_adjusted => {
                        self.expires = minimum;
                        self.send(false, None, 0, true, &mut outputs);
                    }
                    _ => self.fail(
                        "423 Interval Too Brief without a usable Min-Expires".to_owned(),
                        None,
                        &mut outputs,
                    ),
                }
            }
            _ => {
                let reason = format!("{status} {}", response.reason);
                // A refusal of the account itself will not heal by retrying.
                let retry = if matches!(status, 403 | 404) {
                    None
                } else {
                    Some(now + RETRY_DELAY)
                };
                self.fail(reason, retry, &mut outputs);
            }
        }
        Some(outputs)
    }

    // ----- internals -------------------------------------------------------

    fn begin(&mut self, removing: bool, outputs: &mut Vec<Output>) {
        if !removing && !self.bound {
            self.set_state(RegistrationState::Registering, outputs);
        }
        self.send(removing, None, 0, false, outputs);
    }

    fn send(
        &mut self,
        removing: bool,
        authorization: Option<(&'static str, String)>,
        challenges: u8,
        interval_adjusted: bool,
        outputs: &mut Vec<Output>,
    ) {
        match self.build(removing, authorization) {
            Ok((bytes, headers)) => {
                self.transaction = Some(Transaction {
                    headers,
                    removing,
                    challenges,
                    interval_adjusted,
                });
                outputs.push(Output::Send(bytes));
            }
            Err(reason) => {
                self.transaction = None;
                if removing {
                    self.finish(outputs);
                } else {
                    self.fail(reason, None, outputs);
                }
            }
        }
    }

    fn build(
        &mut self,
        removing: bool,
        authorization: Option<(&'static str, String)>,
    ) -> Result<(Vec<u8>, TransactionHeaders), String> {
        self.cseq += 1;
        self.counter += 1;
        let branch = format!("z9hG4bK-reg-{}-{:x}", self.seed, self.counter);
        let expires = if removing { 0 } else { self.expires };
        let mut request = RegisterRequest::new(
            self.target(),
            format!("sip:{}@{}", self.username, self.domain),
            self.contact_uri(),
            self.local.to_string(),
            self.call_id.clone(),
            branch,
            self.from_tag.clone(),
        )
        .cseq(self.cseq)
        .expires(expires)
        .build()
        .map_err(|error| error.to_string())?;
        if let Some((name, value)) = authorization {
            request
                .headers
                .insert(name, value)
                .map_err(|error| error.to_string())?;
        }
        add_rport(&mut request.headers).map_err(|error| error.to_string())?;
        let bytes = request.serialize().map_err(|error| error.to_string())?;
        let headers =
            TransactionHeaders::from_message(&crate::sip::message::Message::Request(request))
                .map_err(|error| error.to_string())?;
        Ok((bytes, headers))
    }

    fn target(&self) -> String {
        format!("sip:{}", self.domain)
    }

    fn contact_uri(&self) -> String {
        format!("sip:{}@{}", self.username, self.contact)
    }

    fn authorization(&self, response: &Response) -> Result<(&'static str, String), String> {
        self.authorize(response, "REGISTER", &self.target())
    }

    fn fail(&mut self, reason: String, retry: Option<Instant>, outputs: &mut Vec<Output>) {
        self.bound = false;
        self.next_attempt = retry;
        self.set_state(RegistrationState::Failed(reason), outputs);
    }

    fn finish(&mut self, outputs: &mut Vec<Output>) {
        self.transaction = None;
        self.bound = false;
        self.next_attempt = None;
        self.finished = true;
        self.set_state(RegistrationState::Unregistered, outputs);
    }

    fn set_state(&mut self, state: RegistrationState, outputs: &mut Vec<Output>) {
        if self.reported.as_ref() != Some(&state) {
            self.reported = Some(state.clone());
            outputs.push(Output::State(state));
        }
    }
}

/// Asks the server to answer to the source port of the request and to report
/// it (RFC 3581). A Via that already asks is left as it is.
pub fn add_rport(headers: &mut Headers) -> Result<(), crate::sip::message::SipMessageError> {
    let Some(via) = headers.get("via").map(str::to_owned) else {
        return Ok(());
    };
    let asked = via.split(';').skip(1).any(|parameter| {
        let name = parameter.split('=').next().unwrap_or_default();
        name.trim().eq_ignore_ascii_case("rport")
    });
    if asked {
        return Ok(());
    }
    headers.insert("via", format!("{via};rport"))
}

/// The address the server saw the request come from, taken from the
/// `received` and `rport` parameters of the top Via of its response.
fn reported_address(headers: &Headers, local: SocketAddr) -> Option<SocketAddr> {
    let via = headers.get("via")?;
    let parameter = |name: &str| {
        via.split(';')
            .skip(1)
            .filter_map(|part| part.split_once('='))
            .find(|(key, _)| key.trim().eq_ignore_ascii_case(name))
            .map(|(_, value)| value.trim().trim_matches(|c| c == '[' || c == ']'))
    };
    let port = parameter("rport")?
        .parse::<u16>()
        .ok()
        .filter(|port| *port != 0)?;
    // A server on the same network may leave `received` out.
    let ip = match parameter("received") {
        Some(received) => received.parse().ok()?,
        None => local.ip(),
    };
    let address = SocketAddr::new(ip, port);
    (address.is_ipv4() == local.is_ipv4() && !ip.is_unspecified()).then_some(address)
}

fn challenge_headers(status: u16) -> (&'static str, &'static str) {
    if status == 407 {
        ("proxy-authenticate", "proxy-authorization")
    } else {
        ("www-authenticate", "authorization")
    }
}

fn challenge_is_stale(response: &Response, status: u16) -> bool {
    response
        .headers
        .get(challenge_headers(status).0)
        .and_then(|value| DigestChallenge::parse(value).ok())
        .and_then(|challenge| challenge.stale)
        .is_some_and(|stale| stale.eq_ignore_ascii_case("true"))
}

/// Refresh well before the registration expires: half-way for short
/// lifetimes, 30 seconds early for long ones.
fn refresh_delay(expires: u32) -> Duration {
    let lifetime = Duration::from_secs(u64::from(expires));
    lifetime - (lifetime / 2).min(Duration::from_secs(30))
}

/// Lifetime the server granted: the `expires` parameter of our Contact, or
/// else the Expires header.
fn granted_expires(headers: &Headers, contact_uri: &str) -> Option<u32> {
    for value in headers.get_all("contact") {
        for entry in value.split(',') {
            if !entry.contains(contact_uri) {
                continue;
            }
            let parameters = entry.rsplit_once('>').map_or(entry, |(_, rest)| rest);
            let expires = parameters
                .split(';')
                .filter_map(|part| part.split_once('='))
                .find(|(name, _)| name.trim().eq_ignore_ascii_case("expires"))
                .and_then(|(_, value)| value.trim().trim_matches('"').parse::<u32>().ok());
            if expires.is_some() {
                return expires;
            }
        }
    }
    headers
        .get("expires")
        .and_then(|value| value.trim().parse::<u32>().ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sip::message::{Message, Request, ResponseBuilder};

    const CHALLENGE: &str = "Digest realm=\"pbx\", nonce=\"abc123\", qop=\"auth\", algorithm=MD5";

    fn registration() -> Registration {
        Registration::new(
            Account::new("192.0.2.10", "alice", "secret").with_expires(120),
            "192.0.2.10:5060".parse().unwrap(),
            "192.0.2.20:5070".parse().unwrap(),
            "test",
        )
        .unwrap()
    }

    fn sent(outputs: &[Output]) -> Request {
        let bytes = outputs
            .iter()
            .find_map(|output| match output {
                Output::Send(bytes) => Some(bytes.clone()),
                Output::State(_) => None,
            })
            .expect("a REGISTER should be sent");
        match Message::parse(std::str::from_utf8(&bytes).unwrap()).unwrap() {
            Message::Request(request) => request,
            Message::Response(_) => panic!("expected a request"),
        }
    }

    fn states(outputs: &[Output]) -> Vec<RegistrationState> {
        outputs
            .iter()
            .filter_map(|output| match output {
                Output::State(state) => Some(state.clone()),
                Output::Send(_) => None,
            })
            .collect()
    }

    fn has_send(outputs: &[Output]) -> bool {
        outputs
            .iter()
            .any(|output| matches!(output, Output::Send(_)))
    }

    fn reply(request: &Request, status: u16, extra: &[(&str, &str)]) -> Response {
        let mut builder = ResponseBuilder::new(status, "Test");
        for name in ["via", "from", "to", "call-id", "cseq"] {
            builder = builder
                .header(name, request.headers.get(name).unwrap())
                .unwrap();
        }
        for (name, value) in extra {
            builder = builder.header(name, *value).unwrap();
        }
        builder.build().unwrap()
    }

    fn feed(
        registration: &mut Registration,
        response: Response,
        now: Instant,
    ) -> Option<Vec<Output>> {
        let headers =
            TransactionHeaders::from_message(&Message::Response(response.clone())).unwrap();
        registration.on_response(&response, &headers, now)
    }

    fn register(registration: &mut Registration, now: Instant) -> Request {
        let first = sent(&registration.start());
        let outputs = feed(registration, reply(&first, 200, &[("expires", "120")]), now).unwrap();
        assert_eq!(
            states(&outputs),
            [RegistrationState::Registered { expires: 120 }]
        );
        first
    }

    #[test]
    fn first_register_names_the_account_and_the_local_contact() {
        let mut registration = registration();
        let outputs = registration.start();
        assert_eq!(states(&outputs), [RegistrationState::Registering]);

        let request = sent(&outputs);
        assert_eq!(request.method.as_str(), "REGISTER");
        assert_eq!(request.target, "sip:192.0.2.10");
        assert_eq!(request.headers.get("cseq"), Some("1 REGISTER"));
        assert_eq!(request.headers.get("expires"), Some("120"));
        assert_eq!(
            request.headers.get("contact"),
            Some("<sip:alice@192.0.2.20:5070>")
        );
        assert_eq!(request.headers.get("to"), Some("<sip:alice@192.0.2.10>"));
        assert!(request.headers.get("authorization").is_none());
    }

    #[test]
    fn success_without_a_challenge_registers() {
        let mut registration = registration();
        register(&mut registration, Instant::now());
        assert!(registration.is_bound());
    }

    #[test]
    fn challenge_is_answered_in_the_same_call_with_the_next_cseq() {
        let mut registration = registration();
        let now = Instant::now();
        let first = sent(&registration.start());

        let outputs = feed(
            &mut registration,
            reply(&first, 401, &[("www-authenticate", CHALLENGE)]),
            now,
        )
        .unwrap();
        assert!(states(&outputs).is_empty());
        let second = sent(&outputs);
        assert_eq!(second.headers.get("cseq"), Some("2 REGISTER"));
        assert_eq!(second.headers.get("call-id"), first.headers.get("call-id"));
        assert_ne!(second.headers.get("via"), first.headers.get("via"));
        let authorization = second.headers.get("authorization").unwrap();
        assert!(authorization.contains("username=\"alice\""));
        assert!(authorization.contains("uri=\"sip:192.0.2.10\""));
        assert!(authorization.contains("nonce=\"abc123\""));

        let outputs = feed(&mut registration, reply(&second, 200, &[]), now).unwrap();
        assert_eq!(
            states(&outputs),
            [RegistrationState::Registered { expires: 120 }]
        );
    }

    #[test]
    fn proxy_challenge_is_answered_with_proxy_authorization() {
        let mut registration = registration();
        let first = sent(&registration.start());
        let outputs = feed(
            &mut registration,
            reply(&first, 407, &[("proxy-authenticate", CHALLENGE)]),
            Instant::now(),
        )
        .unwrap();
        let second = sent(&outputs);
        assert!(second.headers.get("proxy-authorization").is_some());
        assert!(second.headers.get("authorization").is_none());
    }

    #[test]
    fn auth_username_replaces_the_user_name_in_the_digest_only() {
        let mut registration = Registration::new(
            Account::new("192.0.2.10", "100", "secret").with_auth_username("alice-auth"),
            "192.0.2.10:5060".parse().unwrap(),
            "192.0.2.20:5070".parse().unwrap(),
            "test",
        )
        .unwrap();
        let first = sent(&registration.start());
        assert_eq!(first.headers.get("to"), Some("<sip:100@192.0.2.10>"));
        let outputs = feed(
            &mut registration,
            reply(&first, 401, &[("www-authenticate", CHALLENGE)]),
            Instant::now(),
        )
        .unwrap();
        let authorization = sent(&outputs)
            .headers
            .get("authorization")
            .unwrap()
            .to_owned();
        assert!(authorization.contains("username=\"alice-auth\""));
    }

    #[test]
    fn second_challenge_means_a_wrong_password_and_is_not_retried() {
        let mut registration = registration();
        let now = Instant::now();
        let first = sent(&registration.start());
        let outputs = feed(
            &mut registration,
            reply(&first, 401, &[("www-authenticate", CHALLENGE)]),
            now,
        )
        .unwrap();
        let second = sent(&outputs);

        let outputs = feed(
            &mut registration,
            reply(&second, 401, &[("www-authenticate", CHALLENGE)]),
            now,
        )
        .unwrap();
        assert!(!has_send(&outputs));
        assert_eq!(
            states(&outputs),
            [RegistrationState::Failed(
                "wrong user name or password (401)".to_owned()
            )]
        );
        assert!(
            registration
                .poll(now + Duration::from_secs(3600))
                .is_empty()
        );
    }

    #[test]
    fn stale_challenge_is_answered_once_more() {
        let mut registration = registration();
        let now = Instant::now();
        let stale = "Digest realm=\"pbx\", nonce=\"fresh\", stale=true, qop=\"auth\"";
        let first = sent(&registration.start());
        let second = sent(
            &feed(
                &mut registration,
                reply(&first, 401, &[("www-authenticate", CHALLENGE)]),
                now,
            )
            .unwrap(),
        );
        let outputs = feed(
            &mut registration,
            reply(&second, 401, &[("www-authenticate", stale)]),
            now,
        )
        .unwrap();
        let third = sent(&outputs);
        assert_eq!(third.headers.get("cseq"), Some("3 REGISTER"));
        assert!(
            third
                .headers
                .get("authorization")
                .unwrap()
                .contains("nonce=\"fresh\"")
        );

        // A third challenge ends the attempt even if it claims to be stale.
        let outputs = feed(
            &mut registration,
            reply(&third, 401, &[("www-authenticate", stale)]),
            now,
        )
        .unwrap();
        assert!(!has_send(&outputs));
        assert!(matches!(
            states(&outputs).as_slice(),
            [RegistrationState::Failed(_)]
        ));
    }

    #[test]
    fn challenge_without_digest_fails() {
        let mut registration = registration();
        let first = sent(&registration.start());
        let outputs = feed(&mut registration, reply(&first, 401, &[]), Instant::now()).unwrap();
        assert!(!has_send(&outputs));
        assert!(matches!(
            states(&outputs).as_slice(),
            [RegistrationState::Failed(_)]
        ));
    }

    #[test]
    fn interval_too_brief_is_retried_once_with_min_expires() {
        let mut registration = registration();
        let now = Instant::now();
        let first = sent(&registration.start());
        let outputs = feed(
            &mut registration,
            reply(&first, 423, &[("min-expires", "600")]),
            now,
        )
        .unwrap();
        let second = sent(&outputs);
        assert_eq!(second.headers.get("expires"), Some("600"));

        let outputs = feed(
            &mut registration,
            reply(&second, 423, &[("min-expires", "900")]),
            now,
        )
        .unwrap();
        assert!(!has_send(&outputs));
        assert!(matches!(
            states(&outputs).as_slice(),
            [RegistrationState::Failed(_)]
        ));
    }

    #[test]
    fn granted_lifetime_comes_from_our_contact_then_from_expires() {
        let mut registration = registration();
        let first = sent(&registration.start());
        let outputs = feed(
            &mut registration,
            reply(
                &first,
                200,
                &[
                    (
                        "contact",
                        "<sip:bob@192.0.2.99>;expires=999, <sip:alice@192.0.2.20:5070>;expires=45",
                    ),
                    ("expires", "120"),
                ],
            ),
            Instant::now(),
        )
        .unwrap();
        assert_eq!(
            states(&outputs),
            [RegistrationState::Registered { expires: 45 }]
        );
    }

    #[test]
    fn refresh_is_sent_before_expiry_without_a_new_state() {
        let mut registration = registration();
        let now = Instant::now();
        let first = register(&mut registration, now);

        // 120 s granted: refresh 30 s early.
        assert!(registration.poll(now + Duration::from_secs(89)).is_empty());
        let outputs = registration.poll(now + Duration::from_secs(90));
        assert!(states(&outputs).is_empty());
        let refresh = sent(&outputs);
        assert_eq!(refresh.headers.get("cseq"), Some("2 REGISTER"));
        assert_eq!(refresh.headers.get("call-id"), first.headers.get("call-id"));
        assert_eq!(refresh.headers.get("expires"), Some("120"));

        // The same lifetime again is not reported twice.
        let later = now + Duration::from_secs(90);
        let outputs = feed(
            &mut registration,
            reply(&refresh, 200, &[("expires", "120")]),
            later,
        )
        .unwrap();
        assert!(outputs.is_empty());
        assert!(
            registration
                .poll(later + Duration::from_secs(89))
                .is_empty()
        );
        assert!(has_send(
            &registration.poll(later + Duration::from_secs(90))
        ));
    }

    #[test]
    fn short_lifetime_is_refreshed_half_way() {
        assert_eq!(refresh_delay(1), Duration::from_millis(500));
        assert_eq!(refresh_delay(40), Duration::from_secs(20));
        assert_eq!(refresh_delay(3600), Duration::from_secs(3570));
    }

    #[test]
    fn timeout_fails_and_retries_after_the_delay() {
        let mut registration = registration();
        let now = Instant::now();
        registration.start();

        let outputs = registration.on_timeout(now);
        assert_eq!(
            states(&outputs),
            [RegistrationState::Failed(
                "no response from the SIP server".to_owned()
            )]
        );
        assert!(
            registration
                .poll(now + RETRY_DELAY - Duration::from_millis(1))
                .is_empty()
        );
        let outputs = registration.poll(now + RETRY_DELAY);
        assert_eq!(states(&outputs), [RegistrationState::Registering]);
        assert_eq!(sent(&outputs).headers.get("cseq"), Some("2 REGISTER"));
    }

    #[test]
    fn server_error_is_retried_but_forbidden_is_not() {
        let now = Instant::now();

        let mut registration = registration();
        let first = sent(&registration.start());
        let outputs = feed(&mut registration, reply(&first, 503, &[]), now).unwrap();
        assert_eq!(
            states(&outputs),
            [RegistrationState::Failed("503 Test".to_owned())]
        );
        assert!(has_send(&registration.poll(now + RETRY_DELAY)));

        let mut registration = self::registration();
        let first = sent(&registration.start());
        feed(&mut registration, reply(&first, 403, &[]), now).unwrap();
        assert!(registration.poll(now + RETRY_DELAY * 10).is_empty());
    }

    #[test]
    fn unrelated_and_provisional_responses_do_not_end_the_request() {
        let mut registration = registration();
        let now = Instant::now();
        let first = sent(&registration.start());

        let mut other = first.clone();
        other.headers.insert("call-id", "someone-else").unwrap();
        assert!(feed(&mut registration, reply(&other, 200, &[]), now).is_none());

        let outputs = feed(&mut registration, reply(&first, 100, &[]), now).unwrap();
        assert!(outputs.is_empty());

        let outputs = feed(&mut registration, reply(&first, 200, &[]), now).unwrap();
        assert_eq!(
            states(&outputs),
            [RegistrationState::Registered { expires: 120 }]
        );
        // The answered request is gone: a repeated 200 no longer matches.
        assert!(feed(&mut registration, reply(&first, 200, &[]), now).is_none());
    }

    #[test]
    fn unregister_sends_expires_zero_and_answers_a_challenge() {
        let mut registration = registration();
        let now = Instant::now();
        register(&mut registration, now);

        let outputs = registration.unregister();
        assert!(states(&outputs).is_empty());
        let removal = sent(&outputs);
        assert_eq!(removal.headers.get("expires"), Some("0"));
        assert_eq!(removal.headers.get("cseq"), Some("2 REGISTER"));

        let outputs = feed(
            &mut registration,
            reply(&removal, 401, &[("www-authenticate", CHALLENGE)]),
            now,
        )
        .unwrap();
        let authorized = sent(&outputs);
        assert_eq!(authorized.headers.get("expires"), Some("0"));
        assert!(authorized.headers.get("authorization").is_some());

        let outputs = feed(&mut registration, reply(&authorized, 200, &[]), now).unwrap();
        assert_eq!(states(&outputs), [RegistrationState::Unregistered]);
        assert!(registration.is_finished());
        assert!(
            registration
                .poll(now + Duration::from_secs(3600))
                .is_empty()
        );
    }

    #[test]
    fn unregister_without_a_binding_finishes_at_once() {
        let mut registration = registration();
        let now = Instant::now();
        registration.start();
        registration.on_timeout(now);

        let outputs = registration.unregister();
        assert!(!has_send(&outputs));
        assert_eq!(states(&outputs), [RegistrationState::Unregistered]);
        assert!(registration.is_finished());
    }

    #[test]
    fn unregister_that_gets_no_answer_still_finishes() {
        let mut registration = registration();
        let now = Instant::now();
        register(&mut registration, now);
        registration.unregister();

        let outputs = registration.on_timeout(now);
        assert_eq!(states(&outputs), [RegistrationState::Unregistered]);
        assert!(registration.is_finished());
    }

    /// A response whose top Via carries what a server behind our NAT saw.
    fn reply_seen_at(request: &Request, seen: &str, extra: &[(&str, &str)]) -> Response {
        let (ip, port) = seen.rsplit_once(':').unwrap();
        let mut response = reply(request, 200, extra);
        let via = request
            .headers
            .get("via")
            .unwrap()
            .replace(";rport", &format!(";rport={port};received={ip}"));
        response.headers.insert("via", via).unwrap();
        response
    }

    #[test]
    fn rport_is_added_once() {
        let mut registration = registration();
        let mut request = sent(&registration.start());
        let before = request.headers.get("via").unwrap().to_owned();
        add_rport(&mut request.headers).unwrap();
        assert_eq!(request.headers.get("via"), Some(before.as_str()));
        assert_eq!(before.matches("rport").count(), 1);
    }

    #[test]
    fn register_asks_for_rport() {
        let mut registration = registration();
        let request = sent(&registration.start());
        assert!(request.headers.get("via").unwrap().ends_with(";rport"));
    }

    #[test]
    fn address_seen_by_the_server_becomes_the_contact() {
        let mut registration = registration();
        let now = Instant::now();
        let first = sent(&registration.start());
        assert_eq!(
            registration.contact_address(),
            "192.0.2.20:5070".parse().unwrap()
        );

        // The server saw another address: register that one, report nothing yet.
        let outputs = feed(
            &mut registration,
            reply_seen_at(&first, "203.0.113.7:40000", &[("expires", "120")]),
            now,
        )
        .unwrap();
        assert!(states(&outputs).is_empty());
        let second = sent(&outputs);
        assert_eq!(second.headers.get("cseq"), Some("2 REGISTER"));
        assert_eq!(second.headers.get("call-id"), first.headers.get("call-id"));
        assert_eq!(
            second.headers.get("contact"),
            Some("<sip:alice@203.0.113.7:40000>")
        );
        // The Via still names the real local socket.
        assert!(
            second
                .headers
                .get("via")
                .unwrap()
                .contains("192.0.2.20:5070")
        );
        assert_eq!(
            registration.contact_address(),
            "203.0.113.7:40000".parse().unwrap()
        );

        // Same address again: now the registration is complete.
        let outputs = feed(
            &mut registration,
            reply_seen_at(
                &second,
                "203.0.113.7:40000",
                &[("contact", "<sip:alice@203.0.113.7:40000>;expires=60")],
            ),
            now,
        )
        .unwrap();
        assert!(!has_send(&outputs));
        assert_eq!(
            states(&outputs),
            [RegistrationState::Registered { expires: 60 }]
        );
    }

    #[test]
    fn matching_or_missing_server_view_keeps_the_local_contact() {
        let now = Instant::now();

        let mut registration = registration();
        let first = sent(&registration.start());
        let outputs = feed(
            &mut registration,
            reply_seen_at(&first, "192.0.2.20:5070", &[]),
            now,
        )
        .unwrap();
        assert!(!has_send(&outputs));
        assert_eq!(
            states(&outputs),
            [RegistrationState::Registered { expires: 120 }]
        );

        // `rport` echoed without a value tells nothing.
        let mut registration = self::registration();
        let first = sent(&registration.start());
        let outputs = feed(&mut registration, reply(&first, 200, &[]), now).unwrap();
        assert!(!has_send(&outputs));
        assert_eq!(
            registration.contact_address(),
            "192.0.2.20:5070".parse().unwrap()
        );
    }

    #[test]
    fn rport_without_received_keeps_the_local_ip() {
        let mut registration = registration();
        let first = sent(&registration.start());
        let mut response = reply(&first, 200, &[]);
        let via = first
            .headers
            .get("via")
            .unwrap()
            .replace(";rport", ";rport=6000");
        response.headers.insert("via", via).unwrap();
        let outputs = feed(&mut registration, response, Instant::now()).unwrap();
        assert_eq!(
            sent(&outputs).headers.get("contact"),
            Some("<sip:alice@192.0.2.20:6000>")
        );
    }

    #[test]
    fn contact_stops_following_a_server_that_keeps_changing_its_mind() {
        let mut registration = registration();
        let now = Instant::now();
        let mut request = sent(&registration.start());
        for port in [40001, 40002, 40003] {
            let outputs = feed(
                &mut registration,
                reply_seen_at(&request, &format!("203.0.113.7:{port}"), &[]),
                now,
            )
            .unwrap();
            request = sent(&outputs);
        }
        // The fourth different answer is accepted as it is.
        let outputs = feed(
            &mut registration,
            reply_seen_at(&request, "203.0.113.7:40004", &[]),
            now,
        )
        .unwrap();
        assert!(!has_send(&outputs));
        assert_eq!(
            states(&outputs),
            [RegistrationState::Registered { expires: 120 }]
        );
        assert_eq!(
            registration.contact_address(),
            "203.0.113.7:40003".parse().unwrap()
        );
    }

    #[test]
    fn keepalive_is_due_every_interval_only_while_registered() {
        let mut registration = registration();
        let now = Instant::now();
        registration.start();
        assert!(!registration.keepalive_due(now + KEEPALIVE_INTERVAL * 5));

        let mut registration = self::registration();
        register(&mut registration, now);
        // The first call after registering only starts the clock.
        assert!(!registration.keepalive_due(now));
        assert!(!registration.keepalive_due(now + KEEPALIVE_INTERVAL - Duration::from_millis(1)));
        assert!(registration.keepalive_due(now + KEEPALIVE_INTERVAL));
        assert!(!registration.keepalive_due(now + KEEPALIVE_INTERVAL));
        assert!(registration.keepalive_due(now + KEEPALIVE_INTERVAL * 2));

        registration.unregister();
        registration.on_timeout(now);
        assert!(!registration.keepalive_due(now + KEEPALIVE_INTERVAL * 10));
    }

    #[test]
    fn server_text_is_split_into_host_and_port() {
        let parse = |server: &str| Account::new(server, "alice", "x").server_host_port();
        assert_eq!(
            parse("pbx.example.com"),
            Ok(("pbx.example.com".to_owned(), 5060))
        );
        assert_eq!(
            parse(" pbx.example.com:5080 "),
            Ok(("pbx.example.com".to_owned(), 5080))
        );
        assert_eq!(parse("192.0.2.1:5062"), Ok(("192.0.2.1".to_owned(), 5062)));
        assert_eq!(
            parse("[2001:db8::1]:5070"),
            Ok(("2001:db8::1".to_owned(), 5070))
        );
        assert_eq!(parse("[2001:db8::1]"), Ok(("2001:db8::1".to_owned(), 5060)));
        assert_eq!(parse("2001:db8::1"), Ok(("2001:db8::1".to_owned(), 5060)));
        assert!(parse("").is_err());
        assert!(parse("pbx:0").is_err());
        assert!(parse("pbx:port").is_err());
        assert!(parse("alice@pbx").is_err());
    }

    #[test]
    fn domain_defaults_to_the_server_host() {
        let account = Account::new("pbx.example.com:5080", "alice", "x");
        assert_eq!(account.effective_domain(), Ok("pbx.example.com".to_owned()));
        assert_eq!(
            account.with_domain("example.com").effective_domain(),
            Ok("example.com".to_owned())
        );
        assert_eq!(
            Account::new("[2001:db8::1]:5070", "alice", "x").effective_domain(),
            Ok("[2001:db8::1]".to_owned())
        );
    }

    #[test]
    fn invalid_accounts_are_refused() {
        let server: SocketAddr = "192.0.2.10:5060".parse().unwrap();
        let local: SocketAddr = "192.0.2.20:5070".parse().unwrap();
        let new = |account: Account| Registration::new(account, server, local, "test").is_err();
        assert!(new(Account::new("192.0.2.10", "", "x")));
        assert!(new(Account::new("192.0.2.10", "al ice", "x")));
        assert!(new(Account::new("192.0.2.10", "alice@pbx", "x")));
        assert!(new(Account::new("192.0.2.10", "alice", "x").with_expires(0)));
        assert!(new(
            Account::new("192.0.2.10", "alice", "x").with_domain("a b")
        ));
        assert!(
            Registration::new(
                Account::new("192.0.2.10", "alice", "x"),
                server,
                "[::1]:5070".parse().unwrap(),
                "test"
            )
            .is_err()
        );
    }

    #[test]
    fn debug_output_hides_the_password() {
        let text = format!("{:?}", Account::new("pbx", "alice", "top-secret"));
        assert!(!text.contains("top-secret"));
    }
}
