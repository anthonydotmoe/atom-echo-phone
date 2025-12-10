use std::net::SocketAddr;
use std::time::{Duration, Instant};

use crate::{header_value, Request, Response};

// Timer values from RFC 3261 (assuming UDP/unreliable transport)
const T1: Duration = Duration::from_millis(500);
const T2: Duration = Duration::from_secs(4);
const TIMER_H: Duration = Duration::from_millis(500 * 64); // 64 * T1
const TIMER_I: Duration = Duration::from_secs(5); // Time to keep transaction after ACK

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InviteServerTxState {
    Proceeding,
    Completed,
    Confirmed,
}

#[derive(Debug, Clone)]
struct InviteServerTransaction {
    call_id: String,
    cseq: u32,
    remote: SocketAddr,
    last_response: Option<Response>,
    state: InviteServerTxState,
    timer_g_interval: Duration,
    next_timer_g: Option<Instant>,
    deadline_h: Option<Instant>,
    deadline_i: Option<Instant>,
}

impl InviteServerTransaction {
    fn new(call_id: &str, cseq: u32, remote: SocketAddr) -> Self {
        Self {
            call_id: call_id.to_string(),
            cseq,
            remote,
            last_response: None,
            state: InviteServerTxState::Proceeding,
            timer_g_interval: T1,
            next_timer_g: None,
            deadline_h: None,
            deadline_i: None,
        }
    }

    fn matches(&self, call_id: &str, cseq: u32) -> bool {
        self.call_id == call_id && self.cseq == cseq
    }

    fn update_with_response(&mut self, resp: &Response, now: Instant) {
        let status = resp.status_code;
        self.last_response = Some(resp.clone());

        // Provisional response -> stay in Proceeding, no timers.
        if status < 200 {
            return;
        }

        // Final response -> start retransmission timers
        self.state = InviteServerTxState::Completed;
        self.timer_g_interval = T1;
        self.next_timer_g = Some(now + self.timer_g_interval);
        self.deadline_h = Some(now + TIMER_H);
        self.deadline_i = None;
    }

    fn on_ack(&mut self, now: Instant) {
        // ACK stops retransmissions; keep transaction briefly (Timer I)
        self.state = InviteServerTxState::Confirmed;
        self.next_timer_g = None;
        self.deadline_i = Some(now + TIMER_I);
    }

    fn maybe_retransmit(&mut self, now: Instant) -> Option<Response> {
        if self.state != InviteServerTxState::Completed {
            return None;
        }

        let Some(deadline_h) = self.deadline_h else {
            return None;
        };

        if now >= deadline_h {
            // Give up waiting for ACK.
            self.next_timer_g = None;
            return None;
        }

        let Some(next) = self.next_timer_g else {
            return None;
        };

        if now < next {
            return None;
        }

        // Send the last response again, backoff timer G (max T2)
        if let Some(resp) = &self.last_response {
            let out = resp.clone();
            let new_interval = (self.timer_g_interval * 2).min(T2);
            self.timer_g_interval = new_interval;
            self.next_timer_g = Some(now + self.timer_g_interval);
            return Some(out);
        }

        None
    }

    fn expired(&self, now: Instant) -> bool {
        match self.state {
            InviteServerTxState::Proceeding => false,
            InviteServerTxState::Completed => self
                .deadline_h
                .map(|h| now >= h)
                .unwrap_or(false),
            InviteServerTxState::Confirmed => self
                .deadline_i
                .map(|i| now >= i)
                .unwrap_or(false),
        }
    }
}

#[derive(Debug, Default)]
pub struct InviteServerTransactionManager {
    transactions: Vec<InviteServerTransaction>,
}

impl InviteServerTransactionManager {
    pub fn new() -> Self {
        Self {
            transactions: Vec::new(),
        }
    }

    /// Handle an incoming INVITE request. If it is a retransmission,
    /// return the last response to be resent.
    pub fn on_invite(
        &mut self,
        req: &Request,
        remote: SocketAddr,
    ) -> Option<Response> {
        let call_id = header_value(&req.headers, "Call-ID")?;
        let cseq = parse_cseq_number(header_value(&req.headers, "CSeq")?)?;

        // Look for existing transaction
        if let Some(tx) = self
            .transactions
            .iter()
            .find(|t| t.matches(call_id, cseq))
        {
            return tx.last_response.clone();
        }

        // New transaction
        self.transactions
            .push(InviteServerTransaction::new(call_id, cseq, remote));
        None
    }

    /// Record that we sent a response so the manager can retransmit it later.
    pub fn on_outgoing_response(
        &mut self,
        resp: &Response,
        remote: SocketAddr,
        now: Instant,
    ) {
        // Only track responses to INVITE
        let cseq_header = match header_value(&resp.headers, "CSeq") {
            Some(v) => v,
            None => return,
        };
        let Some(cseq_num) = parse_cseq_number(cseq_header) else { return; };
        let Some(cseq_method) = parse_cseq_method(cseq_header) else { return; };
        if cseq_method != "INVITE" {
            return;
        }

        let call_id = match header_value(&resp.headers, "Call-ID") {
            Some(v) => v,
            None => return,
        };

        let tx = self
            .transactions
            .iter_mut()
            .find(|t| t.matches(call_id, cseq_num));

        match tx {
            Some(t) => t.update_with_response(resp, now),
            None => {
                // If we somehow send a response without seeing the INVITE first,
                // start tracking now.
                let mut t = InviteServerTransaction::new(call_id, cseq_num, remote);
                t.update_with_response(resp, now);
                self.transactions.push(t);
            }
        }
    }

