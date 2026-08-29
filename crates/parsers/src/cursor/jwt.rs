use serde::Deserialize;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Claims {
    pub sub: String,
    pub email: String,
    pub exp: Option<i64>,
    /// JWT `type` 声明：网站登录为 `web`（调不了原生 RPC），IDE 原生 token
    /// 为其它值或缺省。仅用于预判 Bot 可用性与凭证类型展示。
    pub token_type: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Payload {
    #[serde(default)]
    sub: String,
    #[serde(default)]
    email: String,
    #[serde(default)]
    exp: Option<i64>,
    #[serde(default, rename = "type")]
    token_type: Option<String>,
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
        exp: parsed.exp.filter(|e| *e > 0),
        token_type: parsed
            .token_type
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty()),
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
        out.push(
            TABLE[(((b1.unwrap_or(0) & 0x0f) << 2) | (b2.unwrap_or(0) >> 6)) as usize] as char,
        );
        if b2.is_none() {
            break;
        }
        out.push(TABLE[(b2.unwrap_or(0) & 0x3f) as usize] as char);
        i += 3;
    }
    out
}

#[cfg(test)]
pub(crate) fn fake_jwt(sub: &str, email: &str) -> String {
    fake_jwt_exp(sub, email, None)
}

#[cfg(test)]
pub(crate) fn fake_jwt_exp(sub: &str, email: &str, exp: Option<i64>) -> String {
    let mut payload = if email.is_empty() {
        format!(r#"{{"sub":"{sub}""#)
    } else {
        format!(r#"{{"sub":"{sub}","email":"{email}""#)
    };
    if let Some(exp) = exp {
        payload.push_str(&format!(r#","exp":{exp}"#));
    }
    payload.push('}');
    format!(
        "eyJhbGciOiJub25lIn0.{}.sig",
        b64url_encode(payload.as_bytes())
    )
}

#[cfg(test)]
pub(crate) fn fake_jwt_typed(sub: &str, email: &str, token_type: &str) -> String {
    let payload = format!(r#"{{"sub":"{sub}","email":"{email}","type":"{token_type}"}}"#);
    format!(
        "eyJhbGciOiJub25lIn0.{}.sig",
        b64url_encode(payload.as_bytes())
    )
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
        assert_eq!(claims.exp, None);
        assert_eq!(
            account_hash_from_sub(&claims.sub),
            account_hash_from_sub("user_01abc")
        );
        let with_exp = fake_jwt_exp("user_01abc", "you@example.com", Some(1_800_000_000));
        assert_eq!(decode_claims(&with_exp).unwrap().exp, Some(1_800_000_000));
    }

    #[test]
    fn rejects_empty_sub() {
        let payload = b64url_encode(br#"{"email":"a@b.com"}"#);
        assert!(decode_claims(&format!("h.{payload}.s")).is_none());
    }

    #[test]
    fn decodes_type_claim() {
        let web = fake_jwt_typed("user_w", "w@x.com", "web");
        assert_eq!(
            decode_claims(&web).unwrap().token_type.as_deref(),
            Some("web")
        );
        let plain = fake_jwt("user_p", "p@x.com");
        assert_eq!(decode_claims(&plain).unwrap().token_type, None);
    }
}
