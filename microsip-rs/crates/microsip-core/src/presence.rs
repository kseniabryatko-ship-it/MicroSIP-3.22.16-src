//! The user's own status ("online", "away", "busy") for colleagues, and
//! theirs for the user, as MicroSIP shows them.
//!
//! The phone tells the exchange its status with PUBLISH (RFC 3903) and a
//! PIDF document (RFC 3863) with the rich-presence activity (RFC 4480);
//! the exchange passes it on to everybody subscribed to the `presence`
//! event of this account. [`Publication`] is that exchange without the
//! network, like `Subscription` for SUBSCRIBE: the executor sends what it
//! makes and hands it the answers.
//!
//! Not every exchange takes PUBLISH from phones. Asterisk (and FreePBX)
//! answers with an error: there colleagues see only whether a phone is
//! free, ringing or talking (the busy lamps). The phone then says so once
//! and stops trying until the status changes.

use crate::sip::message::{Message, Method, RequestBuilder, Response};
use crate::transaction_headers::TransactionHeaders;
use std::time::{Duration, Instant};

/// Lifetime asked for a published status.
pub const PUBLISH_EXPIRES: u32 = 3_600;
/// The shortest pause between two refreshes, whatever the server grants.
const MIN_REFRESH: Duration = Duration::from_secs(60);
/// Pause before a status the server could not take is sent again.
const RETRY_DELAY: Duration = Duration::from_secs(300);

/// The user's own status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PresenceStatus {
    #[default]
    Online,
    Away,
    Busy,
}

impl PresenceStatus {
    pub const ALL: [PresenceStatus; 3] = [
        PresenceStatus::Online,
        PresenceStatus::Away,
        PresenceStatus::Busy,
    ];

    /// Name in the settings file and in the PIDF note.
    pub const fn name(self) -> &'static str {
        match self {
            PresenceStatus::Online => "online",
            PresenceStatus::Away => "away",
            PresenceStatus::Busy => "busy",
        }
    }

    /// Unknown names are "online".
    pub fn from_name(name: &str) -> Self {
        let name = name.trim();
        Self::ALL
            .into_iter()
            .find(|status| status.name().eq_ignore_ascii_case(name))
            .unwrap_or_default()
    }

    /// The note other phones show, in English like MicroSIP's.
    const fn note(self) -> &'static str {
        match self {
            PresenceStatus::Online => "Online",
            PresenceStatus::Away => "Away",
            PresenceStatus::Busy => "Busy",
        }
    }
}

/// The PIDF document of the user's status. `entity` is the account's
/// address (`sip:4009@pbx`); `tuple` a stable identifier.
pub fn pidf_body(entity: &str, tuple: &str, status: PresenceStatus) -> String {
    let activity = match status {
        PresenceStatus::Online => String::new(),
        PresenceStatus::Away => "<rpid:activities><rpid:away/></rpid:activities>".to_owned(),
        PresenceStatus::Busy => "<rpid:activities><rpid:busy/></rpid:activities>".to_owned(),
    };
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\r\n\
         <presence xmlns=\"urn:ietf:params:xml:ns:pidf\" \
         xmlns:dm=\"urn:ietf:params:xml:ns:pidf:data-model\" \
         xmlns:rpid=\"urn:ietf:params:xml:ns:pidf:rpid\" entity=\"{entity}\">\r\n\
         <tuple id=\"{tuple}\"><status><basic>open</basic></status>\
         <note>{note}</note></tuple>\r\n\
         <dm:person id=\"p-{tuple}\">{activity}<dm:note>{note}</dm:note></dm:person>\r\n\
         </presence>\r\n",
        note = status.note()
    )
}

/// What a colleague's status says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemotePresence {
    /// The phone is reachable (`open`); `false` when it is offline.
    pub online: bool,
    /// Their chosen status, when they set one this phone knows.
    pub status: Option<PresenceStatus>,
    /// They are in a call (`on-the-phone`), as some exchanges report.
    pub on_the_phone: bool,
    /// The free text of the status, if any.
    pub note: String,
}

