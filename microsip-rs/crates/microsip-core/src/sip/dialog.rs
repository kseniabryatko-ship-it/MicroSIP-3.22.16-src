use crate::sip::message::{Method, Request, RequestBuilder, SipMessageError};
use std::collections::HashSet;
use std::error::Error;
use std::fmt;

const MAX_CSEQ: u32 = (1_u32 << 31) - 1;

#[derive(Debug)]
pub enum DialogError {
    InvalidConfiguration(&'static str),
    InvalidBranch,
    ReusedBranch,
    InvalidState(DialogState),
    CseqExhausted,
    Message(SipMessageError),
}

impl fmt::Display for DialogError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidConfiguration(reason) => {
                write!(formatter, "Invalid dialog configuration: {reason}")
            }
            Self::InvalidBranch => formatter.write_str("Invalid RFC 3261 Via branch"),
            Self::ReusedBranch => formatter.write_str("Via branch has already been used"),
            Self::InvalidState(state) => {
                write!(formatter, "Operation is not allowed in state {state:?}")
            }
            Self::CseqExhausted => formatter.write_str("Dialog CSeq range is exhausted"),
            Self::Message(error) => error.fmt(formatter),
        }
    }
}

impl Error for DialogError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Message(error) => Some(error),
            _ => None,
        }
    }
}

