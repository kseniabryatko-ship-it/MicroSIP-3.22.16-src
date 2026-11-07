use std::fmt;
use std::time::{Duration, Instant};

pub const T1: Duration = Duration::from_millis(500);
pub const MAX_TIMER_A: Duration = Duration::from_secs(4);
pub const TIMER_D: Duration = Duration::from_secs(32);
pub const TIMER_F: Duration = Duration::from_secs(64);
pub const TIMER_H: Duration = Duration::from_secs(64);
pub const TIMER_J: Duration = Duration::from_secs(64);
pub const TIMER_K: Duration = Duration::from_secs(4);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientTransactionState {
    Trying,
    Proceeding,
    Completed,
    Terminated,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServerTransactionState {
    Trying,
    Proceeding,
    Completed,
    Terminated,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransactionError {
    InvalidMethod,
    InvalidBranch,
    ZeroCSeq,
    InvalidProvisionalStatus(u16),
    InvalidFinalStatus(u16),
    InvalidClientState {
        operation: &'static str,
        state: ClientTransactionState,
    },
    InvalidServerState {
        operation: &'static str,
        state: ServerTransactionState,
    },
}

impl fmt::Display for TransactionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidMethod => formatter.write_str("Invalid SIP method"),
            Self::InvalidBranch => formatter.write_str("Invalid SIP Via branch"),
            Self::ZeroCSeq => formatter.write_str("CSeq number must be greater than zero"),
            Self::InvalidProvisionalStatus(status) => {
                write!(formatter, "Invalid provisional SIP status code: {status}")
            }
            Self::InvalidFinalStatus(status) => {
                write!(formatter, "Invalid final SIP status code: {status}")
            }
            Self::InvalidClientState { operation, state } => {
                write!(
                    formatter,
                    "Cannot {operation} in client transaction state {state:?}"
                )
            }
            Self::InvalidServerState { operation, state } => {
                write!(
                    formatter,
                    "Cannot {operation} in server transaction state {state:?}"
                )
            }
        }
    }
}

impl std::error::Error for TransactionError {}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TransactionKey {
    branch: String,
    method: String,
}

impl TransactionKey {
    pub fn new(branch: &str, method: &str) -> Result<Self, TransactionError> {
        Ok(Self {
            branch: normalize_branch(branch)?,
            method: normalize_method(method)?,
        })
    }

    pub fn branch(&self) -> &str {
        &self.branch
    }

    pub fn method(&self) -> &str {
        &self.method
    }
}

#[derive(Debug, Clone)]
pub struct ClientTransaction {
    key: TransactionKey,
    cseq: u32,
    state: ClientTransactionState,
    started_at: Instant,
    response_deadline: Instant,
    next_retransmission_at: Option<Instant>,
    retransmission_delay: Duration,
    retransmission_count: u32,
    final_status: Option<u16>,
    ack_required: bool,
    ack_deadline: Option<Instant>,
    completed_deadline: Option<Instant>,
}

impl ClientTransaction {
    pub fn new(
        method: &str,
        cseq: u32,
        branch: &str,
        now: Instant,
    ) -> Result<Self, TransactionError> {
        if cseq == 0 {
            return Err(TransactionError::ZeroCSeq);
        }

        let key = TransactionKey::new(branch, method)?;

        let response_timeout = if key.method() == "INVITE" {
            TIMER_D
        } else {
            TIMER_F
        };

        Ok(Self {
            key,
            cseq,
            state: ClientTransactionState::Trying,
            started_at: now,
            response_deadline: now + response_timeout,
            next_retransmission_at: Some(now + T1),
            retransmission_delay: T1,
            retransmission_count: 0,
            final_status: None,
            ack_required: false,
            ack_deadline: None,
            completed_deadline: None,
        })
    }

    pub fn key(&self) -> &TransactionKey {
        &self.key
    }

    pub fn method(&self) -> &str {
        self.key.method()
    }