/// Reads a PIDF document; `None` when it is not one.
pub fn parse_pidf(body: &str) -> Option<RemotePresence> {
    let lower = body.to_ascii_lowercase();
    if !lower.contains("<presence") {
        return None;
    }
    // Namespace prefixes differ between servers: only local names count.
    let basic = element_texts(body, "basic");
    let online = basic.iter().any(|text| text.eq_ignore_ascii_case("open"));
    let note = element_texts(body, "note")
        .into_iter()
        .find(|text| !text.is_empty())
        .unwrap_or_default();
    let has = |name: &str| has_element(&lower, name);
    let status = if has("away") || has("vacation") || has("holiday") {
        Some(PresenceStatus::Away)
    } else if has("busy") || has("meeting") || has("do-not-disturb") {
        Some(PresenceStatus::Busy)
    } else {
        match note.to_ascii_lowercase().as_str() {
            "away" | "be right back" | "out to lunch" => Some(PresenceStatus::Away),
            "busy" | "do not disturb" | "dnd" => Some(PresenceStatus::Busy),
            "online" | "available" | "ready" => Some(PresenceStatus::Online),
            _ => online.then_some(PresenceStatus::Online),
        }
    };
    Some(RemotePresence {
        online,
        status,
        on_the_phone: has("on-the-phone"),
        note,
    })
}

/// True when an element with this local name occurs (`<rpid:away/>`).
fn has_element(lower: &str, name: &str) -> bool {
    lower.match_indices('<').any(|(at, _)| {
        let tag = &lower[at + 1..];
        let tag = tag
            .split(['>', ' ', '/', '\t', '\r', '\n'])
            .next()
            .unwrap_or("");
        tag.rsplit(':').next() == Some(name)
    })
}

/// The texts of every element with this local name, trimmed.
fn element_texts(body: &str, name: &str) -> Vec<String> {
    let mut texts = Vec::new();
    let mut rest = body;
    while let Some(start) = rest.find('<') {
        let after = &rest[start + 1..];
        let Some(end) = after.find('>') else {
            break;
        };
        let tag = &after[..end];
        rest = &after[end + 1..];
        if tag.starts_with('/') || tag.ends_with('/') {
            continue;
        }
        let tag_name = tag.split_whitespace().next().unwrap_or("");
        if tag_name.rsplit(':').next() != Some(name) {
            continue;
        }
        let close = format!("</{tag_name}>");
        if let Some(stop) = rest.find(&close) {
            texts.push(unescape(rest[..stop].trim()));
        }
    }
    texts
}

fn unescape(text: &str) -> String {
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// What the executor has to do after an answer to PUBLISH.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PublishReply {
    /// Nothing; still waiting, or done.
    Wait,
    /// The status is now published.
    Published,
    /// The server wants the password: send again with credentials.
    Authorize,
    /// The server forgot the earlier status: send it afresh now.
    Again,
    /// The server does not take it; the text says why.
    Refused(String),
}

/// The user's status as published on one account.
#[derive(Debug)]
pub struct Publication {
    status: PresenceStatus,
    /// This phone's account: `sip:4009@pbx`.
    aor: String,
    via_sent_by: String,
    call_id: String,
    from_tag: String,
    cseq: u32,
    branch_counter: u64,
    /// The server's name for the published status (SIP-ETag).
    etag: Option<String>,
    /// The status changed since it was last published.
    changed: bool,
    pending: Option<TransactionHeaders>,
    /// The pending request carries credentials already.
    authorized: bool,
    /// The pending request removes the status.
    removing: bool,
    /// When to refresh, or to try again.
    next: Option<Instant>,
    /// The server refused; nothing is sent until the status changes.
    refused: bool,
}

impl Publication {
    /// `aor` is the account's address, `via_sent_by` this phone's address,
    /// `seed` a string unique to this process.
    pub fn new(
        status: PresenceStatus,
        aor: impl Into<String>,
        via_sent_by: impl Into<String>,
        seed: &str,
    ) -> Self {
        Self {
            status,
            aor: aor.into(),
            via_sent_by: via_sent_by.into(),
            call_id: format!("pub-{seed}"),
            from_tag: format!("pub-{seed}"),
            cseq: 0,
            branch_counter: 0,
            etag: None,
            changed: true,
            pending: None,
            authorized: false,
            removing: false,
            next: None,
            refused: false,
        }
    }

    pub fn status(&self) -> PresenceStatus {
        self.status
    }

    pub fn call_id(&self) -> &str {
        &self.call_id
    }

