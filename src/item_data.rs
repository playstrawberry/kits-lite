//! Bounded encoding of the item components exposed by the server.

use crate::model::{Error, MAX_ITEM_BYTES};

const HEADER: &[u8] = b"SCMP\x01";
const MAX_COMPONENTS: usize = 128;

pub fn encode(components: &[(u16, Vec<u8>)]) -> Result<Vec<u8>, Error> {
    let size = HEADER.len()
        + 2
        + components
            .iter()
            .map(|(_, value)| 6 + value.len())
            .sum::<usize>();
    if size > MAX_ITEM_BYTES || components.len() > MAX_COMPONENTS {
        return Err(Error::Limit);
    }
    let mut out = HEADER.to_vec();
    out.extend_from_slice(&(components.len() as u16).to_le_bytes());
    let mut seen = std::collections::BTreeSet::new();
    for (kind, value) in components {
        if usize::from(*kind) >= MAX_COMPONENTS || !seen.insert(*kind) {
            return Err(Error::Invalid);
        }
        out.extend_from_slice(&kind.to_le_bytes());
        out.extend_from_slice(&(value.len() as u32).to_le_bytes());
        out.extend_from_slice(value);
    }
    Ok(out)
}

pub fn decode(bytes: &[u8]) -> Result<Vec<(u16, Vec<u8>)>, Error> {
    if bytes.len() > MAX_ITEM_BYTES || !bytes.starts_with(HEADER) {
        return Err(Error::Invalid);
    }
    let mut rest = &bytes[HEADER.len()..];
    fn take<'a>(rest: &mut &'a [u8], count: usize) -> Result<&'a [u8], Error> {
        let value = rest.get(..count).ok_or(Error::Invalid)?;
        *rest = &rest[count..];
        Ok(value)
    }
    let count =
        u16::from_le_bytes(take(&mut rest, 2)?.try_into().map_err(|_| Error::Invalid)?) as usize;
    if count > MAX_COMPONENTS {
        return Err(Error::Limit);
    }
    let mut out = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for _ in 0..count {
        let kind = u16::from_le_bytes(take(&mut rest, 2)?.try_into().map_err(|_| Error::Invalid)?);
        let size = u32::from_le_bytes(take(&mut rest, 4)?.try_into().map_err(|_| Error::Invalid)?)
            as usize;
        if usize::from(kind) >= MAX_COMPONENTS || !seen.insert(kind) {
            return Err(Error::Invalid);
        }
        out.push((kind, take(&mut rest, size)?.to_vec()));
    }
    if !rest.is_empty() {
        return Err(Error::Invalid);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn component_bytes_preserve_names_lore_enchantments_and_arbitrary_payloads() {
        let components = vec![
            (0, vec![0, 255, 0, 1]),
            (6, "İsim avec accents".as_bytes().to_vec()),
            (11, vec![1, 0, 9]),
            (13, vec![255, 3]),
        ];
        assert_eq!(decode(&encode(&components).unwrap()), Ok(components));
        assert_eq!(decode(&encode(&[]).unwrap()), Ok(Vec::new()));
    }

    #[test]
    fn oversized_duplicate_truncated_and_unknown_component_data_is_refused() {
        assert!(encode(&[(0, vec![0; MAX_ITEM_BYTES])]).is_err());
        assert!(encode(&[(1, vec![]), (1, vec![])]).is_err());
        let bytes = encode(&[(1, vec![0, 1])]).unwrap();
        for end in 0..bytes.len() {
            assert!(decode(&bytes[..end]).is_err());
        }
        let mut trailing = bytes;
        trailing.push(0);
        assert!(decode(&trailing).is_err());
        assert!(encode(&[(u16::MAX, vec![])]).is_err());
    }
}
