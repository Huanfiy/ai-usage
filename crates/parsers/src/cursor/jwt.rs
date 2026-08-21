use serde::Deserialize;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Claims {
    pub sub: String,
    pub email: String,
}

#[derive(Debug, Deserialize)]
struct Payload {
    #[serde(default)]
    sub: String,
    #[serde(default)]
    email: String,
}

/// Decode the JWT payload. The token itself is never returned to callers of parse.
pub fn decode_claims(token: &str) -> Option<Claims> {
    let payload = token.split('.').nth(1)?;
    let bytes = b64url_decode(payload)?;
    let parsed: Payload = serde_json::from_slice(&bytes).ok()?;
    let sub = parsed.sub.trim();
    if sub.is_empty() {
        return None;
    }
    Some(Claims {
        sub: sub.to_string(),
        email: parsed.email.trim().to_string(),
    })
}

fn b64url_decode(input: &str) -> Option<Vec<u8>> {
    fn val(c: u8) -> Option<u8> {
        match c {
            b'A'..=b'Z' => Some(c - b'A'),
            b'a'..=b'z' => Some(c - b'a' + 26),
            b'0'..=b'9' => Some(c - b'0' + 52),
            b'+' | b'-' => Some(62),
            b'/' | b'_' => Some(63),
            _ => None,
        }
    }
    let mut out = Vec::with_capacity(input.len() * 3 / 4);
    let mut buf = 0u32;
    let mut nbits = 0;
    for &c in input.as_bytes() {
        if c == b'=' {
            break;
        }
        buf = (buf << 6) | u32::from(val(c)?);
        nbits += 6;
        if nbits >= 8 {
            nbits -= 8;
            out.push((buf >> nbits) as u8);
            buf &= (1 << nbits) - 1;
        }
    }
    Some(out)
}

#[cfg(test)]
fn b64url_encode(bytes: &[u8]) -> String {
    const TABLE: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::new();
    let mut i = 0;
    while i < bytes.len() {
        let b0 = bytes[i];
        let b1 = bytes.get(i + 1).copied();
        let b2 = bytes.get(i + 2).copied();
        out.push(TABLE[(b0 >> 2) as usize] as char);
        out.push(TABLE[(((b0 & 0x03) << 4) | (b1.unwrap_or(0) >> 4)) as usize] as char);
        if b1.is_none() {
            break;
        }
        out.push(TABLE[(((b1.unwrap_or(0) & 0x0f) << 2) | (b2.unwrap_or(0) >> 6)) as usize] as char);
        if b2.is_none() {
            break;
        }
        out.push(TABLE[(b2.unwrap_or(0) & 0x3f) as usize] as char);
        i += 3;
    }
    out
}

#[cfg(test)]
fn fake_jwt(sub: &str, email: &str) -> String {
    let payload = if email.is_empty() {
        format!(r#"{{"sub":"{sub}"}}"#)
    } else {
        format!(r#"{{"sub":"{sub}","email":"{email}"}}"#)
    };
    format!("eyJhbGciOiJub25lIn0.{}.sig", b64url_encode(payload.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ai_usage_protocol::account_hash_from_sub;

    #[test]
    fn decodes_sub_and_email() {
        let token = fake_jwt("user_01abc", "you@example.com");
        let claims = decode_claims(&token).unwrap();
        assert_eq!(claims.sub, "user_01abc");
        assert_eq!(claims.email, "you@example.com");
        assert_eq!(
            account_hash_from_sub(&claims.sub),
            account_hash_from_sub("user_01abc")
        );
    }

    #[test]
    fn rejects_empty_sub() {
        let payload = b64url_encode(br#"{"email":"a@b.com"}"#);
        assert!(decode_claims(&format!("h.{payload}.s")).is_none());
    }
}