    pub fn branch(&self) -> &str {
        self.key.branch()
    }

    pub fn cseq(&self) -> u32 {
        self.cseq
    }

    pub fn state(&self) -> ClientTransactionState {
        self.state
    }

    pub fn started_at(&self) -> Instant {
        self.started_at
    }

    pub fn final_status(&self) -> Option<u16> {
        self.final_status
    }

    pub fn retransmission_count(&self) -> u32 {
        self.retransmission_count
    }

    pub fn ack_required(&self) -> bool {
        self.ack_required
    }

    pub fn record_provisional(&mut self, status: u16) -> Result<(), TransactionError> {
        if !(100..200).contains(&status) {
            return Err(TransactionError::InvalidProvisionalStatus(status));
        }

        match self.state {
            ClientTransactionState::Trying | ClientTransactionState::Proceeding => {}
            ClientTransactionState::Completed | ClientTransactionState::Terminated => {
                return Err(TransactionError::InvalidClientState {
                    operation: "record a provisional response",
                    state: self.state,
                });
            }
        }

        if status != 100 {
            self.state = ClientTransactionState::Proceeding;
            self.next_retransmission_at = None;
        }

        Ok(())
    }

    pub fn record_final(&mut self, status: u16, now: Instant) -> Result<(), TransactionError> {
        if !(200..=699).contains(&status) {
            return Err(TransactionError::InvalidFinalStatus(status));
        }

        match self.state {
            ClientTransactionState::Trying | ClientTransactionState::Proceeding => {}
            ClientTransactionState::Completed | ClientTransactionState::Terminated => {
                return Err(TransactionError::InvalidClientState {
                    operation: "record a final response",
                    state: self.state,
                });
            }
        }

        self.final_status = Some(status);
        self.state = ClientTransactionState::Completed;
        self.next_retransmission_at = None;

        self.ack_required = self.method() == "INVITE" && status >= 300;

        if self.ack_required {
            self.ack_deadline = Some(now + TIMER_H);
        } else {
            self.ack_deadline = None;
        }

        if self.method() != "INVITE" {
            self.completed_deadline = Some(now + TIMER_J);
        } else {
            self.completed_deadline = None;
        }

        Ok(())
    }

    pub fn record_ack(&mut self) -> Result<(), TransactionError> {
        if !self.ack_required {
            return Err(TransactionError::InvalidClientState {
                operation: "record an ACK",
                state: self.state,
            });
        }

        if self.state == ClientTransactionState::Terminated {
            return Ok(());
        }

        if self.state != ClientTransactionState::Completed {
            return Err(TransactionError::InvalidClientState {
                operation: "record an ACK",
                state: self.state,
            });
        }

        self.state = ClientTransactionState::Terminated;

        Ok(())
    }

    pub fn mark_ack_sent(&mut self) -> Result<(), TransactionError> {
        let successful_invite = self.method() == "INVITE"
            && self
                .final_status
                .is_some_and(|status| (200..300).contains(&status));

        if !successful_invite {
            return Err(TransactionError::InvalidClientState {
                operation: "mark a successful INVITE ACK as sent",
                state: self.state,
            });
        }

        if self.state == ClientTransactionState::Terminated {
            return Ok(());
        }

        if self.state != ClientTransactionState::Completed {
            return Err(TransactionError::InvalidClientState {
                operation: "mark a successful INVITE ACK as sent",
                state: self.state,
            });
        }

        self.state = ClientTransactionState::Terminated;

        Ok(())
    }

    pub fn retransmission_due(&self, now: Instant) -> bool {
        if !matches!(
            self.state,
            ClientTransactionState::Trying | ClientTransactionState::Proceeding
        ) {
            return false;
        }

        self.next_retransmission_at
            .is_some_and(|deadline| now >= deadline)
    }

