//! Small helpers shared by the adapter: number parsing, quoting, base64, hashing.

/// Parses an unsigned integer written as decimal, `0x` hex, `0b`/`#` binary or `0o` octal.
/// Underscores are allowed as digit separators.
pub fn parse_u64(text: &str) -> Option<u64> {
    let t = text.trim().replace('_', "");
    let lower = t.to_ascii_lowercase();
    if let Some(hex) = lower.strip_prefix("0x") {
        u64::from_str_radix(hex, 16).ok()
    } else if let Some(bin) = lower.strip_prefix("0b").or_else(|| lower.strip_prefix('#')) {
        u64::from_str_radix(bin, 2).ok()
    } else if let Some(oct) = lower.strip_prefix("0o") {
        u64::from_str_radix(oct, 8).ok()
    } else {
        lower.parse::<u64>().ok()
    }
}

/// Like [`parse_u64`] but also accepts a leading minus sign (two's complement in `bits`).
pub fn parse_int_bits(text: &str, bits: u32) -> Option<u64> {
    let t = text.trim();
    if let Some(rest) = t.strip_prefix('-') {
        let magnitude = parse_u64(rest)?;
        let value = (magnitude as i128).checked_neg()?;
        let min = -(1i128 << (bits.min(64) - 1));
        if value < min {
            return None;
        }
        Some((value as u64) & mask(bits))
    } else {
        let value = parse_u64(t)?;
        if bits < 64 && value > mask(bits) {
            return None;
        }
        Some(value)
    }
}

/// Bit mask with the lowest `bits` bits set.
pub fn mask(bits: u32) -> u64 {
    if bits >= 64 {
        u64::MAX
    } else {
        (1u64 << bits) - 1
    }
}

/// Formats `value` as `0x`-prefixed hex padded to the given bit width.
pub fn hex(value: u64, bits: u32) -> String {
    let digits = bits.div_ceil(4).max(1) as usize;
    format!("0x{value:0digits$x}")
}

/// Parses the leading hexadecimal address of a GDB value such as `0x20000010 <buf>`.
pub fn leading_address(value: &str) -> Option<u64> {
    let v = value.trim();
    let v = v.strip_prefix('(').map_or(v, |rest| {
        // `(int *) 0x20000000` style
        rest.split_once(')')
            .map_or(rest, |(_, tail)| tail.trim_start())
    });
    let rest = v.strip_prefix("0x")?;
    let end = rest
        .find(|c: char| !c.is_ascii_hexdigit())
        .unwrap_or(rest.len());
    u64::from_str_radix(&rest[..end], 16).ok()
}

/// Quotes a string as a GDB/MI c-string parameter.
pub fn mi_quote(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub fn base64_encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(B64[(n >> 18) as usize & 63] as char);
        out.push(B64[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            B64[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            B64[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

pub fn base64_decode(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() / 4 * 3);
    let mut acc: u32 = 0;
    let mut bits = 0;
    for c in text.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' | b'\r' | b'\n' | b' ' => continue,
            _ => return None,
        };
        acc = (acc << 6) | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

/// Decodes a GDB hex byte string (`"0a1bff"`).
pub fn hex_decode(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).ok())
        .collect()
}

pub fn hex_encode(data: &[u8]) -> String {
    data.iter().map(|b| format!("{b:02x}")).collect()
}

/// 64-bit FNV-1a; stable across Rust releases (used for cache directory names).
pub fn fnv1a(data: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in data {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    hash
}

/// Makes a string safe to use as a file name on every platform.
pub fn sanitize_file_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let trimmed = cleaned.trim_matches('.');
    let mut result = if trimmed.is_empty() {
        "anon".to_string()
    } else {
        trimmed.to_string()
    };
    result.truncate(80);
    result
}

/// Little-endian bytes to integer (up to 8 bytes).
pub fn le_to_u64(bytes: &[u8]) -> u64 {
    bytes
        .iter()
        .take(8)
        .enumerate()
        .fold(0u64, |acc, (i, b)| acc | (u64::from(*b) << (8 * i)))
}

pub fn u64_to_le(value: u64, len: usize) -> Vec<u8> {
    (0..len).map(|i| (value >> (8 * i)) as u8).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_numbers_in_all_bases() {
        assert_eq!(parse_u64("42"), Some(42));
        assert_eq!(parse_u64("0x2A"), Some(42));
        assert_eq!(parse_u64("0b101010"), Some(42));
        assert_eq!(parse_u64("#101010"), Some(42));
        assert_eq!(parse_u64("0o52"), Some(42));
        assert_eq!(parse_u64("0xFFFF_0000"), Some(0xFFFF_0000));
        assert_eq!(parse_u64("zz"), None);
        assert_eq!(parse_u64(""), None);
    }

    #[test]
    fn parses_signed_values_with_width() {
        assert_eq!(parse_int_bits("-1", 8), Some(0xFF));
        assert_eq!(parse_int_bits("-128", 8), Some(0x80));
        assert_eq!(parse_int_bits("-129", 8), None);
        assert_eq!(parse_int_bits("256", 8), None);
        assert_eq!(parse_int_bits("0xFFFFFFFF", 32), Some(0xFFFF_FFFF));
        assert_eq!(parse_int_bits("3", 2), Some(3));
        assert_eq!(parse_int_bits("4", 2), None);
    }

    #[test]
    fn formats_hex_with_padding() {
        assert_eq!(hex(0x1F, 32), "0x0000001f");
        assert_eq!(hex(0x3, 2), "0x3");
        assert_eq!(hex(0xAB, 8), "0xab");
    }

    #[test]
    fn extracts_leading_addresses() {
        assert_eq!(leading_address("0x20000010 <buf>"), Some(0x2000_0010));
        assert_eq!(leading_address("(int *) 0x20000000"), Some(0x2000_0000));
        assert_eq!(leading_address("42"), None);
    }

    #[test]
    fn quotes_mi_strings() {
        assert_eq!(mi_quote(r#"a "b" \c"#), r#""a \"b\" \\c""#);
        assert_eq!(mi_quote("x\ny"), r#""x\ny""#);
    }

    #[test]
    fn base64_round_trip() {
        for data in [&b""[..], b"f", b"fo", b"foo", b"foob", b"fooba", b"foobar"] {
            let encoded = base64_encode(data);
            assert_eq!(base64_decode(&encoded).unwrap(), data);
        }
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert!(base64_decode("!!").is_none());
    }

    #[test]
    fn hex_round_trip() {
        let data = [0x00, 0x7f, 0xff, 0x10];
        assert_eq!(hex_encode(&data), "007fff10");
        assert_eq!(hex_decode("007fff10").unwrap(), data);
        assert!(hex_decode("abc").is_none());
    }

    #[test]
    fn sanitizes_file_names() {
        assert_eq!(sanitize_file_name("ns::foo<int>"), "ns__foo_int_");
        assert_eq!(sanitize_file_name("..."), "anon");
        assert_eq!(sanitize_file_name("main"), "main");
    }

    #[test]
    fn little_endian_conversion() {
        assert_eq!(le_to_u64(&[0x78, 0x56, 0x34, 0x12]), 0x1234_5678);
        assert_eq!(u64_to_le(0x1234_5678, 4), vec![0x78, 0x56, 0x34, 0x12]);
    }

    #[test]
    fn fnv_is_stable() {
        assert_eq!(fnv1a(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a(b"a"), 0xaf63_dc4c_8601_ec8c);
    }
}
