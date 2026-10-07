//! Bounded metadata for the requester's own messages, in request order.
pub const MAX: usize = 32;
pub type Key = ([u8; 32], [u8; 16]);
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Order {
    pub seq: i64,
    pub timestamp_ms: i64,
}
pub fn build_keys(keys: &[Key]) -> Option<Vec<u8>> {
    if keys.is_empty() || keys.len() > MAX {
        return None;
    }
    let mut p = vec![keys.len() as u8];
    for (sender, mid) in keys {
        p.extend_from_slice(sender);
        p.extend_from_slice(mid);
    }
    Some(p)
}
pub fn parse_keys(p: &[u8]) -> Option<Vec<Key>> {
    let count = usize::from(*p.first()?);
    if count == 0 || count > MAX || p.len() != 1 + count * 48 {
        return None;
    }
    p[1..]
        .chunks_exact(48)
        .map(|e| Some((e[..32].try_into().ok()?, e[32..].try_into().ok()?)))
        .collect()
}
pub fn build_orders(orders: &[Option<Order>]) -> Option<Vec<u8>> {
    if orders.is_empty() || orders.len() > MAX {
        return None;
    }
    let mut p = vec![orders.len() as u8];
    for order in orders {
        let (seq, seconds) = match order {
            Some(o) if o.seq > 0 && o.timestamp_ms >= 0 && o.timestamp_ms % 1000 == 0 => {
                (o.seq as u64, (o.timestamp_ms / 1000) as u64)
            }
            Some(_) => return None,
            None => (0, 0),
        };
        p.extend_from_slice(&seq.to_be_bytes());
        p.extend_from_slice(&seconds.to_be_bytes());
    }
    Some(p)
}
pub fn parse_orders(p: &[u8], expected: usize) -> Option<Vec<Option<Order>>> {
    if expected == 0
        || expected > MAX
        || usize::from(*p.first()?) != expected
        || p.len() != 1 + expected * 16
    {
        return None;
    }
    p[1..]
        .chunks_exact(16)
        .map(|e| {
            let seq = u64::from_be_bytes(e[..8].try_into().ok()?);
            let seconds = u64::from_be_bytes(e[8..].try_into().ok()?);
            if seq == 0 {
                return if seconds == 0 { Some(None) } else { None };
            }
            Some(Some(Order {
                seq: i64::try_from(seq).ok()?,
                timestamp_ms: i64::try_from(seconds.checked_mul(1000)?).ok()?,
            }))
        })
        .collect()
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_bounded_and_checked() {
        let keys = vec![([4; 32], [9; 16]); MAX];
        let p = build_keys(&keys).unwrap();
        assert_eq!(parse_keys(&p), Some(keys));
        for end in 0..p.len() {
            assert!(parse_keys(&p[..end]).is_none());
        }
        assert!(build_keys(&[]).is_none());
        assert!(build_keys(&vec![([0; 32], [0; 16]); MAX + 1]).is_none());
        let orders = [
            Some(Order {
                seq: 9,
                timestamp_ms: 7000,
            }),
            None,
        ];
        let p = build_orders(&orders).unwrap();
        assert_eq!(parse_orders(&p, 2), Some(orders.to_vec()));
        assert!(parse_orders(&p, 1).is_none());
        let mut bad = p.clone();
        bad.push(0);
        assert!(parse_orders(&bad, 2).is_none());
        let mut bad = vec![1];
        bad.extend_from_slice(&1u64.to_be_bytes());
        bad.extend_from_slice(&u64::MAX.to_be_bytes());
        assert!(parse_orders(&bad, 1).is_none());
        let mut bad = vec![1];
        bad.extend_from_slice(&0u64.to_be_bytes());
        bad.extend_from_slice(&1u64.to_be_bytes());
        assert!(parse_orders(&bad, 1).is_none());
    }
}