    pub fn record_retransmission(&mut self, now: Instant) -> Option<Duration> {
        if !self.retransmission_due(now) {
            return None;
        }

        let current_delay = self.retransmission_delay;

        self.retransmission_count = self.retransmission_count.saturating_add(1);

        let doubled_delay = current_delay * 2;

        self.retransmission_delay = std::cmp::min(doubled_delay, MAX_TIMER_A);

        self.next_retransmission_at = Some(now + self.retransmission_delay);

        Some(current_delay)
    }

    pub fn response_timed_out(&self, now: Instant) -> bool {
        matches!(
            self.state,
            ClientTransactionState::Trying | ClientTransactionState::Proceeding
        ) && now >= self.response_deadline
    }

    pub fn ack_timed_out(&self, now: Instant) -> bool {
        self.ack_deadline.is_some_and(|deadline| now >= deadline)
    }

    pub fn completed_expired(&self, now: Instant) -> bool {
        self.completed_deadline
            .is_some_and(|deadline| now >= deadline)
    }

    pub fn expire_if_needed(&mut self, now: Instant) -> bool {
        if self.state == ClientTransactionState::Terminated {
            return false;
        }

        let should_expire =
            self.response_timed_out(now) || self.ack_timed_out(now) || self.completed_expired(now);

        if should_expire {
            self.state = ClientTransactionState::Terminated;
        }

        should_expire
    }
}

#[derive(Debug, Clone)]
pub struct ServerTransaction {
    key: TransactionKey,
    state: ServerTransactionState,
    final_status: Option<u16>,
    ack_required: bool,
    ack_deadline: Option<Instant>,
    next_response_retransmission_at: Option<Instant>,
    response_retransmission_delay: Duration,
    response_retransmission_count: u32,
}

impl ServerTransaction {
    pub fn new(method: &str, branch: &str) -> Result<Self, TransactionError> {
        let key = TransactionKey::new(branch, method)?;

        Ok(Self {
            key,
            state: ServerTransactionState::Trying,
            final_status: None,
            ack_required: false,
            ack_deadline: None,
            next_response_retransmission_at: None,
            response_retransmission_delay: T1,
            response_retransmission_count: 0,
        })
    }

    pub fn key(&self) -> &TransactionKey {
        &self.key
    }

    pub fn method(&self) -> &str {
        self.key.method()
    }

    pub fn branch(&self) -> &str {
        self.key.branch()
    }

    pub fn state(&self) -> ServerTransactionState {
        self.state
    }

    pub fn final_status(&self) -> Option<u16> {
        self.final_status
    }

    pub fn ack_required(&self) -> bool {
        self.ack_required
    }

    pub fn response_retransmission_count(&self) -> u32 {
        self.response_retransmission_count
    }

    pub fn record_provisional(&mut self, status: u16) -> Result<(), TransactionError> {
        if !(100..200).contains(&status) {
            return Err(TransactionError::InvalidProvisionalStatus(status));
        }

        match self.state {
            ServerTransactionState::Trying | ServerTransactionState::Proceeding => {}
            ServerTransactionState::Completed | ServerTransactionState::Terminated => {
                return Err(TransactionError::InvalidServerState {
                    operation: "record a provisional response",
                    state: self.state,
                });
            }
        }

        if status != 100 {
            self.state = ServerTransactionState::Proceeding;
        }

        Ok(())
    }

    pub fn record_final(&mut self, status: u16, now: Instant) -> Result<(), TransactionError> {
        if !(200..=699).contains(&status) {
            return Err(TransactionError::InvalidFinalStatus(status));
        }

        match self.state {
            ServerTransactionState::Trying | ServerTransactionState::Proceeding => {}
            ServerTransactionState::Completed | ServerTransactionState::Terminated => {
                return Err(TransactionError::InvalidServerState {
                    operation: "record a final response",
                    state: self.state,
                });
            }
        }

        self.final_status = Some(status);

        let successful_invite = self.method() == "INVITE" && (200..300).contains(&status);

        self.ack_required = successful_invite;

        if successful_invite {
            self.state = ServerTransactionState::Completed;
            self.ack_deadline = Some(now + TIMER_H);
            self.next_response_retransmission_at = Some(now + T1);
        } else {
            self.state = ServerTransactionState::Terminated;
            self.ack_deadline = None;
            self.next_response_retransmission_at = None;
        }

        Ok(())
    }

