//! What a telephone exchange tells its phones besides calls: whether there
//! is voicemail (RFC 3842, "message-summary") and whether colleagues are on
//! the phone (RFC 4235, "dialog", the busy lamp field of desk phones).
//!
//! Both come by subscription: the phone sends SUBSCRIBE for an event, the
//! server answers and then sends NOTIFY whenever something changes, until
//! the subscription runs out; the phone renews it before then.
//! [`Subscription`] is that exchange without the network, as `registration`
//! is for REGISTER; the executor sends what it makes and hands it what
//! arrives. The parsers read the bodies of the NOTIFY requests.

use crate::sip::message::{Method, Request, RequestBuilder, Response};
use crate::transaction_headers::TransactionHeaders;
use std::time::{Duration, Instant};

/// Lifetime asked for a subscription.
pub const SUBSCRIBE_EXPIRES: u32 = 600;
/// Pause before a failed subscription is tried again.
pub const RESUBSCRIBE_DELAY: Duration = Duration::from_secs(60);
/// The shortest pause between two renewals, whatever the server grants.
const MIN_REFRESH: Duration = Duration::from_secs(30);

/// What is subscribed to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Package {
    /// The account's own mailbox.
    Voicemail,
    /// The calls of another extension.
    Dialog,
}

impl Package {
    /// The value of the Event header.
    pub const fn event(self) -> &'static str {
        match self {
            Package::Voicemail => "message-summary",
            Package::Dialog => "dialog",
        }
    }

    const fn accept(self) -> &'static str {
        match self {
            Package::Voicemail => "application/simple-message-summary",
            Package::Dialog => "application/dialog-info+xml",
        }
    }
}

/// The state of the mailbox.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Voicemail {
    pub waiting: bool,
    /// Messages not listened to yet.
    pub new: u32,
    /// Messages already listened to and kept.
    pub old: u32,
}

/// Reads a message-summary body:
///
/// ```text
/// Messages-Waiting: yes
/// Voice-Message: 2/8 (0/2)
/// ```
pub fn parse_message_summary(body: &str) -> Option<Voicemail> {
    let mut waiting = None;
    let (mut new, mut old) = (0, 0);
    for line in body.lines() {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        match name.trim().to_ascii_lowercase().as_str() {
            "messages-waiting" => waiting = Some(value.eq_ignore_ascii_case("yes")),
            "voice-message" => {
                // "new/old", then urgent counts in brackets.
                let counts = value.split_whitespace().next().unwrap_or("");
                if let Some((first, second)) = counts.split_once('/') {
                    new = first.trim().parse().unwrap_or(0);
                    old = second.trim().parse().unwrap_or(0);
                }
            }
            _ => {}
        }
    }
    // Some servers send the counts without the flag.
    let waiting = waiting.unwrap_or(new > 0);
    Some(Voicemail { waiting, new, old })
}

/// What a watched extension is doing, as its lamp shows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineState {
    /// Nothing is known yet, or the server will not tell.
    Unknown,
    Idle,
    /// Someone is calling it.
    Ringing,
    /// On the phone, or dialling out.
    Busy,
}

/// Reads a dialog-info body. The busiest of its dialogs decides: a call in
/// progress over one that rings, a ringing one over none.
pub fn parse_dialog_info(body: &str) -> Option<LineState> {
    if !body.contains("dialog-info") {
        return None;
    }
    let mut state = LineState::Idle;
    // Each <dialog ...> element up to its end.
    for part in body.split("<dialog").skip(1) {
        // "<dialog-info" itself is not a dialog.
        if part.starts_with('-') {
            continue;
        }
        let opening = part.split('>').next().unwrap_or("");
        let incoming = opening.contains("direction=\"recipient\"")
            || opening.contains("direction='recipient'");
        let Some(text) = part
            .split("<state")
            .nth(1)
            .and_then(|rest| rest.split_once('>'))
            .map(|(_, rest)| rest.split('<').next().unwrap_or("").trim())
        else {
            continue;
        };
        let this = match text {
            "confirmed" => LineState::Busy,
            // Being called rings; calling out is already busy.
            "early" | "proceeding" | "trying" if incoming => LineState::Ringing,
            "early" | "proceeding" | "trying" => LineState::Busy,
            _ => LineState::Idle,
        };
        state = match (state, this) {
            (LineState::Busy, _) | (_, LineState::Busy) => LineState::Busy,
            (LineState::Ringing, _) | (_, LineState::Ringing) => LineState::Ringing,
            _ => LineState::Idle,
        };
    }
    Some(state)
}

