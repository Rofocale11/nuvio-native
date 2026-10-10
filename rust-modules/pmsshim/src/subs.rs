//! Addon subtitle downloads → the UTF-8 text the app's sidecar parser takes (`PLAYBACK.md`: the
//! control plane does not follow redirects, so the shim fetches and normalises the file itself).

use std::io::Read;

/// Gunzip when gzipped, then decode: UTF-8 (BOM stripped), UTF-16 with a BOM, else Windows-1252 —
/// the encoding of most legacy SRTs that are not UTF-8.
pub fn to_utf8_text(mut b: Vec<u8>) -> Vec<u8> {
    if b.starts_with(&[0x1f, 0x8b]) {
        let mut out = Vec::new();
        if flate2::read::MultiGzDecoder::new(&b[..]).take(16 << 20).read_to_end(&mut out).is_ok() {
            b = out;
        }
    }
    if b.starts_with(&[0xEF, 0xBB, 0xBF]) {
        b.drain(..3);
    }
    if let Some(rest) = b.strip_prefix(&[0xFF, 0xFE]) {
        return String::from_utf16_lossy(&rest.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect::<Vec<_>>()).into_bytes();
    }
    if let Some(rest) = b.strip_prefix(&[0xFE, 0xFF]) {
        return String::from_utf16_lossy(&rest.chunks_exact(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect::<Vec<_>>()).into_bytes();
    }
    if std::str::from_utf8(&b).is_ok() {
        return b;
    }
    b.iter().map(|&c| cp1252(c)).collect::<String>().into_bytes()
}

fn cp1252(c: u8) -> char {
    const HIGH: [u16; 32] = [
        0x20AC, 0xFFFD, 0x201A, 0x0192, 0x201E, 0x2026, 0x2020, 0x2021, 0x02C6, 0x2030, 0x0160, 0x2039, 0x0152, 0xFFFD, 0x017D, 0xFFFD,
        0xFFFD, 0x2018, 0x2019, 0x201C, 0x201D, 0x2022, 0x2013, 0x2014, 0x02DC, 0x2122, 0x0161, 0x203A, 0x0153, 0xFFFD, 0x017E, 0x0178,
    ];
    match c {
        0x80..=0x9F => char::from_u32(u32::from(HIGH[usize::from(c - 0x80)])).unwrap_or('\u{FFFD}'),
        _ => char::from(c),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn decodes() {
        assert_eq!(to_utf8_text(b"\xEF\xBB\xBF1\nh\xC3\xA9".to_vec()), "1\nhé".as_bytes());
        assert_eq!(to_utf8_text(b"caf\xE9 \x93q\x94".to_vec()), "café “q”".as_bytes());
        assert_eq!(to_utf8_text(vec![0xFF, 0xFE, b'h', 0, b'i', 0]), b"hi");
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        gz.write_all(b"1\n00:00:01,000 --> 00:00:02,000\nhi\n").unwrap();
        assert!(String::from_utf8(to_utf8_text(gz.finish().unwrap())).unwrap().ends_with("hi\n"));
    }
}
