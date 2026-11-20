//! The validity period of an X.509 certificate, read from its DER encoding without a
//! certificate library: the server shows its certificate's expiry (T140), and `loded doctor`
//! and `lode doctor` check it (T178).

/// The validity period of a DER certificate, as Unix seconds.
pub fn validity(der: &[u8]) -> Option<(i64, i64)> {
    let (tag, cert, _) = tlv(der)?;
    if tag != 0x30 {
        return None;
    }
    let (tag, tbs, _) = tlv(cert)?;
    if tag != 0x30 {
        return None;
    }
    let mut rest = tbs;
    let (tag, _, after) = tlv(rest)?;
    if tag == 0xa0 {
        rest = after; // explicit version
    }
    // serialNumber, signature, issuer
    for expected in [0x02, 0x30, 0x30] {
        let (tag, _, after) = tlv(rest)?;
        if tag != expected {
            return None;
        }
        rest = after;
    }
    let (tag, validity, _) = tlv(rest)?;
    if tag != 0x30 {
        return None;
    }
    let (tag1, t1, after) = tlv(validity)?;
    let (tag2, t2, _) = tlv(after)?;
    Some((der_time(tag1, t1)?, der_time(tag2, t2)?))
}

/// One DER tag-length-value: the tag, the contents and what follows.
fn tlv(input: &[u8]) -> Option<(u8, &[u8], &[u8])> {
    let (&tag, rest) = input.split_first()?;
    let (&first, mut rest) = rest.split_first()?;
    let len = if first < 0x80 {
        usize::from(first)
    } else {
        let n = usize::from(first & 0x7f);
        if n == 0 || n > 4 || rest.len() < n {
            return None;
        }
        let (bytes, after) = rest.split_at(n);
        rest = after;
        bytes
            .iter()
            .fold(0usize, |acc, b| (acc << 8) | usize::from(*b))
    };
    if rest.len() < len {
        return None;
    }
    let (content, after) = rest.split_at(len);
    Some((tag, content, after))
}

/// UTCTime (`YYMMDDHHMMSSZ`) or GeneralizedTime (`YYYYMMDDHHMMSSZ`) as Unix seconds.
fn der_time(tag: u8, bytes: &[u8]) -> Option<i64> {
    let text = std::str::from_utf8(bytes).ok()?.strip_suffix('Z')?;
    let (year, rest) = match (tag, text.len()) {
        (0x17, 12) => {
            let yy: i32 = text.get(..2)?.parse().ok()?;
            (if yy >= 50 { 1900 + yy } else { 2000 + yy }, text.get(2..)?)
        }
        (0x18, 14) => (text.get(..4)?.parse().ok()?, text.get(4..)?),
        _ => return None,
    };
    let num = |r: std::ops::Range<usize>| -> Option<u8> { rest.get(r)?.parse().ok() };
    let month = time::Month::try_from(num(0..2)?).ok()?;
    let date = time::Date::from_calendar_date(year, month, num(2..4)?).ok()?;
    let clock = time::Time::from_hms(num(4..6)?, num(6..8)?, num(8..10)?).ok()?;
    Some(
        time::PrimitiveDateTime::new(date, clock)
            .assume_utc()
            .unix_timestamp(),
    )
}