/// What the executor has to do after a response to SUBSCRIBE.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reply {
    /// Nothing; still waiting, or accepted.
    Wait,
    /// The server wants the password: send again with credentials.
    Authorize,
    /// Refused; the text says why. It is tried again later.
    Failed(String),
}

/// One subscription to one event of one address.
#[derive(Debug)]
pub struct Subscription {
    package: Package,
    /// Whose events: `sip:4010@pbx`.
    target: String,
    /// This phone's account: `sip:4009@pbx`.
    aor: String,
    contact: String,
    via_sent_by: String,
    call_id_base: String,
    /// Each new dialog (after a refusal or the end) has its own Call-ID.
    round: u32,
    from_tag: String,
    to_tag: Option<String>,
    /// Where requests inside the dialog go: the server's Contact.
    remote_target: Option<String>,
    cseq: u32,
    branch_counter: u64,
    /// The request waiting for its answer.
    pending: Option<TransactionHeaders>,
    /// The credentials were already given for the pending request.
    authorized: bool,
    /// The server has accepted the subscription.
    active: bool,
    /// When to renew, or to try again.
    next: Option<Instant>,
    /// Ending: the last request asks for zero lifetime.
    ending: bool,
}

impl Subscription {
    /// `target` and `aor` are SIP addresses, `contact` this phone's Contact
    /// URI, `via_sent_by` its address, `seed` a string unique to this
    /// process.
    pub fn new(
        package: Package,
        target: impl Into<String>,
        aor: impl Into<String>,
        contact: impl Into<String>,
        via_sent_by: impl Into<String>,
        seed: &str,
    ) -> Self {
        Self {
            package,
            target: target.into(),
            aor: aor.into(),
            contact: contact.into(),
            via_sent_by: via_sent_by.into(),
            call_id_base: format!("sub-{seed}"),
            round: 0,
            from_tag: format!("sub-{seed}"),
            to_tag: None,
            remote_target: None,
            cseq: 0,
            branch_counter: 0,
            pending: None,
            authorized: false,
            active: false,
            next: None,
            ending: false,
        }
    }

    pub fn package(&self) -> Package {
        self.package
    }

    pub fn target(&self) -> &str {
        &self.target
    }

    /// The Request-URI the next request goes to, which the credentials
    /// must name.
    pub fn request_uri(&self) -> String {
        self.remote_target
            .clone()
            .unwrap_or_else(|| self.target.clone())
    }

    pub fn call_id(&self) -> String {
        format!("{}-{}", self.call_id_base, self.round)
    }

    pub fn is_active(&self) -> bool {
        self.active
    }