    pub fn on_ack(&mut self, ack: &Request, now: Instant) {
        let call_id = match header_value(&ack.headers, "Call-ID") {
            Some(v) => v,
            None => return,
        };
        let cseq = match header_value(&ack.headers, "CSeq")
            .and_then(parse_cseq_number)
        {
            Some(v) => v,
            None => return,
        };

        if let Some(tx) = self
            .transactions
            .iter_mut()
            .find(|t| t.matches(call_id, cseq))
        {
            tx.on_ack(now);
        }
    }

    /// Advance timers and produce any retransmissions that should be sent now.
    pub fn poll(&mut self, now: Instant) -> Vec<(Response, SocketAddr)> {
        let mut out = Vec::new();

        for tx in &mut self.transactions {
            if let Some(resp) = tx.maybe_retransmit(now) {
                out.push((resp, tx.remote));
            }
        }

        self.transactions
            .retain(|tx| !tx.expired(now));

        out
    }
}

fn parse_cseq_number(cseq: &str) -> Option<u32> {
    cseq.split_whitespace()
        .next()
        .and_then(|n| n.parse::<u32>().ok())
}

fn parse_cseq_method(cseq: &str) -> Option<&str> {
    cseq.split_whitespace().nth(1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Header, Method};
    use std::str::FromStr;

    fn sample_invite() -> Request {
        let mut req = Request::new(Method::Invite, "sip:alice@example.com").unwrap();
        req.add_header(Header::new("Via", "SIP/2.0/UDP 192.0.2.10:5060;branch=z9hG4bK1").unwrap()).unwrap();
        req.add_header(Header::new("From", "<sip:bob@example.com>;tag=from1").unwrap()).unwrap();
        req.add_header(Header::new("To", "<sip:alice@example.com>").unwrap()).unwrap();
        req.add_header(Header::new("Call-ID", "call123").unwrap()).unwrap();
        req.add_header(Header::new("CSeq", "1 INVITE").unwrap()).unwrap();
        req.add_header(Header::new("Content-Length", "0").unwrap()).unwrap();
        req
    }

    fn sample_response(status: u16) -> Response {
        let mut resp = Response::new(status, "OK").unwrap();
        resp.add_header(Header::new("Via", "SIP/2.0/UDP 192.0.2.10:5060;branch=z9hG4bK1").unwrap());
        resp.add_header(Header::new("From", "<sip:bob@example.com>;tag=from1").unwrap());
        resp.add_header(Header::new("To", "<sip:alice@example.com>;tag=to1").unwrap());
        resp.add_header(Header::new("Call-ID", "call123").unwrap());
        resp.add_header(Header::new("CSeq", "1 INVITE").unwrap());
        resp.add_header(Header::new("Content-Length", "0").unwrap());
        resp
    }

    fn sample_ack() -> Request {
        let mut req = Request::new(Method::Ack, "sip:alice@example.com").unwrap();
        req.add_header(Header::new("Via", "SIP/2.0/UDP 192.0.2.10:5060;branch=z9hG4bKack").unwrap()).unwrap();
        req.add_header(Header::new("From", "<sip:bob@example.com>;tag=from1").unwrap()).unwrap();
        req.add_header(Header::new("To", "<sip:alice@example.com>;tag=to1").unwrap()).unwrap();
        req.add_header(Header::new("Call-ID", "call123").unwrap()).unwrap();
        req.add_header(Header::new("CSeq", "1 ACK").unwrap()).unwrap();
        req.add_header(Header::new("Content-Length", "0").unwrap()).unwrap();
        req
    }

    #[test]
    fn retransmits_final_response_until_ack() {
        let mut mgr = InviteServerTransactionManager::new();
        let base = Instant::now();
        let remote = SocketAddr::from_str("192.0.2.10:5060").unwrap();
        let invite = sample_invite();

        // First INVITE starts transaction
        assert!(mgr.on_invite(&invite, remote).is_none());

        // Final response arms timers
        let resp = sample_response(200);
        mgr.on_outgoing_response(&resp, remote, base);

        // Before T1: no retransmission
        assert!(mgr.poll(base + Duration::from_millis(100)).is_empty());

        // At T1: one retransmission
        let events = mgr.poll(base + T1);
        assert_eq!(events.len(), 1);

        // ACK stops further retransmissions
        let ack = sample_ack();
        mgr.on_ack(&ack, base + Duration::from_secs(1));
        assert!(mgr.poll(base + Duration::from_secs(2)).is_empty());
    }

    #[test]
    fn responds_to_retransmitted_invite_with_last_response() {
        let mut mgr = InviteServerTransactionManager::new();
        let remote = SocketAddr::from_str("192.0.2.10:5060").unwrap();
        let invite = sample_invite();
        assert!(mgr.on_invite(&invite, remote).is_none());

        let resp = sample_response(180);
        mgr.on_outgoing_response(&resp, remote, Instant::now());

        let retrans = mgr.on_invite(&invite, remote);
        assert!(retrans.is_some());
        assert_eq!(retrans.unwrap().status_code, 180);
    }
}
