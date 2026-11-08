//! The message handling of a minimal multicast DNS responder (RFC 6762) for local site names.
//!
//! It answers only A and AAAA questions for its own names, with the local address, so browsers on the
//! LAN find `shop.local` without a port or router DNS. It does no service discovery, probing or
//! conflict resolution: the names come from the device's own sites, and a conflicting name on the LAN
//! shows up as two answers. The sockets live in the `nodo-mdns` binary.

use std::net::Ipv4Addr;

pub const PORT: u16 = 5353;
pub const GROUP: Ipv4Addr = Ipv4Addr::new(224, 0, 0, 251);
const TYPE_A: u16 = 1;
const TYPE_AAAA: u16 = 28;
const TYPE_NSEC: u16 = 47;
const TYPE_ANY: u16 = 255;
const CLASS_IN: u16 = 1;
const CACHE_FLUSH: u16 = 0x8000;
const UNICAST_RESPONSE: u16 = 0x8000;
/// RFC 6762 section 10: host address records use 120 seconds.
const TTL: u32 = 120;
/// RFC 6762 section 6.7: answers to legacy resolvers should use a TTL of at most 10 seconds.
const LEGACY_TTL: u32 = 10;
const MAX_POINTER_JUMPS: usize = 16;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Question {
    pub name: String,
    pub qtype: u16,
    pub unicast: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Query {
    pub id: u16,
    pub questions: Vec<Question>,
}

/// Where an answer must go.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reply {
    /// To the group, which keeps every cache on the link current.
    Multicast,
    /// To the asker's port 5353, because it asked for a unicast reply.
    Unicast,
    /// To a resolver that is not a full mDNS participant, which needs the ID and question echoed.
    Legacy,
}

/// Parses a query. Responses, other opcodes and malformed messages are `None`.
pub fn parse_query(message: &[u8]) -> Option<Query> {
    let header = message.get(..12)?;
    let id = u16::from_be_bytes([header[0], header[1]]);
    let flags = u16::from_be_bytes([header[2], header[3]]);
    // QR set means a response, and the opcode must be a standard query.
    if flags & 0x8000 != 0 || (flags >> 11) & 0xf != 0 {
        return None;
    }
    let count = u16::from_be_bytes([header[4], header[5]]);
    let mut offset = 12;
    let mut questions = Vec::new();
    for _ in 0..count.min(32) {
        let (name, next) = read_name(message, offset)?;
        let fixed = message.get(next..next + 4)?;
        let qtype = u16::from_be_bytes([fixed[0], fixed[1]]);
        let qclass = u16::from_be_bytes([fixed[2], fixed[3]]);
        offset = next + 4;
        if qclass & !UNICAST_RESPONSE == CLASS_IN {
            questions.push(Question { name, qtype, unicast: qclass & UNICAST_RESPONSE != 0 });
        }
    }
    Some(Query { id, questions })
}

/// Reads a possibly compressed name. Returns it lowercased, and the offset after it.
fn read_name(message: &[u8], start: usize) -> Option<(String, usize)> {
    let mut labels: Vec<String> = Vec::new();
    let mut offset = start;
    let mut end = None;
    let mut jumps = 0;
    loop {
        let len = *message.get(offset)? as usize;
        match len {
            0 => {
                let name = labels.join(".");
                return (name.len() <= 253).then(|| (name, end.unwrap_or(offset + 1)));
            }
            l if l & 0xc0 == 0xc0 => {
                let pointer = (l & 0x3f) << 8 | *message.get(offset + 1)? as usize;
                end.get_or_insert(offset + 2);
                jumps += 1;
                if jumps > MAX_POINTER_JUMPS {
                    return None;
                }
                offset = pointer;
            }
            l if l <= 63 => {
                let label = message.get(offset + 1..offset + 1 + l)?;
                labels.push(String::from_utf8_lossy(label).to_ascii_lowercase());
                offset += 1 + l;
            }
            _ => return None,
        }
    }
}

fn write_name(out: &mut Vec<u8>, name: &str) {
    for label in name.split('.') {
        out.push(label.len() as u8);
        out.extend_from_slice(label.as_bytes());
    }
    out.push(0);
}

pub struct Responder {
    pub address: Ipv4Addr,
    names: Vec<String>,
}

impl Responder {
    /// Keeps only valid `.local` names, lowercased.
    pub fn new(address: Ipv4Addr, names: &[String]) -> Self {
        let mut names: Vec<String> = names
            .iter()
            .map(|name| name.trim().to_ascii_lowercase())
            .filter(|name| crate::names::has_mdns_suffix(name))
            .collect();
        names.sort();
        names.dedup();
        Self { address, names }
    }

    pub fn names(&self) -> &[String] {
        &self.names
    }