    /// The SUBSCRIBE to send now: the first, a renewal or a retry. With
    /// `authorization` it answers the server's challenge; with `end` it
    /// asks the server to stop.
    pub fn request(
        &mut self,
        authorization: Option<(&str, String)>,
        end: bool,
    ) -> Result<(Vec<u8>, Request), String> {
        self.cseq += 1;
        self.branch_counter += 1;
        self.ending = end;
        self.authorized = authorization.is_some();
        self.next = None;
        let branch = format!(
            "z9hG4bK-{}-{}-{}",
            self.from_tag, self.round, self.branch_counter
        );
        let to = match self.to_tag.as_ref() {
            Some(tag) => format!("<{}>;tag={tag}", self.target),
            None => format!("<{}>", self.target),
        };
        let uri = self
            .remote_target
            .clone()
            .unwrap_or_else(|| self.target.clone());
        let expires = if end { 0 } else { SUBSCRIBE_EXPIRES };
        let error = |error: crate::sip::message::SipMessageError| error.to_string();
        let mut builder = RequestBuilder::new(Method::Subscribe, uri)
            .header(
                "via",
                format!("SIP/2.0/UDP {};branch={branch};rport", self.via_sent_by),
            )
            .map_err(error)?
            .header("max-forwards", "70")
            .map_err(error)?
            .header("from", format!("<{}>;tag={}", self.aor, self.from_tag))
            .map_err(error)?
            .header("to", to)
            .map_err(error)?
            .header("call-id", self.call_id())
            .map_err(error)?
            .header("cseq", format!("{} SUBSCRIBE", self.cseq))
            .map_err(error)?
            .header("contact", format!("<{}>", self.contact))
            .map_err(error)?
            .header("event", self.package.event())
            .map_err(error)?
            .header("accept", self.package.accept())
            .map_err(error)?
            .header("expires", expires.to_string())
            .map_err(error)?;
        if let Some((name, value)) = authorization {
            builder = builder.header(name, value).map_err(error)?;
        }
        let request = builder.build().map_err(error)?;
        let headers = TransactionHeaders::from_message(&crate::sip::message::Message::Request(
            request.clone(),
        ))
        .map_err(|error| error.to_string())?;
        self.pending = Some(headers);
        let bytes = request.serialize().map_err(error)?;
        Ok((bytes, request))
    }

    /// True when `headers` belong to the answer of the pending SUBSCRIBE.
    pub fn is_answer(&self, headers: &TransactionHeaders) -> bool {
        self.pending
            .as_ref()
            .is_some_and(|pending| pending.matches_response(headers))
    }

    /// Takes the answer to the pending SUBSCRIBE.
    pub fn on_response(&mut self, response: &Response, now: Instant) -> Reply {
        let status = response.status_code;
        if status < 200 {
            return Reply::Wait;
        }
        if matches!(status, 401 | 407) && !self.authorized {
            return Reply::Authorize;
        }
        self.pending = None;
        if (200..300).contains(&status) {
            if self.ending {
                self.active = false;
                return Reply::Wait;
            }
            self.active = true;
            if let Some(tag) = response
                .headers
                .get("to")
                .and_then(|to| tag_of(to).map(str::to_owned))
            {
                self.to_tag = Some(tag);
            }
            if let Some(contact) = response.headers.get("contact").and_then(uri_of) {
                self.remote_target = Some(contact.to_owned());
            }
            let granted = response
                .headers
                .get("expires")
                .and_then(|value| value.trim().parse::<u64>().ok())
                .unwrap_or(u64::from(SUBSCRIBE_EXPIRES));
            // Renewed well before the end, never in a rush.
            let refresh = Duration::from_secs(granted * 4 / 5).max(MIN_REFRESH);
            self.next = Some(now + refresh);
            return Reply::Wait;
        }
        self.fail(now);
        Reply::Failed(format!("{status} {}", response.reason))
    }

    /// The SUBSCRIBE got no answer at all.
    pub fn on_timeout(&mut self, now: Instant) {
        self.pending = None;
        if !self.ending {
            self.fail(now);
        }
    }

    /// Starts afresh later, in a new dialog.
    fn fail(&mut self, now: Instant) {
        self.active = false;
        self.to_tag = None;
        self.remote_target = None;
        self.round += 1;
        self.cseq = 0;
        self.next = Some(now + RESUBSCRIBE_DELAY);
    }

    /// True when `request` is a NOTIFY of this subscription.
    pub fn is_notify_of(&self, request: &Request) -> bool {
        request.method.as_str() == "NOTIFY"
            && request.headers.get("call-id") == Some(self.call_id().as_str())
            && request
                .headers
                .get("to")
                .and_then(tag_of)
                .is_some_and(|tag| tag == self.from_tag)
    }

