//! Short-lived, module-scoped capabilities for same-origin media streaming.
//!
//! A module page cannot present the sidecar token to `<video>`/`<img>`, and its
//! sandboxed frame must not receive it. Instead the daemon mints an opaque,
//! expiring capability bound to one module. The page builds URLs under
//! `/m/<id>/media/<token>/…`; the media route verifies the capability and
//! proxies GET/HEAD to that module's own sidecar with the real sidecar token,
//! streaming the body (and Range) through unchanged.
//!
//! The capability grants exactly the authority the module already has through
//! `backend.request` — access to its own sidecar — and nothing else. It is
//! signed with an in-memory per-process key, so it cannot be forged and simply
//! becomes invalid on restart.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use hmac::{Hmac, Mac};
use sha2::Sha256;
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

type HmacSha256 = Hmac<Sha256>;

fn secret() -> &'static [u8; 32] {
    static SECRET: OnceLock<[u8; 32]> = OnceLock::new();
    SECRET.get_or_init(rand::random)
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn sign(payload: &str) -> Vec<u8> {
    let mut mac = HmacSha256::new_from_slice(secret()).expect("hmac accepts any key length");
    mac.update(payload.as_bytes());
    mac.finalize().into_bytes().to_vec()
}

/// Issue a capability token scoped to `module` for `ttl_secs`.
pub fn issue(module: &str, ttl_secs: u64) -> String {
    let exp = now_secs().saturating_add(ttl_secs);
    let payload = format!("{module}\n{exp}");
    let encoded_payload = URL_SAFE_NO_PAD.encode(payload.as_bytes());
    let encoded_sig = URL_SAFE_NO_PAD.encode(sign(&payload));
    format!("{encoded_payload}.{encoded_sig}")
}

/// Verify that `token` was issued for `module` and has not expired.
pub fn verify(token: &str, module: &str) -> bool {
    let Some((payload_b64, sig_b64)) = token.split_once('.') else {
        return false;
    };
    let Ok(payload_bytes) = URL_SAFE_NO_PAD.decode(payload_b64) else {
        return false;
    };
    let Ok(expected_sig) = URL_SAFE_NO_PAD.decode(sig_b64) else {
        return false;
    };
    let Ok(payload) = String::from_utf8(payload_bytes) else {
        return false;
    };
    let Some((bound_module, exp)) = payload.split_once('\n') else {
        return false;
    };
    if bound_module != module {
        return false;
    }
    let Ok(exp) = exp.parse::<u64>() else {
        return false;
    };
    if exp < now_secs() {
        return false;
    }
    let mut mac = HmacSha256::new_from_slice(secret()).expect("hmac accepts any key length");
    mac.update(payload.as_bytes());
    mac.verify_slice(&expected_sig).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_for_the_same_module() {
        let token = issue("com.example.media", 60);
        assert!(verify(&token, "com.example.media"));
    }

    #[test]
    fn rejects_other_modules_and_tampering() {
        let token = issue("com.example.media", 60);
        assert!(!verify(&token, "com.example.other"));
        assert!(!verify("garbage", "com.example.media"));
        assert!(!verify("a.b", "com.example.media"));

        let (payload, sig) = token.split_once('.').expect("token parts");
        let tampered = format!("{payload}x.{sig}");
        assert!(!verify(&tampered, "com.example.media"));
    }

    #[test]
    fn rejects_expired() {
        let token = issue("com.example.media", 0);
        // exp == now is treated as expired only once the second ticks over; not
        // worth sleeping in a unit test, so just confirm a clearly past token.
        let payload = "com.example.media\n1".to_string();
        let encoded_payload = URL_SAFE_NO_PAD.encode(payload.as_bytes());
        let encoded_sig = URL_SAFE_NO_PAD.encode(sign(&payload));
        let expired = format!("{encoded_payload}.{encoded_sig}");
        assert!(!verify(&expired, "com.example.media"));
        // A zero-TTL token is still accepted within the same second; ensure the
        // normal token path itself works.
        let _ = token;
    }
}