    /// The answer to one query, or `None` when none of the questions is ours.
    /// `legacy` is true when the query did not come from port 5353.
    pub fn answer(&self, query: &Query, legacy: bool) -> Option<(Vec<u8>, Reply)> {
        let ours: Vec<&Question> = query
            .questions
            .iter()
            .filter(|q| matches!(q.qtype, TYPE_A | TYPE_AAAA | TYPE_ANY) && self.names.contains(&q.name))
            .collect();
        if ours.is_empty() {
            return None;
        }
        let reply = if legacy {
            Reply::Legacy
        } else if ours.iter().all(|q| q.unicast) {
            Reply::Unicast
        } else {
            Reply::Multicast
        };
        let mut out = Vec::with_capacity(128);
        let id = if legacy { query.id } else { 0 };
        let questions = if legacy { ours.len() as u16 } else { 0 };
        out.extend_from_slice(&id.to_be_bytes());
        out.extend_from_slice(&0x8400u16.to_be_bytes());
        out.extend_from_slice(&questions.to_be_bytes());
        out.extend_from_slice(&(ours.len() as u16).to_be_bytes());
        out.extend_from_slice(&[0, 0, 0, 0]);
        if legacy {
            for question in &ours {
                write_name(&mut out, &question.name);
                out.extend_from_slice(&question.qtype.to_be_bytes());
                out.extend_from_slice(&CLASS_IN.to_be_bytes());
            }
        }
        let (class, ttl) = if legacy { (CLASS_IN, LEGACY_TTL) } else { (CLASS_IN | CACHE_FLUSH, TTL) };
        for question in &ours {
            write_name(&mut out, &question.name);
            if question.qtype == TYPE_AAAA {
                // No IPv6 address: an NSEC record that lists only A says so, so clients stop waiting.
                out.extend_from_slice(&TYPE_NSEC.to_be_bytes());
                out.extend_from_slice(&class.to_be_bytes());
                out.extend_from_slice(&ttl.to_be_bytes());
                let mut rdata = Vec::new();
                write_name(&mut rdata, &question.name);
                rdata.extend_from_slice(&[0, 1, 0x40]);
                out.extend_from_slice(&(rdata.len() as u16).to_be_bytes());
                out.extend_from_slice(&rdata);
            } else {
                out.extend_from_slice(&TYPE_A.to_be_bytes());
                out.extend_from_slice(&class.to_be_bytes());
                out.extend_from_slice(&ttl.to_be_bytes());
                out.extend_from_slice(&4u16.to_be_bytes());
                out.extend_from_slice(&self.address.octets());
            }
        }
        Some((out, reply))
    }

    /// An unsolicited response with every name, sent at start so caches on the LAN update at once.
    pub fn announcement(&self) -> Option<Vec<u8>> {
        let query = Query {
            id: 0,
            questions: self.names.iter().map(|n| Question { name: n.clone(), qtype: TYPE_A, unicast: false }).collect(),
        };
        self.answer(&query, false).map(|(message, _)| message)
    }
}

/// A one-question A query, for `nodo-mdns --query`.
pub fn build_query(id: u16, name: &str) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&id.to_be_bytes());
    out.extend_from_slice(&[0, 0, 0, 1, 0, 0, 0, 0, 0, 0]);
    write_name(&mut out, name);
    out.extend_from_slice(&TYPE_A.to_be_bytes());
    out.extend_from_slice(&CLASS_IN.to_be_bytes());
    out
}