    /// Takes a NOTIFY of this subscription. A notice that the subscription
    /// is over makes it start again soon, as a new one.
    pub fn on_notify(&mut self, request: &Request, now: Instant) {
        if self.to_tag.is_none() {
            // The NOTIFY may come before the 200 to SUBSCRIBE.
            self.to_tag = request
                .headers
                .get("from")
                .and_then(tag_of)
                .map(str::to_owned);
        }
        let terminated = request
            .headers
            .get("subscription-state")
            .is_some_and(|state| state.trim().to_ascii_lowercase().starts_with("terminated"));
        if terminated && !self.ending {
            self.active = false;
            self.to_tag = None;
            self.remote_target = None;
            self.round += 1;
            self.cseq = 0;
            self.pending = None;
            self.next = Some(now + Duration::from_secs(2));
        }
    }

    /// True when a renewal or a retry is due.
    pub fn is_due(&self, now: Instant) -> bool {
        self.pending.is_none() && !self.ending && self.next.is_some_and(|due| now >= due)
    }
}

/// The tag parameter of a From or To value.
fn tag_of(value: &str) -> Option<&str> {
    value
        .split(';')
        .skip(1)
        .find_map(|parameter| parameter.trim().strip_prefix("tag="))
        .map(str::trim)
        .filter(|tag| !tag.is_empty())
}