impl From<SipMessageError> for DialogError {
    fn from(error: SipMessageError) -> Self {
        Self::Message(error)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DialogState {
    Confirmed,
    Terminating,
    Terminated,
}

/// Parameters for a direct UDP dialog after a successful INVITE.
/// Route sets and proxy routing are deliberately not supported here.
#[derive(Debug, Clone)]
pub struct DialogConfig {
    /// Remote Contact URI from the successful INVITE response.
    pub remote_target: String,
    /// Full local From value, including the local tag.
    pub local_from: String,
    /// Full remote To value from the response, including its tag.
    pub remote_to: String,
    pub call_id: String,
    /// Local UDP sent-by address, for example 127.0.0.1:5062.
    pub local_sent_by: String,
    /// Full local Contact header value.
    pub local_contact: String,
    pub invite_cseq: u32,
    pub invite_branch: String,
}

#[derive(Debug)]
pub struct EstablishedDialog {
    config: DialogConfig,
    state: DialogState,
    local_cseq: u32,
    used_branches: HashSet<String>,
}

impl EstablishedDialog {
    pub fn new(config: DialogConfig) -> Result<Self, DialogError> {
        validate_config(&config)?;

        let mut used_branches = HashSet::new();
        used_branches.insert(config.invite_branch.clone());
        let local_cseq = config.invite_cseq;

        Ok(Self {
            config,
            state: DialogState::Confirmed,
            local_cseq,
            used_branches,
        })
    }

    pub fn state(&self) -> DialogState {
        self.state
    }

    pub fn local_cseq(&self) -> u32 {
        self.local_cseq
    }

    /// Builds an ACK for the original successful INVITE.
    /// Retransmit the returned request unchanged if the 2xx repeats.
    pub fn build_success_ack(&mut self, branch: &str) -> Result<Request, DialogError> {
        self.require_confirmed()?;
        self.check_new_branch(branch)?;

        let request = self.build_request(Method::Ack, self.config.invite_cseq, branch)?;

        self.used_branches.insert(branch.to_owned());
        Ok(request)
    }

    /// Builds an INVITE inside the dialog (a re-INVITE, used for hold) and
    /// reserves its CSeq and branch. The caller adds the SDP body.
    pub fn build_reinvite(&mut self, branch: &str) -> Result<Request, DialogError> {
        self.require_confirmed()?;
        self.check_new_branch(branch)?;

        let cseq = self
            .local_cseq
            .checked_add(1)
            .filter(|number| *number <= MAX_CSEQ)
            .ok_or(DialogError::CseqExhausted)?;

        let request = self.build_request(Method::Invite, cseq, branch)?;

        self.local_cseq = cseq;
        self.used_branches.insert(branch.to_owned());
        Ok(request)
    }

    /// Builds a REFER inside the dialog (call transfer) and reserves its
    /// CSeq and branch. The caller adds Refer-To and Referred-By.
    pub fn build_refer(&mut self, branch: &str) -> Result<Request, DialogError> {
        self.require_confirmed()?;
        self.check_new_branch(branch)?;

        let cseq = self
            .local_cseq
            .checked_add(1)
            .filter(|number| *number <= MAX_CSEQ)
            .ok_or(DialogError::CseqExhausted)?;

        let request = self.build_request(Method::Refer, cseq, branch)?;

        self.local_cseq = cseq;
        self.used_branches.insert(branch.to_owned());
        Ok(request)
    }

    /// Builds the ACK for a successful re-INVITE that had this CSeq.
    pub fn build_reinvite_ack(&mut self, cseq: u32, branch: &str) -> Result<Request, DialogError> {
        self.require_confirmed()?;
        self.check_new_branch(branch)?;
        if cseq <= self.config.invite_cseq || cseq > self.local_cseq {
            return Err(DialogError::InvalidConfiguration(
                "no re-INVITE with this CSeq was sent",
            ));
        }

        let request = self.build_request(Method::Ack, cseq, branch)?;

        self.used_branches.insert(branch.to_owned());
        Ok(request)
    }

    /// Builds BYE and reserves its CSeq and branch.
    /// Retransmissions must reuse the returned request unchanged.
    pub fn build_bye(&mut self, branch: &str) -> Result<Request, DialogError> {
        self.require_confirmed()?;
        self.check_new_branch(branch)?;

        let cseq = self
            .local_cseq
            .checked_add(1)
            .filter(|number| *number <= MAX_CSEQ)
            .ok_or(DialogError::CseqExhausted)?;

        let request = self.build_request(Method::Bye, cseq, branch)?;

        self.local_cseq = cseq;
        self.used_branches.insert(branch.to_owned());
        self.state = DialogState::Terminating;
        Ok(request)
    }

    /// Call only after the networking layer has validated the BYE outcome.
    /// This method does not itself validate or receive a SIP response.
    pub fn finish_local_bye(&mut self) -> Result<(), DialogError> {
        if self.state != DialogState::Terminating {
            return Err(DialogError::InvalidState(self.state));
        }

        self.state = DialogState::Terminated;
        Ok(())
    }

    fn require_confirmed(&self) -> Result<(), DialogError> {
        if self.state == DialogState::Confirmed {
            Ok(())
        } else {
            Err(DialogError::InvalidState(self.state))
        }
    }

    fn check_new_branch(&self, branch: &str) -> Result<(), DialogError> {
        validate_branch(branch)?;
        if self.used_branches.contains(branch) {
            return Err(DialogError::ReusedBranch);
        }
        Ok(())
    }

    fn build_request(
        &self,
        method: Method,
        cseq: u32,
        branch: &str,
    ) -> Result<Request, DialogError> {
        let cseq_value = format!("{cseq} {}", method.as_str());
        let via = format!(
            "SIP/2.0/UDP {};branch={branch};rport",
            self.config.local_sent_by
        );

        Ok(
            RequestBuilder::new(method, self.config.remote_target.clone())
                .header("via", via)?
                .header("from", self.config.local_from.clone())?
                .header("to", self.config.remote_to.clone())?
                .header("call-id", self.config.call_id.clone())?
                .header("cseq", cseq_value)?
                .header("contact", self.config.local_contact.clone())?
                .header("max-forwards", "70")?
                .build()?,
        )
    }
}

fn validate_config(config: &DialogConfig) -> Result<(), DialogError> {
    if !config.remote_target.starts_with("sip:")
        || config.remote_target.len() <= 4
        || config.remote_target.chars().any(|character| {
            character.is_whitespace() || character.is_control() || matches!(character, '<' | '>')
        })
    {
        return Err(DialogError::InvalidConfiguration(
            "remote target must be a bare sip: URI",
        ));
    }

    if config.invite_cseq > MAX_CSEQ {
        return Err(DialogError::InvalidConfiguration(
            "INVITE CSeq must be less than 2^31",
        ));
    }

    validate_branch(&config.invite_branch)?;

    if config.call_id.is_empty()
        || config
            .call_id
            .chars()
            .any(|character| character.is_whitespace() || character.is_control())
    {
        return Err(DialogError::InvalidConfiguration("invalid Call-ID"));
    }

    if config
        .local_sent_by
        .parse::<std::net::SocketAddr>()
        .is_err()
    {
        return Err(DialogError::InvalidConfiguration(
            "local sent-by must be a numeric IP address and port",
        ));
    }

    validate_tagged_address(&config.local_from)?;
    validate_tagged_address(&config.remote_to)?;

    if config.local_contact.trim().is_empty() {
        return Err(DialogError::InvalidConfiguration("empty local Contact"));
    }

    // Use the existing builder to check all values for header injection.
    let mut headers = crate::sip::message::Headers::default();
    headers.insert("from", config.local_from.clone())?;
    headers.insert("to", config.remote_to.clone())?;
    headers.insert("contact", config.local_contact.clone())?;
    Ok(())
}

fn validate_tagged_address(value: &str) -> Result<(), DialogError> {
    // This limited implementation accepts name-addr without a display name.
    let Some(remainder) = value.strip_prefix("<sip:") else {
        return Err(DialogError::InvalidConfiguration(
            "address must use <sip:...>;tag=... form",
        ));
    };
    let Some((uri, parameters)) = remainder.split_once('>') else {
        return Err(DialogError::InvalidConfiguration("unclosed address"));
    };
    if uri.is_empty() || !parameters.starts_with(';') {
        return Err(DialogError::InvalidConfiguration("invalid tagged address"));
    }

    let mut tag_count = 0;
    for parameter in parameters.split(';').skip(1) {
        let Some((name, tag)) = parameter.trim().split_once('=') else {
            continue;
        };
        if name.trim().eq_ignore_ascii_case("tag") {
            if !is_token(tag) {
                return Err(DialogError::InvalidConfiguration("invalid dialog tag"));
            }
            tag_count += 1;
        }
    }

    if tag_count != 1 {
        return Err(DialogError::InvalidConfiguration(
            "address must contain exactly one tag",
        ));
    }
    Ok(())
}

fn validate_branch(branch: &str) -> Result<(), DialogError> {
    if branch.starts_with("z9hG4bK") && is_token(branch) {
        Ok(())
    } else {
        Err(DialogError::InvalidBranch)
    }
}

fn is_token(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-.!%*_+`'~".contains(&byte))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sip::message::Message;

    fn config() -> DialogConfig {
        DialogConfig {
            remote_target: "sip:bob@127.0.0.1:5070".to_owned(),
            local_from: "<sip:alice@local.test>;tag=alice".to_owned(),
            remote_to: "<sip:bob@local.test>;tag=bob".to_owned(),
            call_id: "dialog-test@local.test".to_owned(),
            local_sent_by: "127.0.0.1:5062".to_owned(),
            local_contact: "<sip:alice@127.0.0.1:5062>".to_owned(),
            invite_cseq: 7,
            invite_branch: "z9hG4bK-invite".to_owned(),
        }
    }

    #[test]
    fn ack_uses_ack_method_and_original_sequence() {
        let mut dialog = EstablishedDialog::new(config()).expect("valid dialog");
        let ack = dialog.build_success_ack("z9hG4bK-ack").expect("valid ACK");

        assert_eq!(ack.method, Method::Ack);
        assert_eq!(ack.target, "sip:bob@127.0.0.1:5070");
        assert_eq!(ack.headers.get("cseq"), Some("7 ACK"));
        assert_eq!(
            ack.headers.get("via"),
            Some("SIP/2.0/UDP 127.0.0.1:5062;branch=z9hG4bK-ack;rport")
        );
        assert_eq!(dialog.local_cseq(), 7);

        let bytes = ack.serialize().expect("ACK should serialize");
        let text = std::str::from_utf8(&bytes).expect("valid UTF-8");
        assert_eq!(
            Message::parse(text).expect("ACK should parse"),
            Message::Request(ack)
        );
    }

    #[test]
    fn refer_takes_the_next_cseq_and_leaves_the_dialog_open() {
        let mut dialog = EstablishedDialog::new(config()).expect("valid dialog");
        let first = dialog.local_cseq();
        let refer = dialog.build_refer("z9hG4bK-refer").expect("valid REFER");
        assert_eq!(refer.method, Method::Refer);
        assert_eq!(
            refer.headers.get("cseq"),
            Some(format!("{} REFER", first + 1).as_str())
        );
        assert_eq!(dialog.state(), DialogState::Confirmed);
        // The call can still be ended afterwards.
        let bye = dialog.build_bye("z9hG4bK-bye").expect("valid BYE");
        assert_eq!(
            bye.headers.get("cseq"),
            Some(format!("{} BYE", first + 2).as_str())
        );
        assert!(dialog.build_refer("z9hG4bK-late").is_err());
    }

    #[test]
    fn reinvite_takes_the_next_cseq_and_bye_follows_it() {
        let mut dialog = EstablishedDialog::new(config()).expect("valid dialog");
        let first = dialog.local_cseq();

        let reinvite = dialog
            .build_reinvite("z9hG4bK-hold")
            .expect("valid re-INVITE");
        assert_eq!(reinvite.method, Method::Invite);
        assert_eq!(
            reinvite.headers.get("cseq"),
            Some(format!("{} INVITE", first + 1).as_str())
        );
        assert!(reinvite.headers.get("contact").is_some());
        assert_eq!(dialog.state(), DialogState::Confirmed);

        let ack = dialog
            .build_reinvite_ack(first + 1, "z9hG4bK-hold-ack")
            .expect("valid ACK");
        assert_eq!(
            ack.headers.get("cseq"),
            Some(format!("{} ACK", first + 1).as_str())
        );
        // Only a CSeq that a re-INVITE really used can be acknowledged.
        assert!(dialog.build_reinvite_ack(first, "z9hG4bK-a").is_err());
        assert!(dialog.build_reinvite_ack(first + 2, "z9hG4bK-b").is_err());
        assert!(dialog.build_reinvite("z9hG4bK-hold").is_err());

        let bye = dialog.build_bye("z9hG4bK-bye").expect("valid BYE");
        assert_eq!(
            bye.headers.get("cseq"),
            Some(format!("{} BYE", first + 2).as_str())
        );
        assert!(dialog.build_reinvite("z9hG4bK-late").is_err());
    }

    #[test]
    fn bye_increments_cseq_and_changes_state() {
        let mut dialog = EstablishedDialog::new(config()).expect("valid dialog");
        let bye = dialog.build_bye("z9hG4bK-bye").expect("valid BYE");

        assert_eq!(bye.method, Method::Bye);
        assert_eq!(bye.headers.get("cseq"), Some("8 BYE"));
        assert_eq!(dialog.state(), DialogState::Terminating);
        assert!(dialog.build_bye("z9hG4bK-another").is_err());
        dialog
            .finish_local_bye()
            .expect("BYE outcome was validated externally");
        assert_eq!(dialog.state(), DialogState::Terminated);
    }

    #[test]
    fn rejects_reused_branch_without_changing_sequence() {
        let mut dialog = EstablishedDialog::new(config()).expect("valid dialog");
        assert!(matches!(
            dialog.build_success_ack("z9hG4bK-invite"),
            Err(DialogError::ReusedBranch)
        ));
        dialog.build_success_ack("z9hG4bK-ack").expect("valid ACK");
        assert!(matches!(
            dialog.build_bye("z9hG4bK-ack"),
            Err(DialogError::ReusedBranch)
        ));
        assert_eq!(dialog.local_cseq(), 7);
        assert_eq!(dialog.state(), DialogState::Confirmed);
    }

    #[test]
    fn rejects_header_injection() {
        let mut settings = config();
        settings.local_contact = "<sip:alice@local.test>\r\nInjected: yes".to_owned();
        assert!(EstablishedDialog::new(settings).is_err());
    }

    #[test]
    fn rejects_missing_remote_tag() {
        let mut settings = config();
        settings.remote_to = "<sip:bob@local.test>".to_owned();
        assert!(EstablishedDialog::new(settings).is_err());
    }

    #[test]
    fn rejects_cseq_overflow_without_changing_state() {
        let mut settings = config();
        settings.invite_cseq = MAX_CSEQ;
        let mut dialog = EstablishedDialog::new(settings).expect("valid dialog");
        assert!(matches!(
            dialog.build_bye("z9hG4bK-bye"),
            Err(DialogError::CseqExhausted)
        ));
        assert_eq!(dialog.state(), DialogState::Confirmed);
        assert_eq!(dialog.local_cseq(), MAX_CSEQ);
    }
}