    /// The address PUBLISH goes to, which the credentials must name.
    pub fn uri(&self) -> &str {
        &self.aor
    }

    pub fn is_published(&self) -> bool {
        self.etag.is_some()
    }

    /// A new status: it is to be sent at once (see [`Self::wants_sending`]).
    pub fn set_status(&mut self, status: PresenceStatus) {
        if status != self.status {
            self.status = status;
            self.changed = true;
            self.refused = false;
        }
    }

    /// True when a request should go now: a change, a refresh or a retry.
    pub fn wants_sending(&self, now: Instant) -> bool {
        self.pending.is_none()
            && !self.removing
            && ((self.changed && !self.refused) || self.next.is_some_and(|due| now >= due))
    }

    /// The PUBLISH to send now. With `authorization` it answers the
    /// server's challenge; with `remove` it takes the status down.
    pub fn request(
        &mut self,
        authorization: Option<(&str, String)>,
        remove: bool,
    ) -> Result<Vec<u8>, String> {
        self.cseq += 1;
        self.branch_counter += 1;
        self.authorized = authorization.is_some();
        self.removing = remove;
        self.next = None;
        let branch = format!("z9hG4bK-{}-{}", self.from_tag, self.branch_counter);
        let expires = if remove { 0 } else { PUBLISH_EXPIRES };
        let error = |error: crate::sip::message::SipMessageError| error.to_string();
        let mut builder = RequestBuilder::new(Method::Publish, self.aor.clone())
            .header(
                "via",
                format!("SIP/2.0/UDP {};branch={branch};rport", self.via_sent_by),
            )
            .map_err(error)?
            .header("max-forwards", "70")
            .map_err(error)?
            .header("from", format!("<{}>;tag={}", self.aor, self.from_tag))
            .map_err(error)?
            .header("to", format!("<{}>", self.aor))
            .map_err(error)?
            .header("call-id", self.call_id.clone())
            .map_err(error)?
            .header("cseq", format!("{} PUBLISH", self.cseq))
            .map_err(error)?
            .header("event", "presence")
            .map_err(error)?
            .header("expires", expires.to_string())
            .map_err(error)?;
        if let Some(etag) = self.etag.as_ref() {
            builder = builder
                .header("sip-if-match", etag.clone())
                .map_err(error)?;
        }
        // A refresh or a removal carries no document; a new status does.
        let with_body = !remove && (self.changed || self.etag.is_none());
        if with_body {
            builder = builder
                .header("content-type", "application/pidf+xml")
                .map_err(error)?
                .body(pidf_body(&self.aor, &self.from_tag, self.status).into_bytes());
        }
        if let Some((name, value)) = authorization {
            builder = builder.header(name, value).map_err(error)?;
        }
        let request = builder.build().map_err(error)?;
        self.pending = Some(
            TransactionHeaders::from_message(&Message::Request(request.clone()))
                .map_err(|error| error.to_string())?,
        );
        if with_body {
            self.changed = false;
        }
        request.serialize().map_err(error)
    }

    /// True when `headers` belong to the answer of the pending PUBLISH.
    pub fn is_answer(&self, headers: &TransactionHeaders) -> bool {
        self.pending
            .as_ref()
            .is_some_and(|pending| pending.matches_response(headers))
    }

    /// Takes the answer to the pending PUBLISH.
    pub fn on_response(&mut self, response: &Response, now: Instant) -> PublishReply {
        let status = response.status_code;
        if status < 200 {
            return PublishReply::Wait;
        }
        if matches!(status, 401 | 407) && !self.authorized {
            return PublishReply::Authorize;
        }
        self.pending = None;
        if self.removing {
            self.etag = None;
            return PublishReply::Wait;
        }
        if (200..300).contains(&status) {
            self.etag = response
                .headers
                .get("sip-etag")
                .map(|etag| etag.trim().to_owned())
                .filter(|etag| !etag.is_empty());
            let granted = response
                .headers
                .get("expires")
                .and_then(|value| value.trim().parse::<u64>().ok())
                .unwrap_or(u64::from(PUBLISH_EXPIRES));
            let refresh = Duration::from_secs(granted * 4 / 5).max(MIN_REFRESH);
            self.next = Some(now + refresh);
            return PublishReply::Published;
        }
        if status == 412 && self.etag.is_some() {
            // The server lost what was published: the whole status again.
            self.etag = None;
            self.changed = true;
            return PublishReply::Again;
        }
        self.etag = None;
        if (500..600).contains(&status) {
            // Perhaps a passing trouble: try again later, with the whole
            // status (there is no published one to refer to).
            self.next = Some(now + RETRY_DELAY);
        } else {
            self.refused = true;
        }
        PublishReply::Refused(format!("{status} {}", response.reason))
    }