    pub fn record_ack(&mut self) -> Result<(), TransactionError> {
        if !self.ack_required {
            return Err(TransactionError::InvalidServerState {
                operation: "record an ACK",
                state: self.state,
            });
        }

        if self.state == ServerTransactionState::Terminated {
            return Ok(());
        }

        if self.state != ServerTransactionState::Completed {
            return Err(TransactionError::InvalidServerState {
                operation: "record an ACK",
                state: self.state,
            });
        }

        self.state = ServerTransactionState::Terminated;

        Ok(())
    }

    pub fn response_retransmission_due(&self, now: Instant) -> bool {
        self.state == ServerTransactionState::Completed
            && self
                .next_response_retransmission_at
                .is_some_and(|deadline| now >= deadline)
    }

    pub fn record_response_retransmission(&mut self, now: Instant) -> Option<Duration> {
        if !self.response_retransmission_due(now) {
            return None;
        }

        let current_delay = self.response_retransmission_delay;

        self.response_retransmission_count = self.response_retransmission_count.saturating_add(1);

        let doubled_delay = current_delay * 2;

        self.response_retransmission_delay = std::cmp::min(doubled_delay, MAX_TIMER_A);

        self.next_response_retransmission_at = Some(now + self.response_retransmission_delay);

        Some(current_delay)
    }

    pub fn ack_timed_out(&self, now: Instant) -> bool {
        self.ack_deadline.is_some_and(|deadline| now >= deadline)
    }

    pub fn expire_if_needed(&mut self, now: Instant) -> bool {
        if self.state == ServerTransactionState::Terminated {
            return false;
        }

        if self.ack_timed_out(now) {
            self.state = ServerTransactionState::Terminated;
            return true;
        }

        false
    }
}