/// The URI inside a Contact value: `<sip:a@b;x>;expires=1` gives `sip:a@b;x`.
fn uri_of(value: &str) -> Option<&str> {
    let value = value.trim();
    match value.split_once('<') {
        Some((_, rest)) => rest.split_once('>').map(|(uri, _)| uri),
        None => value.split(';').next(),
    }
    .map(str::trim)
    .filter(|uri| uri.starts_with("sip:") || uri.starts_with("sips:"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sip::message::{Message, ResponseBuilder};

    #[test]
    fn mailbox_summaries_are_read() {
        let summary = "Messages-Waiting: yes\r\nMessage-Account: sip:*97@pbx\r\n\
                       Voice-Message: 2/8 (0/2)\r\n";
        assert_eq!(
            parse_message_summary(summary),
            Some(Voicemail {
                waiting: true,
                new: 2,
                old: 8
            })
        );
        assert_eq!(
            parse_message_summary("Messages-Waiting: no\r\nVoice-Message: 0/3\r\n"),
            Some(Voicemail {
                waiting: false,
                new: 0,
                old: 3
            })
        );
        // Counts alone, odd letter case, no counts at all.
        let counts_only = parse_message_summary("voice-message: 1/0\n").unwrap();
        assert!(counts_only.waiting);
        assert_eq!(
            parse_message_summary("MESSAGES-WAITING: YES\n"),
            Some(Voicemail {
                waiting: true,
                new: 0,
                old: 0
            })
        );
        assert_eq!(parse_message_summary(""), Some(Voicemail::default()));
    }

    fn dialog_info(dialogs: &str) -> String {
        format!(
            "<?xml version=\"1.0\"?>\r\n<dialog-info xmlns=\"urn:ietf:params:xml:ns:dialog-info\" \
             version=\"3\" state=\"full\" entity=\"sip:4010@pbx\">{dialogs}</dialog-info>"
        )
    }

    #[test]
    fn line_states_are_read_from_dialog_info() {
        let dialog = |direction: &str, state: &str| {
            format!("<dialog id=\"1\" direction=\"{direction}\"><state>{state}</state></dialog>")
        };
        assert_eq!(parse_dialog_info(&dialog_info("")), Some(LineState::Idle));
        assert_eq!(
            parse_dialog_info(&dialog_info(&dialog("recipient", "early"))),
            Some(LineState::Ringing)
        );
        assert_eq!(
            parse_dialog_info(&dialog_info(&dialog("initiator", "early"))),
            Some(LineState::Busy)
        );
        assert_eq!(
            parse_dialog_info(&dialog_info(&dialog("recipient", "confirmed"))),
            Some(LineState::Busy)
        );
        assert_eq!(
            parse_dialog_info(&dialog_info(&dialog("recipient", "terminated"))),
            Some(LineState::Idle)
        );
        // Asterisk's way: attributes on <state>, a ringing call next to one
        // in progress (call waiting).
        let both = format!(
            "{}<dialog id=\"2\"><state event=\"x\">confirmed</state></dialog>",
            dialog("recipient", "early")
        );
        assert_eq!(
            parse_dialog_info(&dialog_info(&both)),
            Some(LineState::Busy)
        );
        // Not a dialog-info document.
        assert_eq!(parse_dialog_info("Messages-Waiting: yes"), None);
    }

    fn subscription() -> Subscription {
        Subscription::new(
            Package::Dialog,
            "sip:4010@pbx",
            "sip:4009@pbx",
            "sip:4009@192.0.2.7:5060",
            "192.0.2.7:5060",
            "seed",
        )
    }

    fn answer(request: &Request, status: u16, extra: &[(&str, &str)]) -> Response {
        let mut builder = ResponseBuilder::new(status, "Scripted");
        for name in ["via", "from", "call-id", "cseq"] {
            builder = builder
                .header(name, request.headers.get(name).unwrap())
                .unwrap();
        }
        builder = builder
            .header(
                "to",
                format!("{};tag=srv", request.headers.get("to").unwrap()),
            )
            .unwrap();
        for (name, value) in extra {
            builder = builder.header(name, *value).unwrap();
        }
        builder.build().unwrap()
    }

    fn headers_of(response: &Response) -> TransactionHeaders {
        TransactionHeaders::from_message(&Message::Response(response.clone())).unwrap()
    }

    #[test]
    fn a_subscription_is_made_with_a_password_and_renewed_in_time() {
        let mut sub = subscription();
        let now = Instant::now();
        let (bytes, first) = sub.request(None, false).unwrap();
        let text = String::from_utf8(bytes).unwrap().to_lowercase();
        assert!(
            text.starts_with("subscribe sip:4010@pbx sip/2.0\r\n"),
            "{text}"
        );
        assert!(text.contains("event: dialog\r\n"));
        assert!(text.contains("accept: application/dialog-info+xml\r\n"));
        assert!(text.contains("expires: 600\r\n"));
        assert!(text.contains("contact: <sip:4009@192.0.2.7:5060>\r\n"));

        // Challenged once: credentials go with the next request.
        let challenge = answer(&first, 401, &[]);
        assert!(sub.is_answer(&headers_of(&challenge)));
        assert_eq!(sub.on_response(&challenge, now), Reply::Authorize);
        let (_, second) = sub
            .request(Some(("authorization", "Digest x".to_owned())), false)
            .unwrap();
        assert_eq!(second.headers.get("cseq"), Some("2 SUBSCRIBE"));
        assert_eq!(second.headers.get("authorization"), Some("Digest x"));
        // A second challenge means the password is wrong: no endless loop.
        assert!(matches!(
            sub.on_response(&answer(&second, 401, &[]), now),
            Reply::Failed(_)
        ));

        // Accepted for 100 s: renewed after 80, in the same dialog.
        let mut sub = subscription();
        let (_, first) = sub.request(None, false).unwrap();
        let ok = answer(
            &first,
            200,
            &[("expires", "100"), ("contact", "<sip:4010@10.0.0.1:5060>")],
        );
        assert_eq!(sub.on_response(&ok, now), Reply::Wait);
        assert!(sub.is_active());
        assert!(!sub.is_due(now + Duration::from_secs(79)));
        assert!(sub.is_due(now + Duration::from_secs(80)));
        let (_, renewal) = sub.request(None, false).unwrap();
        assert_eq!(renewal.target, "sip:4010@10.0.0.1:5060");
        assert_eq!(renewal.headers.get("to"), Some("<sip:4010@pbx>;tag=srv"));
        assert_eq!(renewal.headers.get("call-id"), first.headers.get("call-id"));

        // Ending asks for zero lifetime and is never renewed.
        let (_, end) = sub.request(None, true).unwrap();
        assert_eq!(end.headers.get("expires"), Some("0"));
        sub.on_response(&answer(&end, 200, &[]), now);
        assert!(!sub.is_active());
        assert!(!sub.is_due(now + Duration::from_secs(10_000)));
    }

    #[test]
    fn refusal_and_silence_are_tried_again_later_in_a_new_dialog() {
        let now = Instant::now();
        let mut sub = subscription();
        let (_, first) = sub.request(None, false).unwrap();
        assert_eq!(
            sub.on_response(&answer(&first, 404, &[]), now),
            Reply::Failed("404 Scripted".to_owned())
        );
        assert!(!sub.is_due(now + RESUBSCRIBE_DELAY - Duration::from_secs(1)));
        assert!(sub.is_due(now + RESUBSCRIBE_DELAY));
        let (_, again) = sub.request(None, false).unwrap();
        assert_ne!(again.headers.get("call-id"), first.headers.get("call-id"));
        assert_eq!(again.headers.get("cseq"), Some("1 SUBSCRIBE"));

        sub.on_timeout(now);
        assert!(sub.is_due(now + RESUBSCRIBE_DELAY));
        // Provisional answers change nothing.
        let (_, third) = sub.request(None, false).unwrap();
        assert_eq!(sub.on_response(&answer(&third, 100, &[]), now), Reply::Wait);
        assert!(!sub.is_due(now + RESUBSCRIBE_DELAY * 2));
    }

    fn notify(sub: &Subscription, state: &str) -> Request {
        RequestBuilder::new(Method::Notify, "sip:4009@192.0.2.7:5060")
            .header("via", "SIP/2.0/UDP pbx;branch=z9hG4bK-n")
            .unwrap()
            .header("from", "<sip:4010@pbx>;tag=srv")
            .unwrap()
            .header("to", "<sip:4009@pbx>;tag=sub-seed")
            .unwrap()
            .header("call-id", sub.call_id())
            .unwrap()
            .header("cseq", "1 NOTIFY")
            .unwrap()
            .header("max-forwards", "70")
            .unwrap()
            .header("event", "dialog")
            .unwrap()
            .header("subscription-state", state)
            .unwrap()
            .build()
            .unwrap()
    }

    #[test]
    fn notifies_are_recognised_and_an_ended_subscription_starts_again() {
        let now = Instant::now();
        let mut sub = subscription();
        sub.request(None, false).unwrap();
        let active = notify(&sub, "active;expires=600");
        assert!(sub.is_notify_of(&active));
        sub.on_notify(&active, now);
        // The server's tag was learnt from the NOTIFY.
        let (_, renewal) = sub.request(None, false).unwrap();
        assert_eq!(renewal.headers.get("to"), Some("<sip:4010@pbx>;tag=srv"));

        // Another subscription's NOTIFY is not this one's.
        let other = subscription();
        let mut foreign = notify(&other, "active");
        foreign.headers.insert("call-id", "something-else").unwrap();
        assert!(!sub.is_notify_of(&foreign));

        let ended = notify(&sub, "terminated;reason=timeout");
        sub.on_notify(&ended, now);
        assert!(!sub.is_active());
        assert!(sub.is_due(now + Duration::from_secs(2)));
        let (_, fresh) = sub.request(None, false).unwrap();
        assert!(!fresh.headers.get("to").unwrap().contains("tag="));
    }

    #[test]
    fn tags_and_contacts_are_read() {
        assert_eq!(tag_of("<sip:a@b>;tag=x1"), Some("x1"));
        assert_eq!(tag_of("<sip:a@b>"), None);
        assert_eq!(
            uri_of("<sip:a@b;transport=tcp>;expires=60"),
            Some("sip:a@b;transport=tcp")
        );
        assert_eq!(uri_of("sip:a@b;expires=60"), Some("sip:a@b"));
        assert_eq!(uri_of("<mailto:a@b>"), None);
    }
}
