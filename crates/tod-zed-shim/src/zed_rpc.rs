//! Just enough of Zed's remote-server framing to tell heartbeats from real
//! traffic and to answer a ping: each frame is a u32 little-endian length and
//! a protobuf `Envelope` (`id` = 1, `responding_to` = 2, the payload a oneof).

/// `Envelope` payload fields.
pub const ACK: u64 = 5;
pub const PING: u64 = 7;
/// Fields that are not the payload (`original_sender_id`, `ack_id`).
const NOT_PAYLOAD: [u64; 2] = [3, 266];

pub struct Head {
    pub id: u32,
    pub responding_to: Option<u32>,
    pub payload: Option<u64>,
}

impl Head {
    pub fn is_heartbeat(&self) -> bool {
        matches!(self.payload, Some(PING) | Some(ACK))
    }
}

fn varint(b: &[u8], i: &mut usize) -> Option<u64> {
    let mut v = 0u64;
    for shift in 0..10 {
        let byte = *b.get(*i)?;
        *i += 1;
        v |= ((byte & 0x7f) as u64) << (7 * shift);
        if byte & 0x80 == 0 {
            return Some(v);
        }
    }
    None
}

pub fn envelope_head(b: &[u8]) -> Head {
    let mut head = Head { id: 0, responding_to: None, payload: None };
    let mut i = 0usize;
    while i < b.len() {
        let Some(key) = varint(b, &mut i) else { break };
        let (field, wire) = (key >> 3, key & 7);
        match wire {
            0 => {
                let v = varint(b, &mut i).unwrap_or(0) as u32;
                match field {
                    1 => head.id = v,
                    2 => head.responding_to = Some(v),
                    _ => {}
                }
            }
            2 => {
                let n = varint(b, &mut i).unwrap_or(0) as usize;
                i += n;
                if !NOT_PAYLOAD.contains(&field) && head.payload.is_none() {
                    head.payload = Some(field);
                }
            }
            1 => i += 8,
            5 => i += 4,
            _ => break,
        }
    }
    head
}

fn push_varint(o: &mut Vec<u8>, mut v: u64) {
    loop {
        let b = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            o.push(b);
            break;
        }
        o.push(b | 0x80);
    }
}

/// A framed `Envelope { id, responding_to, ack: {} }`.
pub fn ack_frame(id: u32, responding_to: u32) -> Vec<u8> {
    let mut body = vec![0x08];
    push_varint(&mut body, id as u64);
    body.push(0x10);
    push_varint(&mut body, responding_to as u64);
    body.extend_from_slice(&[(ACK as u8) << 3 | 2, 0x00]);
    let mut frame = (body.len() as u32).to_le_bytes().to_vec();
    frame.extend(body);
    frame
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ack_round_trips() {
        let f = ack_frame(300, 7);
        assert_eq!(u32::from_le_bytes(f[..4].try_into().unwrap()) as usize, f.len() - 4);
        let h = envelope_head(&f[4..]);
        assert_eq!((h.id, h.responding_to, h.payload), (300, Some(7), Some(ACK)));
        assert!(h.is_heartbeat());
    }

    #[test]
    fn ping_is_a_heartbeat_and_other_payloads_are_not() {
        // id=1, ping {}
        let ping = [0x08, 0x01, (PING as u8) << 3 | 2, 0x00];
        assert!(envelope_head(&ping).is_heartbeat());
        // id=2, original_sender_id (3) then field 20 payload
        let other = [0x08, 0x02, 3 << 3 | 2, 0x00, 0xa2, 0x01, 0x00];
        let h = envelope_head(&other);
        assert_eq!(h.payload, Some(20));
        assert!(!h.is_heartbeat());
    }
}