/// The A records in a response, as (name, address).
pub fn parse_answers(message: &[u8]) -> Vec<(String, Ipv4Addr)> {
    let parse = || -> Option<Vec<(String, Ipv4Addr)>> {
        let flags = u16::from_be_bytes([*message.get(2)?, *message.get(3)?]);
        if flags & 0x8000 == 0 {
            return None;
        }
        let questions = u16::from_be_bytes([*message.get(4)?, *message.get(5)?]);
        let answers = u16::from_be_bytes([*message.get(6)?, *message.get(7)?]);
        let mut offset = 12;
        for _ in 0..questions {
            offset = read_name(message, offset)?.1 + 4;
        }
        let mut found = Vec::new();
        for _ in 0..answers {
            let (name, next) = read_name(message, offset)?;
            let fixed = message.get(next..next + 10)?;
            let rtype = u16::from_be_bytes([fixed[0], fixed[1]]);
            let len = u16::from_be_bytes([fixed[8], fixed[9]]) as usize;
            let data = message.get(next + 10..next + 10 + len)?;
            if rtype == TYPE_A && len == 4 {
                found.push((name, Ipv4Addr::new(data[0], data[1], data[2], data[3])));
            }
            offset = next + 10 + len;
        }
        Some(found)
    };
    parse().unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOCAL: Ipv4Addr = Ipv4Addr::new(192, 168, 1, 60);

    fn responder() -> Responder {
        Responder::new(LOCAL, &["Shop.local".to_owned(), "pay.local".to_owned(), "x.lan".to_owned(), "bad name.local".to_owned()])
    }

    #[test]
    fn only_valid_local_names_are_kept() {
        assert_eq!(responder().names(), ["pay.local", "shop.local"]);
    }

    #[test]
    fn answers_its_own_names_case_insensitively() {
        let query = parse_query(&build_query(7, "SHOP.local")).unwrap();
        let (message, reply) = responder().answer(&query, false).unwrap();
        assert_eq!(reply, Reply::Multicast);
        assert_eq!(&message[..2], &[0, 0], "multicast responses carry ID 0");
        assert_eq!(parse_answers(&message), vec![("shop.local".to_owned(), LOCAL)]);
    }

    #[test]
    fn legacy_resolvers_get_their_id_question_and_a_short_ttl() {
        let query = parse_query(&build_query(0xbeef, "pay.local")).unwrap();
        let (message, reply) = responder().answer(&query, true).unwrap();
        assert_eq!(reply, Reply::Legacy);
        assert_eq!(&message[..2], &[0xbe, 0xef]);
        assert_eq!(u16::from_be_bytes([message[4], message[5]]), 1, "the question is echoed");
        assert_eq!(parse_answers(&message), vec![("pay.local".to_owned(), LOCAL)]);
        let ttl_at = message.len() - 4 - 2 - 4;
        assert_eq!(u32::from_be_bytes(message[ttl_at..ttl_at + 4].try_into().unwrap()), 10);
    }

    #[test]
    fn unicast_questions_get_unicast_replies() {
        let mut raw = build_query(1, "shop.local");
        let class_at = raw.len() - 2;
        raw[class_at] |= 0x80;
        let query = parse_query(&raw).unwrap();
        assert!(query.questions[0].unicast);
        assert_eq!(responder().answer(&query, false).unwrap().1, Reply::Unicast);
    }

    #[test]
    fn other_names_types_and_responses_are_ignored() {
        let r = responder();
        assert!(r.answer(&parse_query(&build_query(1, "router.local")).unwrap(), false).is_none());
        assert!(r.answer(&parse_query(&build_query(1, "x.lan")).unwrap(), false).is_none());
        let mut txt = build_query(1, "shop.local");
        let type_at = txt.len() - 4;
        txt[type_at + 1] = 16;
        assert!(r.answer(&parse_query(&txt).unwrap(), false).is_none());
        let (response, _) = r.answer(&parse_query(&build_query(1, "shop.local")).unwrap(), false).unwrap();
        assert!(parse_query(&response).is_none(), "responses are never answered, so two responders cannot loop");
    }

    #[test]
    fn aaaa_gets_a_negative_answer() {
        let mut raw = build_query(1, "shop.local");
        let type_at = raw.len() - 4;
        raw[type_at + 1] = 28;
        let (message, _) = responder().answer(&parse_query(&raw).unwrap(), false).unwrap();
        assert!(parse_answers(&message).is_empty());
        assert!(message.windows(2).any(|w| w == TYPE_NSEC.to_be_bytes()));
    }

    #[test]
    fn hostile_messages_do_not_panic() {
        let loop_pointer = [0, 1, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0xc0, 12, 0, 1, 0, 1];
        assert!(parse_query(&loop_pointer).is_none(), "a pointer loop is rejected");
        for len in 0..40 {
            let _ = parse_query(&build_query(1, "shop.local")[..len.min(28)]);
            let _ = parse_answers(&[0xff; 40][..len]);
        }
        let long_label = [0u8, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0x7f];
        assert!(parse_query(&long_label).is_none());
    }

    #[test]
    fn compressed_questions_are_read() {
        let mut raw = build_query(1, "shop.local");
        raw[5] = 2;
        // Second question: "pay" followed by a pointer to "local" in the first question.
        raw.extend_from_slice(&[3, b'p', b'a', b'y', 0xc0, 17, 0, 1, 0, 1]);
        let query = parse_query(&raw).unwrap();
        assert_eq!(query.questions[1].name, "pay.local");
        let (message, _) = responder().answer(&query, false).unwrap();
        assert_eq!(parse_answers(&message).len(), 2);
    }

    #[test]
    fn the_announcement_lists_every_name() {
        let names: Vec<String> = parse_answers(&responder().announcement().unwrap()).into_iter().map(|(n, _)| n).collect();
        assert_eq!(names, ["pay.local", "shop.local"]);
        assert!(Responder::new(LOCAL, &[]).announcement().is_none());
    }
}
