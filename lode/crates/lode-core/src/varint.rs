//! Unsigned LEB128 with strict, canonical decoding.

/// Appends `v` as unsigned LEB128.
pub(crate) fn encode(mut v: u64, out: &mut Vec<u8>) {
    loop {
        let byte = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

/// Why a varint could not be decoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum VarintError {
    Truncated,
    /// More bytes than the value needs (a trailing zero group), or more than 64 bits.
    NonCanonical,
}

/// Decodes one varint from the front of `input`, returning the value and the bytes used.
pub(crate) fn decode(input: &[u8]) -> Result<(u64, usize), VarintError> {
    let mut value: u64 = 0;
    for (i, &byte) in input.iter().enumerate() {
        if i == 9 && byte > 1 {
            // A tenth byte may only contribute the 64th bit.
            return Err(VarintError::NonCanonical);
        }
        value |= u64::from(byte & 0x7f) << (7 * i);
        if byte & 0x80 == 0 {
            if byte == 0 && i > 0 {
                return Err(VarintError::NonCanonical);
            }
            return Ok((value, i + 1));
        }
        if i == 9 {
            return Err(VarintError::NonCanonical);
        }
    }
    Err(VarintError::Truncated)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        for v in [0, 1, 127, 128, 300, u32::MAX as u64, u64::MAX] {
            let mut buf = Vec::new();
            encode(v, &mut buf);
            assert_eq!(decode(&buf), Ok((v, buf.len())), "{v}");
        }
    }

    #[test]
    fn rejects_overlong_and_truncated() {
        assert_eq!(decode(&[0x80, 0x00]), Err(VarintError::NonCanonical));
        assert_eq!(decode(&[0x81, 0x80, 0x00]), Err(VarintError::NonCanonical));
        assert_eq!(decode(&[0x80]), Err(VarintError::Truncated));
        assert_eq!(decode(&[]), Err(VarintError::Truncated));
        let mut too_big = vec![0xff; 9];
        too_big.push(0x02);
        assert_eq!(decode(&too_big), Err(VarintError::NonCanonical));
        let mut eleven = vec![0xff; 10];
        eleven.push(0x01);
        assert_eq!(decode(&eleven), Err(VarintError::NonCanonical));
    }
}
