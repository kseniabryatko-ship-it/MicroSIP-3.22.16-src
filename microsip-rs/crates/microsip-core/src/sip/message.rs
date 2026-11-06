use std::error::Error;
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SipMessageError {
    message: String,
}

impl SipMessageError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for SipMessageError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for SipMessageError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SipVersion {
    pub major: u8,
    pub minor: u8,
}

impl Default for SipVersion {
    fn default() -> Self {
        Self { major: 2, minor: 0 }
    }
}

impl SipVersion {
    fn parse(input: &str) -> Result<Self, SipMessageError> {
        let (protocol, version) = input
            .split_once('/')
            .ok_or_else(|| SipMessageError::new("SIP version has no protocol"))?;

        if !protocol.eq_ignore_ascii_case("SIP") {
            return Err(SipMessageError::new("unsupported SIP protocol"));
        }

        let mut components = version.split('.');

        let major = components
            .next()
            .ok_or_else(|| SipMessageError::new("SIP version has no major version"))?
            .parse::<u8>()
            .map_err(|_| SipMessageError::new("invalid SIP major version"))?;

        let minor = components
            .next()
            .ok_or_else(|| SipMessageError::new("SIP version has no minor version"))?
            .parse::<u8>()
            .map_err(|_| SipMessageError::new("invalid SIP minor version"))?;

        if components.next().is_some() {
            return Err(SipMessageError::new("invalid SIP version"));
        }

        Ok(Self { major, minor })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Method {
    Register,
    Invite,
    Ack,
    Bye,
    Cancel,
    Options,
    Message,
    Info,
    Update,
    Prack,
    Subscribe,
    Notify,
    Publish,
    Refer,
    Other(String),
}

impl Method {
    fn parse(input: &str) -> Result<Self, SipMessageError> {
        if input.is_empty() {
            return Err(SipMessageError::new("SIP method is empty"));
        }

        let normalized = input.to_ascii_uppercase();

        Ok(match normalized.as_str() {
            "REGISTER" => Self::Register,
            "INVITE" => Self::Invite,
            "ACK" => Self::Ack,
            "BYE" => Self::Bye,
            "CANCEL" => Self::Cancel,
            "OPTIONS" => Self::Options,
            "MESSAGE" => Self::Message,
            "INFO" => Self::Info,
            "UPDATE" => Self::Update,
            "PRACK" => Self::Prack,
            "SUBSCRIBE" => Self::Subscribe,
            "NOTIFY" => Self::Notify,
            "PUBLISH" => Self::Publish,
            "REFER" => Self::Refer,
            _ => Self::Other(normalized),
        })
    }

    pub fn as_str(&self) -> &str {
        match self {
            Self::Register => "REGISTER",
            Self::Invite => "INVITE",
            Self::Ack => "ACK",
            Self::Bye => "BYE",
            Self::Cancel => "CANCEL",
            Self::Options => "OPTIONS",
            Self::Message => "MESSAGE",
            Self::Info => "INFO",
            Self::Update => "UPDATE",
            Self::Prack => "PRACK",
            Self::Subscribe => "SUBSCRIBE",
            Self::Notify => "NOTIFY",
            Self::Publish => "PUBLISH",
            Self::Refer => "REFER",
            Self::Other(value) => value.as_str(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Header {
    pub name: String,
    pub value: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Headers {
    entries: Vec<Header>,
}

impl Headers {
    pub fn get(&self, name: &str) -> Option<&str> {
        self.entries
            .iter()
            .find(|entry| entry.name.eq_ignore_ascii_case(name))
            .map(|entry| entry.value.as_str())
    }

    pub fn get_all(&self, name: &str) -> Vec<&str> {
        self.entries
            .iter()
            .filter(|entry| entry.name.eq_ignore_ascii_case(name))
            .map(|entry| entry.value.as_str())
            .collect()
    }

    pub fn iter(&self) -> impl Iterator<Item = &Header> {
        self.entries.iter()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub method: Method,
    pub target: String,
    pub version: SipVersion,
    pub headers: Headers,
    pub body: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    pub version: SipVersion,
    pub status_code: u16,
    pub reason: String,
    pub headers: Headers,
    pub body: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Message {
    Request(Request),
    Response(Response),
}

impl Message {
    pub fn parse(input: &str) -> Result<Self, SipMessageError> {
        let (header_section, body) = split_message(input)?;

        let mut lines = header_section.split('\n');

        let start_line = lines
            .next()
            .unwrap_or_default()
            .strip_suffix('\r')
            .unwrap_or_else(|| header_section.split('\n').next().unwrap_or_default());

        let headers = parse_headers(lines)?;

        match headers.content_length()? {
            Some(expected) if body.len() != expected => {
                return Err(SipMessageError::new(format!(
                    "SIP body length is {}, but Content-Length is {}",
                    body.len(),
                    expected
                )));
            }
            Some(_) => {}
            None if !body.is_empty() => {
                return Err(SipMessageError::new(
                    "SIP message has a body but no Content-Length header",
                ));
            }
            None => {}
        }

        let message = match StartLine::parse(start_line)? {
            StartLine::Request {
                method,
                target,
                version,
            } => Message::Request(Request {
                method,
                target,
                version,
                headers,
                body: body.as_bytes().to_vec(),
            }),
            StartLine::Response {
                version,
                status_code,
                reason,
            } => Message::Response(Response {
                version,
                status_code,
                reason,
                headers,
                body: body.as_bytes().to_vec(),
            }),
        };

        Ok(message)
    }

    pub fn headers(&self) -> &Headers {
        match self {
            Self::Request(request) => &request.headers,
            Self::Response(response) => &response.headers,
        }
    }

    pub fn body(&self) -> &[u8] {
        match self {
            Self::Request(request) => request.body.as_slice(),
            Self::Response(response) => response.body.as_slice(),
        }
    }

    pub fn version(&self) -> SipVersion {
        match self {
            Self::Request(request) => request.version,
            Self::Response(response) => response.version,
        }
    }
}

enum StartLine {
    Request {
        method: Method,
        target: String,
        version: SipVersion,
    },
    Response {
        version: SipVersion,
        status_code: u16,
        reason: String,
    },
}

impl StartLine {
    fn parse(input: &str) -> Result<Self, SipMessageError> {
        let parts: Vec<&str> = input.split_whitespace().collect();

        if parts.is_empty() {
            return Err(SipMessageError::new("SIP start line is empty"));
        }

        let is_response = parts[0]
            .split_once('/')
            .map(|(protocol, _)| protocol.eq_ignore_ascii_case("SIP"))
            .unwrap_or(false);

        if is_response {
            if parts.len() < 2 {
                return Err(SipMessageError::new("SIP response has no status code"));
            }

            let version = SipVersion::parse(parts[0])?;

            let status_code = parts[1]
                .parse::<u16>()
                .map_err(|_| SipMessageError::new("invalid SIP status code"))?;

            if !(100..=699).contains(&status_code) {
                return Err(SipMessageError::new("SIP status code is out of range"));
            }

            let reason = parts[2..].join(" ");

            Ok(Self::Response {
                version,
                status_code,
                reason,
            })
        } else {
            if parts.len() != 3 {
                return Err(SipMessageError::new(
                    "SIP request line must contain method, target and version",
                ));
            }

            let method = Method::parse(parts[0])?;
            let target = parts[1].to_owned();
            let version = SipVersion::parse(parts[2])?;

            Ok(Self::Request {
                method,
                target,
                version,
            })
        }
    }
}

fn split_message(input: &str) -> Result<(&str, &str), SipMessageError> {
    if let Some(position) = input.find("\r\n\r\n") {
        return Ok((&input[..position], &input[position + 4..]));
    }

    if let Some(position) = input.find("\n\n") {
        return Ok((&input[..position], &input[position + 2..]));
    }

    Err(SipMessageError::new(
        "SIP message has no end-of-headers separator",
    ))
}

fn parse_headers<'a>(lines: impl Iterator<Item = &'a str>) -> Result<Headers, SipMessageError> {
    let mut headers = Headers::default();
    let mut last_header_index: Option<usize> = None;

    for raw_line in lines {
        let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);

        if line.is_empty() {
            continue;
        }

        if line.starts_with(' ') || line.starts_with('\t') {
            let index = last_header_index.ok_or_else(|| {
                SipMessageError::new("SIP header continuation has no previous header")
            })?;

            let continuation = line.trim();

            if continuation.is_empty() {
                continue;
            }

            headers.entries[index].value.push(' ');
            headers.entries[index].value.push_str(continuation);
            continue;
        }

        let (name, value) = line
            .split_once(':')
            .ok_or_else(|| SipMessageError::new("SIP header has no colon"))?;

        let name = name.trim();

        if !is_valid_header_name(name) {
            return Err(SipMessageError::new("SIP header has an invalid name"));
        }

        headers.entries.push(Header {
            name: name.to_ascii_lowercase(),
            value: value.trim().to_owned(),
        });

        last_header_index = Some(headers.entries.len() - 1);
    }

    Ok(headers)
}

fn is_valid_header_name(name: &str) -> bool {
    !name.is_empty()
        && name.chars().all(|character| {
            character.is_ascii_alphanumeric() || "!#$%&'*+-.^_`|~".contains(character)
        })
}

impl Headers {
    fn content_length(&self) -> Result<Option<usize>, SipMessageError> {
        let mut values = self.get_all("content-length").into_iter();

        let Some(first) = values.next() else {
            return Ok(None);
        };

        let expected = first
            .trim()
            .parse::<usize>()
            .map_err(|_| SipMessageError::new("invalid Content-Length header"))?;

        for value in values {
            let actual = value
                .trim()
                .parse::<usize>()
                .map_err(|_| SipMessageError::new("invalid Content-Length header"))?;

            if actual != expected {
                return Err(SipMessageError::new(
                    "SIP message contains conflicting Content-Length headers",
                ));
            }
        }

        Ok(Some(expected))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_request_with_headers() {
        let input = concat!(
            "INVITE sip:bob@example.com SIP/2.0\r\n",
            "Via: SIP/2.0/TCP client.example.com:5060;branch=z9hG4bK1\r\n",
            "From: <sip:alice@example.com>;tag=1928301774\r\n",
            "To: <sip:bob@example.com>\r\n",
            "Call-ID: call-123@example.com\r\n",
            "CSeq: 1 INVITE\r\n",
            "Max-Forwards: 70\r\n",
            "Content-Length: 0\r\n",
            "\r\n"
        );

        let message = Message::parse(input).expect("SIP request should be valid");

        let Message::Request(request) = message else {
            panic!("message should be a request");
        };

        assert_eq!(request.method, Method::Invite);
        assert_eq!(request.method.as_str(), "INVITE");
        assert_eq!(request.target, "sip:bob@example.com");
        assert_eq!(request.version, SipVersion { major: 2, minor: 0 });
        assert_eq!(request.headers.get("call-id"), Some("call-123@example.com"));
        assert_eq!(request.headers.get("cseq"), Some("1 INVITE"));
        assert_eq!(request.headers.get_all("via").len(), 1);
        assert!(request.body.is_empty());
    }

    #[test]
    fn parses_response_with_body() {
        let input = concat!(
            "SIP/2.0 200 OK\r\n",
            "Via: SIP/2.0/UDP one.example.com;branch=one\r\n",
            "Via: SIP/2.0/UDP two.example.com;branch=two\r\n",
            "From: <sip:alice@example.com>;tag=abc\r\n",
            "To: <sip:bob@example.com>;tag=def\r\n",
            "Call-ID: call-123@example.com\r\n",
            "Content-Length: 5\r\n",
            "\r\n",
            "hello"
        );

        let message = Message::parse(input).expect("SIP response should be valid");

        let Message::Response(response) = message else {
            panic!("message should be a response");
        };

        assert_eq!(response.status_code, 200);
        assert_eq!(response.reason, "OK");
        assert_eq!(response.headers.get_all("via").len(), 2);
        assert_eq!(response.body.as_slice(), b"hello");
    }

    #[test]
    fn parses_folded_header() {
        let input = concat!(
            "SIP/2.0 100 Trying\r\n",
            "Subject: first line\r\n",
            " second line\r\n",
            "Content-Length: 0\r\n",
            "\r\n"
        );

        let message = Message::parse(input).expect("SIP response should be valid");

        assert_eq!(
            message.headers().get("subject"),
            Some("first line second line")
        );
    }

    #[test]
    fn rejects_malformed_messages() {
        let invalid_messages = [
            "",
            "INVITE sip:bob@example.com\r\n\r\n",
            "INVITE sip:bob@example.com HTTP/2.0\r\n\r\n",
            "INVITE sip:bob@example.com SIP/2.0\r\nBrokenHeader\r\n\r\n",
            concat!(
                "INVITE sip:bob@example.com SIP/2.0\r\n",
                "Content-Length: 1\r\n",
                "\r\n",
                "xy"
            ),
            concat!(
                "INVITE sip:bob@example.com SIP/2.0\r\n",
                "Content-Length: 1\r\n",
                "Content-Length: 2\r\n",
                "\r\n",
                "x"
            ),
            concat!(
                "INVITE sip:bob@example.com SIP/2.0\r\n",
                "Content-Type: text/plain\r\n",
                "\r\n",
                "body"
            ),
            concat!("SIP/2.0 999 Invalid\r\n", "Content-Length: 0\r\n", "\r\n"),
        ];

        for input in invalid_messages {
            assert!(
                Message::parse(input).is_err(),
                "message should be invalid: {input}"
            );
        }
    }
}
impl Headers {
    pub fn insert(&mut self, name: &str, value: impl Into<String>) -> Result<(), SipMessageError> {
        let name = normalize_header_name(name)?;
        let value = value.into();

        validate_header_value(&value)?;

        self.entries.retain(|entry| entry.name != name);
        self.entries.push(Header { name, value });

        Ok(())
    }

    pub fn append(&mut self, name: &str, value: impl Into<String>) -> Result<(), SipMessageError> {
        let name = normalize_header_name(name)?;
        let value = value.into();

        validate_header_value(&value)?;

        self.entries.push(Header { name, value });

        Ok(())
    }

    pub fn remove(&mut self, name: &str) -> Option<String> {
        let normalized = normalize_header_name(name).ok()?;
        let mut removed = None;

        self.entries.retain(|entry| {
            if entry.name == normalized {
                if removed.is_none() {
                    removed = Some(entry.value.clone());
                }

                false
            } else {
                true
            }
        });

        removed
    }

    pub fn contains(&self, name: &str) -> bool {
        self.get(name).is_some()
    }
}

#[derive(Debug, Clone)]
pub struct RequestBuilder {
    method: Method,
    target: String,
    version: SipVersion,
    headers: Headers,
    body: Vec<u8>,
}

impl RequestBuilder {
    pub fn new(method: Method, target: impl Into<String>) -> Self {
        Self {
            method,
            target: target.into(),
            version: SipVersion::default(),
            headers: Headers::default(),
            body: Vec::new(),
        }
    }

    pub fn version(mut self, version: SipVersion) -> Self {
        self.version = version;
        self
    }

    pub fn header(mut self, name: &str, value: impl Into<String>) -> Result<Self, SipMessageError> {
        self.headers.insert(name, value)?;
        Ok(self)
    }

    pub fn append_header(
        mut self,
        name: &str,
        value: impl Into<String>,
    ) -> Result<Self, SipMessageError> {
        self.headers.append(name, value)?;
        Ok(self)
    }

    pub fn remove_header(mut self, name: &str) -> Self {
        self.headers.remove(name);
        self
    }

    pub fn body(mut self, body: Vec<u8>) -> Self {
        self.body = body;
        self
    }

    pub fn build(self) -> Result<Request, SipMessageError> {
        let Self {
            method,
            target,
            version,
            mut headers,
            body,
        } = self;

        validate_no_whitespace(&target, "SIP request target")?;

        for required_header in ["via", "from", "to", "call-id", "cseq"] {
            if !headers.contains(required_header) {
                return Err(SipMessageError::new(format!(
                    "SIP request is missing required header: {required_header}"
                )));
            }
        }

        let cseq = headers
            .get("cseq")
            .ok_or_else(|| SipMessageError::new("SIP request has no CSeq header"))?;

        validate_cseq(cseq, &method)?;

        headers.remove("content-length");
        headers.insert("content-length", body.len().to_string())?;

        Ok(Request {
            method,
            target,
            version,
            headers,
            body,
        })
    }
}

#[derive(Debug, Clone)]
pub struct ResponseBuilder {
    version: SipVersion,
    status_code: u16,
    reason: String,
    headers: Headers,
    body: Vec<u8>,
}

impl ResponseBuilder {
    pub fn new(status_code: u16, reason: impl Into<String>) -> Self {
        Self {
            version: SipVersion::default(),
            status_code,
            reason: reason.into(),
            headers: Headers::default(),
            body: Vec::new(),
        }
    }

    pub fn version(mut self, version: SipVersion) -> Self {
        self.version = version;
        self
    }

    pub fn header(mut self, name: &str, value: impl Into<String>) -> Result<Self, SipMessageError> {
        self.headers.insert(name, value)?;
        Ok(self)
    }

    pub fn append_header(
        mut self,
        name: &str,
        value: impl Into<String>,
    ) -> Result<Self, SipMessageError> {
        self.headers.append(name, value)?;
        Ok(self)
    }

    pub fn body(mut self, body: Vec<u8>) -> Self {
        self.body = body;
        self
    }

    pub fn build(self) -> Result<Response, SipMessageError> {
        let Self {
            version,
            status_code,
            reason,
            mut headers,
            body,
        } = self;

        if !(100..=699).contains(&status_code) {
            return Err(SipMessageError::new("SIP status code is out of range"));
        }

        validate_header_value(&reason)?;

        headers.remove("content-length");
        headers.insert("content-length", body.len().to_string())?;

        Ok(Response {
            version,
            status_code,
            reason,
            headers,
            body,
        })
    }
}

#[derive(Debug, Clone)]
pub struct RegisterRequest {
    pub target: String,
    pub aor: String,
    pub contact: String,
    pub via_sent_by: String,
    pub call_id: String,
    pub branch: String,
    pub from_tag: String,
    pub cseq: u32,
    pub expires: Option<u32>,
    pub user_agent: String,
}

impl RegisterRequest {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        target: impl Into<String>,
        aor: impl Into<String>,
        contact: impl Into<String>,
        via_sent_by: impl Into<String>,
        call_id: impl Into<String>,
        branch: impl Into<String>,
        from_tag: impl Into<String>,
    ) -> Self {
        Self {
            target: target.into(),
            aor: aor.into(),
            contact: contact.into(),
            via_sent_by: via_sent_by.into(),
            call_id: call_id.into(),
            branch: branch.into(),
            from_tag: from_tag.into(),
            cseq: 1,
            expires: None,
            user_agent: "MicroSIP-Rust/0.1.0".to_owned(),
        }
    }

    pub fn cseq(mut self, cseq: u32) -> Self {
        self.cseq = cseq;
        self
    }

    pub fn expires(mut self, seconds: u32) -> Self {
        self.expires = Some(seconds);
        self
    }

    pub fn user_agent(mut self, user_agent: impl Into<String>) -> Self {
        self.user_agent = user_agent.into();
        self
    }

    pub fn build(self) -> Result<Request, SipMessageError> {
        validate_no_whitespace(&self.target, "REGISTER target")?;
        validate_no_whitespace(&self.aor, "REGISTER address of record")?;
        validate_no_whitespace(&self.contact, "REGISTER contact")?;
        validate_no_whitespace(&self.via_sent_by, "REGISTER Via host")?;
        validate_no_whitespace(&self.call_id, "REGISTER Call-ID")?;
        validate_no_whitespace(&self.branch, "REGISTER branch")?;
        validate_no_whitespace(&self.from_tag, "REGISTER From tag")?;

        let mut builder = RequestBuilder::new(Method::Register, self.target)
            .header(
                "via",
                format!("SIP/2.0/UDP {};branch={}", self.via_sent_by, self.branch),
            )?
            .header(
                "from",
                format!("{};tag={}", format_angle_uri(&self.aor), self.from_tag),
            )?
            .header("to", format_angle_uri(&self.aor))?
            .header("call-id", self.call_id)?
            .header("cseq", format!("{} REGISTER", self.cseq))?
            .header("contact", format_angle_uri(&self.contact))?
            .header("max-forwards", "70")?;

        if let Some(expires) = self.expires {
            builder = builder.header("expires", expires.to_string())?;
        }

        if !self.user_agent.is_empty() {
            builder = builder.header("user-agent", self.user_agent)?;
        }

        builder.build()
    }
}

#[derive(Debug, Clone)]
pub struct InviteRequest {
    pub target: String,
    pub from_uri: String,
    pub to_uri: String,
    pub contact: String,
    pub via_sent_by: String,
    pub call_id: String,
    pub branch: String,
    pub from_tag: String,
    pub to_tag: Option<String>,
    pub cseq: u32,
    pub body: Vec<u8>,
}

impl InviteRequest {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        target: impl Into<String>,
        from_uri: impl Into<String>,
        to_uri: impl Into<String>,
        contact: impl Into<String>,
        via_sent_by: impl Into<String>,
        call_id: impl Into<String>,
        branch: impl Into<String>,
        from_tag: impl Into<String>,
    ) -> Self {
        Self {
            target: target.into(),
            from_uri: from_uri.into(),
            to_uri: to_uri.into(),
            contact: contact.into(),
            via_sent_by: via_sent_by.into(),
            call_id: call_id.into(),
            branch: branch.into(),
            from_tag: from_tag.into(),
            to_tag: None,
            cseq: 1,
            body: Vec::new(),
        }
    }

    pub fn to_tag(mut self, to_tag: impl Into<String>) -> Self {
        self.to_tag = Some(to_tag.into());
        self
    }

    pub fn cseq(mut self, cseq: u32) -> Self {
        self.cseq = cseq;
        self
    }

    pub fn body(mut self, body: Vec<u8>) -> Self {
        self.body = body;
        self
    }

    pub fn build(self) -> Result<Request, SipMessageError> {
        validate_no_whitespace(&self.target, "INVITE target")?;
        validate_no_whitespace(&self.from_uri, "INVITE From URI")?;
        validate_no_whitespace(&self.to_uri, "INVITE To URI")?;
        validate_no_whitespace(&self.contact, "INVITE contact")?;
        validate_no_whitespace(&self.via_sent_by, "INVITE Via host")?;
        validate_no_whitespace(&self.call_id, "INVITE Call-ID")?;
        validate_no_whitespace(&self.branch, "INVITE branch")?;
        validate_no_whitespace(&self.from_tag, "INVITE From tag")?;

        if let Some(to_tag) = &self.to_tag {
            validate_no_whitespace(to_tag, "INVITE To tag")?;
        }

        let to_value = match &self.to_tag {
            Some(to_tag) => format!("{};tag={}", format_angle_uri(&self.to_uri), to_tag),
            None => format_angle_uri(&self.to_uri),
        };

        let mut builder = RequestBuilder::new(Method::Invite, self.target)
            .header(
                "via",
                format!("SIP/2.0/UDP {};branch={}", self.via_sent_by, self.branch),
            )?
            .header(
                "from",
                format!("{};tag={}", format_angle_uri(&self.from_uri), self.from_tag),
            )?
            .header("to", to_value)?
            .header("call-id", self.call_id)?
            .header("cseq", format!("{} INVITE", self.cseq))?
            .header("contact", format_angle_uri(&self.contact))?
            .header("max-forwards", "70")?
            .body(self.body);

        if !builder.body.is_empty() {
            builder = builder.header("content-type", "application/sdp")?;
        }

        builder.build()
    }
}

impl Request {
    pub fn serialize(&self) -> Result<Vec<u8>, SipMessageError> {
        let expected_length = self
            .headers
            .content_length()?
            .ok_or_else(|| SipMessageError::new("SIP request has no Content-Length header"))?;

        if expected_length != self.body.len() {
            return Err(SipMessageError::new(
                "SIP request Content-Length does not match body length",
            ));
        }

        let mut output = Vec::new();

        output.extend_from_slice(self.method.as_str().as_bytes());
        output.push(b' ');
        output.extend_from_slice(self.target.as_bytes());
        output.extend_from_slice(
            format!(" SIP/{}.{}\r\n", self.version.major, self.version.minor).as_bytes(),
        );

        serialize_headers(&self.headers, &mut output)?;
        output.extend_from_slice(b"\r\n");
        output.extend_from_slice(&self.body);

        Ok(output)
    }
}

impl Response {
    pub fn serialize(&self) -> Result<Vec<u8>, SipMessageError> {
        let expected_length = self
            .headers
            .content_length()?
            .ok_or_else(|| SipMessageError::new("SIP response has no Content-Length header"))?;

        if expected_length != self.body.len() {
            return Err(SipMessageError::new(
                "SIP response Content-Length does not match body length",
            ));
        }

        let mut output = Vec::new();

        output.extend_from_slice(
            format!("SIP/{}.{} ", self.version.major, self.version.minor).as_bytes(),
        );
        output.extend_from_slice(self.status_code.to_string().as_bytes());
        output.push(b' ');
        output.extend_from_slice(self.reason.as_bytes());
        output.extend_from_slice(b"\r\n");

        serialize_headers(&self.headers, &mut output)?;
        output.extend_from_slice(b"\r\n");
        output.extend_from_slice(&self.body);

        Ok(output)
    }
}

impl Message {
    pub fn serialize(&self) -> Result<Vec<u8>, SipMessageError> {
        match self {
            Self::Request(request) => request.serialize(),
            Self::Response(response) => response.serialize(),
        }
    }
}

fn serialize_headers(headers: &Headers, output: &mut Vec<u8>) -> Result<(), SipMessageError> {
    for header in headers.iter() {
        if !is_valid_header_name(&header.name) {
            return Err(SipMessageError::new("SIP header has an invalid name"));
        }

        validate_header_value(&header.value)?;

        output.extend_from_slice(header.name.as_bytes());
        output.extend_from_slice(b": ");
        output.extend_from_slice(header.value.as_bytes());
        output.extend_from_slice(b"\r\n");
    }

    Ok(())
}

fn normalize_header_name(name: &str) -> Result<String, SipMessageError> {
    let name = name.trim();

    if !is_valid_header_name(name) {
        return Err(SipMessageError::new("SIP header has an invalid name"));
    }

    Ok(name.to_ascii_lowercase())
}

fn validate_header_value(value: &str) -> Result<(), SipMessageError> {
    for character in value.chars() {
        if character == '\r' || character == '\n' {
            return Err(SipMessageError::new(
                "SIP header value contains a line break",
            ));
        }

        if character.is_control() && character != '\t' {
            return Err(SipMessageError::new(
                "SIP header value contains a control character",
            ));
        }
    }

    Ok(())
}

fn validate_no_whitespace(value: &str, field_name: &str) -> Result<(), SipMessageError> {
    if value.is_empty() {
        return Err(SipMessageError::new(format!(
            "{field_name} cannot be empty"
        )));
    }

    if value
        .chars()
        .any(|character| character.is_whitespace() || character.is_control())
    {
        return Err(SipMessageError::new(format!(
            "{field_name} contains whitespace or a control character"
        )));
    }

    Ok(())
}

fn validate_cseq(value: &str, method: &Method) -> Result<(), SipMessageError> {
    let mut parts = value.split_whitespace();

    let sequence_number = parts
        .next()
        .ok_or_else(|| SipMessageError::new("CSeq header is empty"))?;

    sequence_number
        .parse::<u32>()
        .map_err(|_| SipMessageError::new("CSeq has an invalid sequence number"))?;

    let sequence_method = parts
        .next()
        .ok_or_else(|| SipMessageError::new("CSeq has no method"))?;

    if !sequence_method.eq_ignore_ascii_case(method.as_str()) {
        return Err(SipMessageError::new(
            "CSeq method does not match the SIP request method",
        ));
    }

    if parts.next().is_some() {
        return Err(SipMessageError::new("CSeq has too many fields"));
    }

    Ok(())
}

fn format_angle_uri(uri: &str) -> String {
    if uri.starts_with('<') && uri.ends_with('>') {
        uri.to_owned()
    } else {
        format!("<{uri}>")
    }
}

#[cfg(test)]
mod builder_tests {
    use super::*;

    #[test]
    fn builds_and_serializes_register() {
        let request = RegisterRequest::new(
            "sip:example.com",
            "sip:alice@example.com",
            "sip:alice@192.0.2.10:5060",
            "192.0.2.10:5060",
            "call-123",
            "z9hG4bK776asdhds",
            "tag-1",
        )
        .expires(3600)
        .build()
        .expect("REGISTER should be valid");

        let bytes = request.serialize().expect("REGISTER should serialize");
        let text = String::from_utf8(bytes).expect("REGISTER should be UTF-8");

        assert!(text.starts_with("REGISTER sip:example.com SIP/2.0\r\n"));
        assert!(text.contains("via: SIP/2.0/UDP 192.0.2.10:5060;branch=z9hG4bK776asdhds\r\n"));
        assert!(text.contains("from: <sip:alice@example.com>;tag=tag-1\r\n"));
        assert!(text.contains("to: <sip:alice@example.com>\r\n"));
        assert!(text.contains("contact: <sip:alice@192.0.2.10:5060>\r\n"));
        assert!(text.contains("expires: 3600\r\n"));
        assert!(text.ends_with("content-length: 0\r\n\r\n"));

        let parsed = Message::parse(&text).expect("REGISTER should parse");

        assert_eq!(parsed, Message::Request(request));
    }

    #[test]
    fn rejects_missing_mandatory_request_headers() {
        let result = RequestBuilder::new(Method::Invite, "sip:bob@example.com").build();

        assert!(result.is_err());
    }

    #[test]
    fn rejects_header_injection() {
        let result = RequestBuilder::new(Method::Invite, "sip:bob@example.com")
            .header("via", "valid\r\nEvil-Header: injected");

        assert!(result.is_err());
    }

    #[test]
    fn builds_invite_with_sdp_body() {
        let sdp = concat!(
            "v=0\r\n",
            "o=alice 289084452 289084452 IN IP4 192.0.2.10\r\n",
            "s=MicroSIP Rust\r\n",
            "c=IN IP4 192.0.2.10\r\n",
            "t=0 0\r\n",
            "m=audio 40000 RTP/AVP 0\r\n",
            "a=rtpmap:0 PCMU/8000\r\n"
        );

        let request = InviteRequest::new(
            "sip:bob@example.com",
            "sip:alice@example.com",
            "sip:bob@example.com",
            "sip:alice@192.0.2.10:5060",
            "192.0.2.10:5060",
            "call-456",
            "z9hG4bK776asdhds2",
            "tag-2",
        )
        .body(sdp.as_bytes().to_vec())
        .build()
        .expect("INVITE should be valid");

        let text = String::from_utf8(request.serialize().expect("INVITE should serialize"))
            .expect("INVITE should be UTF-8");

        assert!(text.starts_with("INVITE sip:bob@example.com SIP/2.0\r\n"));
        assert!(text.contains("content-type: application/sdp\r\n"));
        assert!(text.contains("m=audio 40000 RTP/AVP 0\r\n"));

        let parsed = Message::parse(&text).expect("INVITE should parse");

        assert_eq!(parsed, Message::Request(request));
    }

    #[test]
    fn serializes_response_with_body() {
        let response = ResponseBuilder::new(200, "OK")
            .header("via", "SIP/2.0/UDP example.com;branch=test")
            .expect("Via header should be valid")
            .header("from", "<sip:alice@example.com>;tag=abc")
            .expect("From header should be valid")
            .header("to", "<sip:bob@example.com>;tag=def")
            .expect("To header should be valid")
            .header("call-id", "call-789")
            .expect("Call-ID header should be valid")
            .body(b"hello".to_vec())
            .build()
            .expect("response should be valid");

        let text = String::from_utf8(response.serialize().expect("response should serialize"))
            .expect("response should be UTF-8");

        assert!(text.starts_with("SIP/2.0 200 OK\r\n"));
        assert!(text.ends_with("\r\n\r\nhello"));

        let parsed = Message::parse(&text).expect("response should parse");

        assert_eq!(parsed, Message::Response(response));
    }
}