fn normalize_method(method: &str) -> Result<String, TransactionError> {
    let normalized = method.trim().to_ascii_uppercase();

    let valid = !normalized.is_empty()
        && normalized
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_.!%*+`'~".contains(&byte));

    if valid {
        Ok(normalized)
    } else {
        Err(TransactionError::InvalidMethod)
    }
}

fn normalize_branch(branch: &str) -> Result<String, TransactionError> {
    let normalized = branch.trim();

    let valid = !normalized.is_empty()
        && !normalized
            .chars()
            .any(|character| character.is_whitespace() || character.is_control());

    if valid {
        Ok(normalized.to_owned())
    } else {
        Err(TransactionError::InvalidBranch)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_transaction_key() {
        let key = TransactionKey::new("z9hG4bK-transaction-001", "invite")
            .expect("valid transaction key");

        assert_eq!(key.method(), "INVITE");

        assert_eq!(key.branch(), "z9hG4bK-transaction-001");

        assert!(matches!(
            TransactionKey::new("bad branch", "INVITE"),
            Err(TransactionError::InvalidBranch)
        ));

        assert!(matches!(
            TransactionKey::new("z9hG4bK-test", "bad method"),
            Err(TransactionError::InvalidMethod)
        ));
    }

    #[test]
    fn provisional_response_stops_retransmission() {
        let now = Instant::now();

        let mut transaction = ClientTransaction::new("INVITE", 1, "z9hG4bK-provisional", now)
            .expect("valid client transaction");

        assert!(transaction.retransmission_due(now + T1));

        transaction
            .record_provisional(180)
            .expect("provisional response should be accepted");

        assert_eq!(transaction.state(), ClientTransactionState::Proceeding);

        assert!(!transaction.retransmission_due(now + Duration::from_secs(10)));
    }

    #[test]
    fn failed_invite_requires_ack() {
        let now = Instant::now();

        let mut transaction = ClientTransaction::new("INVITE", 1, "z9hG4bK-failed", now)
            .expect("valid client transaction");

        transaction
            .record_final(486, now)
            .expect("final response should be accepted");

        assert_eq!(transaction.state(), ClientTransactionState::Completed);

        assert!(transaction.ack_required());

        transaction
            .record_ack()
            .expect("ACK should terminate the transaction");

        assert_eq!(transaction.state(), ClientTransactionState::Terminated);
    }

    #[test]
    fn successful_invite_terminates_after_ack_sent() {
        let now = Instant::now();

        let mut transaction = ClientTransaction::new("INVITE", 1, "z9hG4bK-successful", now)
            .expect("valid client transaction");

        transaction
            .record_final(200, now)
            .expect("final response should be accepted");

        assert!(!transaction.ack_required());

        transaction
            .mark_ack_sent()
            .expect("successful INVITE ACK should be accepted");

        assert_eq!(transaction.state(), ClientTransactionState::Terminated);
    }

    #[test]
    fn non_invite_transaction_expires() {
        let now = Instant::now();

        let mut transaction = ClientTransaction::new("OPTIONS", 1, "z9hG4bK-options", now)
            .expect("valid client transaction");

        transaction
            .record_final(200, now)
            .expect("final response should be accepted");

        assert!(!transaction.ack_required());

        assert!(!transaction.expire_if_needed(now + Duration::from_secs(63)));

        assert!(transaction.expire_if_needed(now + Duration::from_secs(65)));

        assert_eq!(transaction.state(), ClientTransactionState::Terminated);
    }

    #[test]
    fn client_retransmission_timer_doubles() {
        let now = Instant::now();

        let mut transaction = ClientTransaction::new("INVITE", 1, "z9hG4bK-timer", now)
            .expect("valid client transaction");

        let first_deadline = now + T1;

        assert!(transaction.retransmission_due(first_deadline));

        let first_delay = transaction
            .record_retransmission(first_deadline)
            .expect("retransmission should be recorded");

        assert_eq!(first_delay, T1);

        let second_deadline = first_deadline + T1 * 2;

        assert!(transaction.retransmission_due(second_deadline));

        let second_delay = transaction
            .record_retransmission(second_deadline)
            .expect("second retransmission should be recorded");

        assert_eq!(second_delay, T1 * 2);
    }

    #[test]
    fn client_response_timer_expires_transaction() {
        let now = Instant::now();

        let mut transaction = ClientTransaction::new("INVITE", 1, "z9hG4bK-timeout", now)
            .expect("valid client transaction");

        assert!(transaction.expire_if_needed(now + Duration::from_secs(33)));

        assert_eq!(transaction.state(), ClientTransactionState::Terminated);
    }

    #[test]
    fn server_retransmits_success_and_waits_for_ack() {
        let now = Instant::now();

        let mut transaction =
            ServerTransaction::new("INVITE", "z9hG4bK-server").expect("valid server transaction");

        transaction
            .record_final(200, now)
            .expect("final response should be accepted");

        assert_eq!(transaction.state(), ServerTransactionState::Completed);

        assert!(transaction.ack_required());

        let first_deadline = now + T1;

        assert!(transaction.response_retransmission_due(first_deadline));

        let delay = transaction
            .record_response_retransmission(first_deadline)
            .expect("response retransmission should be recorded");

        assert_eq!(delay, T1);

        transaction
            .record_ack()
            .expect("ACK should terminate the transaction");

        assert_eq!(transaction.state(), ServerTransactionState::Terminated);
    }

    #[test]
    fn non_invite_server_transaction_terminates_on_final() {
        let now = Instant::now();

        let mut transaction = ServerTransaction::new("OPTIONS", "z9hG4bK-server-options")
            .expect("valid server transaction");

        transaction
            .record_final(200, now)
            .expect("final response should be accepted");

        assert!(!transaction.ack_required());

        assert_eq!(transaction.state(), ServerTransactionState::Terminated);
    }
}