    /// The PUBLISH got no answer at all: tried again later.
    pub fn on_timeout(&mut self, now: Instant) {
        self.pending = None;
        if !self.removing {
            self.next = Some(now + RETRY_DELAY);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn response(status: u16, headers: &[(&str, &str)]) -> Response {
        let mut text = format!("SIP/2.0 {status} Reason\r\n");
        for (name, value) in headers {
            text.push_str(&format!("{name}: {value}\r\n"));
        }
        text.push_str("Content-Length: 0\r\n\r\n");
        match Message::parse(&text).unwrap() {
            Message::Response(response) => response,
            Message::Request(_) => unreachable!(),
        }
    }

    fn text(bytes: &[u8]) -> String {
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    #[test]
    fn statuses_are_named_for_the_settings() {
        for status in PresenceStatus::ALL {
            assert_eq!(PresenceStatus::from_name(status.name()), status);
        }
        assert_eq!(PresenceStatus::from_name("??"), PresenceStatus::Online);
    }

    #[test]
    fn the_own_status_is_a_pidf_document_with_its_activity() {
        let body = pidf_body("sip:4009@pbx", "t1", PresenceStatus::Away);
        assert!(body.contains("entity=\"sip:4009@pbx\""));
        assert!(body.contains("<basic>open</basic>"));
        assert!(body.contains("<rpid:away/>"));
        assert!(body.contains("<note>Away</note>"));
        // What this phone publishes, it reads back the same way.
        for status in PresenceStatus::ALL {
            let read = parse_pidf(&pidf_body("sip:a@b", "t", status)).unwrap();
            assert!(read.online);
            assert_eq!(read.status, Some(status), "{status:?}");
        }
    }

    #[test]
    fn colleagues_statuses_are_read_from_other_phones_and_exchanges() {
        // Asterisk for an extension in a call: open, on the phone.
        let asterisk = "<?xml version=\"1.0\"?>\n<presence entity=\"sip:4010@pbx\" \
            xmlns=\"urn:ietf:params:xml:ns:pidf\" xmlns:pp=\"urn:ietf:params:xml:ns:pidf:person\" \
            xmlns:es=\"urn:ietf:params:xml:ns:pidf:rpid:status:rpid-status\" \
            xmlns:ep=\"urn:ietf:params:xml:ns:pidf:rpid:rpid-person\">\n \
            <pp:person><status><ep:activities><ep:on-the-phone/></ep:activities></status></pp:person>\n \
            <note>On the phone</note>\n <tuple id=\"4010\">\n  <contact priority=\"1\">sip:4010@pbx</contact>\n  \
            <status>\n   <basic>open</basic>\n  </status>\n </tuple>\n</presence>";
        let read = parse_pidf(asterisk).unwrap();
        assert!(read.online && read.on_the_phone);
        assert_eq!(read.note, "On the phone");
        // An offline phone.
        let closed = "<presence xmlns=\"urn:ietf:params:xml:ns:pidf\" entity=\"sip:x@y\">\
            <tuple id=\"a\"><status><basic>closed</basic></status></tuple></presence>";
        let read = parse_pidf(closed).unwrap();
        assert!(!read.online);
        assert_eq!(read.status, None);
        // Another softphone that only writes a note.
        let note = "<presence xmlns='urn:ietf:params:xml:ns:pidf' entity='sip:x@y'>\
            <tuple id='a'><status><basic>open</basic></status><note>Busy</note></tuple></presence>";
        assert_eq!(parse_pidf(note).unwrap().status, Some(PresenceStatus::Busy));
        assert_eq!(parse_pidf("Messages-Waiting: yes"), None);
    }

    #[test]
    fn a_status_is_published_refreshed_changed_and_taken_down() {
        let now = Instant::now();
        let mut publication =
            Publication::new(PresenceStatus::Away, "sip:4009@pbx", "10.0.0.5:5060", "s1");
        assert!(publication.wants_sending(now));
        let first = text(&publication.request(None, false).unwrap());
        assert!(first.starts_with("PUBLISH sip:4009@pbx SIP/2.0\r\n"));
        let lower = first.to_ascii_lowercase();
        assert!(lower.contains("event: presence\r\n"));
        assert!(lower.contains("expires: 3600\r\n"));
        assert!(lower.contains("content-type: application/pidf+xml\r\n"));
        assert!(!lower.contains("sip-if-match"));
        assert!(!publication.wants_sending(now));

        // The password is asked for once.
        let challenge = response(
            401,
            &[("Via", "SIP/2.0/UDP 10.0.0.5:5060"), ("CSeq", "1 PUBLISH")],
        );
        assert_eq!(
            publication.on_response(&challenge, now),
            PublishReply::Authorize
        );
        let with_password = text(
            &publication
                .request(Some(("authorization", "Digest x".to_owned())), false)
                .unwrap(),
        );
        assert!(
            with_password.contains("cseq: 2 PUBLISH") || with_password.contains("CSeq: 2 PUBLISH")
        );
        assert!(with_password.contains("application/pidf+xml"));

        let ok = response(200, &[("SIP-ETag", "etag-1"), ("Expires", "600")]);
        assert_eq!(publication.on_response(&ok, now), PublishReply::Published);
        assert!(publication.is_published());
        // Nothing until the refresh is due, at four fifths of the time.
        assert!(!publication.wants_sending(now + Duration::from_secs(470)));
        assert!(publication.wants_sending(now + Duration::from_secs(481)));
        let refresh = text(&publication.request(None, false).unwrap()).to_ascii_lowercase();
        assert!(refresh.contains("sip-if-match: etag-1\r\n"));
        assert!(!refresh.contains("pidf"), "a refresh has no document");
        let ok = response(200, &[("SIP-ETag", "etag-2")]);
        publication.on_response(&ok, now);

        // A new status goes at once, with its document.
        publication.set_status(PresenceStatus::Busy);
        assert!(publication.wants_sending(now));
        let change = text(&publication.request(None, false).unwrap());
        assert!(
            change
                .to_ascii_lowercase()
                .contains("sip-if-match: etag-2\r\n")
        );
        assert!(change.contains("<rpid:busy/>"));
        publication.on_response(&response(200, &[("SIP-ETag", "etag-3")]), now);

        // The server forgot it: everything again, without the old name.
        publication.set_status(PresenceStatus::Online);
        publication.request(None, false).unwrap();
        assert_eq!(
            publication.on_response(&response(412, &[]), now),
            PublishReply::Again
        );
        let again = text(&publication.request(None, false).unwrap()).to_ascii_lowercase();
        assert!(!again.contains("sip-if-match") && again.contains("pidf"));
        publication.on_response(&response(200, &[("SIP-ETag", "etag-4")]), now);

        // Taking it down: zero lifetime, the name, no document.
        let remove = text(&publication.request(None, true).unwrap()).to_ascii_lowercase();
        assert!(remove.contains("expires: 0\r\n") && remove.contains("sip-if-match: etag-4"));
        assert!(!remove.contains("pidf"));
        publication.on_response(&response(200, &[]), now);
        assert!(!publication.is_published());
    }

    #[test]
    fn an_exchange_that_does_not_take_statuses_is_left_alone() {
        let now = Instant::now();
        let mut publication = Publication::new(PresenceStatus::Away, "sip:4009@pbx", "h:1", "s2");
        publication.request(None, false).unwrap();
        let refused = publication.on_response(&response(489, &[]), now);
        assert!(matches!(refused, PublishReply::Refused(text) if text.starts_with("489")));
        // Not even much later.
        assert!(!publication.wants_sending(now + Duration::from_secs(10_000)));
        // A new status is tried once more.
        publication.set_status(PresenceStatus::Busy);
        assert!(publication.wants_sending(now));

        // A server error or silence is tried again after a while.
        publication.request(None, false).unwrap();
        publication.on_response(&response(503, &[]), now);
        assert!(!publication.wants_sending(now + Duration::from_secs(10)));
        assert!(publication.wants_sending(now + RETRY_DELAY));
        publication.request(None, false).unwrap();
        publication.on_timeout(now);
        assert!(publication.wants_sending(now + RETRY_DELAY));
    }
}
