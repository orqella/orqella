//! Validation for peer-controlled WebRTC SDP and ICE candidates.
//!
//! The transport accepts SDP and candidates over an authenticated bootstrap
//! stream, but the remote peer still controls addresses and ports. These
//! checks reject malformed or unbounded input and keep a peer from using an
//! ICE candidate to probe arbitrary local services.

use std::net::IpAddr;

use crate::error::{Error, Result};

/// Maximum SDP document accepted from a peer.
pub const MAX_SDP_LEN: usize = 256 * 1024;
/// Maximum ICE candidates accepted in one SDP document.
pub const MAX_SDP_CANDIDATES: usize = 128;
/// Maximum length of one ICE candidate string.
pub const MAX_CANDIDATE_LEN: usize = 1024;

/// Validate an SDP document received from a peer.
pub fn validate_sdp(sdp: &str) -> Result<()> {
    if sdp.is_empty() {
        return Err(Error::InvalidSdp("SDP is empty"));
    }
    if sdp.len() > MAX_SDP_LEN {
        return Err(Error::InvalidSdp("SDP is too large"));
    }
    if sdp
        .bytes()
        .any(|byte| byte == 0 || (byte < 0x20 && !matches!(byte, b'\n' | b'\r' | b'\t')))
    {
        return Err(Error::InvalidSdp("SDP contains control characters"));
    }
    let mut candidates = 0usize;
    for line in sdp.lines() {
        if let Some(candidate) = line.strip_prefix("a=candidate:") {
            candidates += 1;
            if candidates > MAX_SDP_CANDIDATES {
                return Err(Error::InvalidSdp("SDP has too many ICE candidates"));
            }
            validate_candidate_body(candidate)?;
        }
    }
    Ok(())
}

/// Validate one ICE candidate string (with or without the `candidate:` prefix).
pub fn validate_ice_candidate(candidate: &str) -> Result<()> {
    if candidate.len() > MAX_CANDIDATE_LEN {
        return Err(Error::InvalidIceCandidate("candidate is too large"));
    }
    if candidate.bytes().any(|byte| byte == 0 || byte < 0x20) {
        return Err(Error::InvalidIceCandidate(
            "candidate contains control characters",
        ));
    }
    let body = candidate.strip_prefix("candidate:").unwrap_or(candidate);
    validate_candidate_body(body)
}

fn validate_candidate_body(body: &str) -> Result<()> {
    let mut parts = body.split_whitespace();
    let foundation = parts.next().ok_or(Error::InvalidIceCandidate(
        "candidate is missing a foundation",
    ))?;
    if foundation.is_empty() || foundation.len() > 32 {
        return Err(Error::InvalidIceCandidate("invalid candidate foundation"));
    }
    let component = parts.next().ok_or(Error::InvalidIceCandidate(
        "candidate is missing a component",
    ))?;
    if !matches!(component, "1" | "2") {
        return Err(Error::InvalidIceCandidate("invalid candidate component"));
    }
    let protocol = parts.next().ok_or(Error::InvalidIceCandidate(
        "candidate is missing a protocol",
    ))?;
    if !protocol.eq_ignore_ascii_case("udp") && !protocol.eq_ignore_ascii_case("tcp") {
        return Err(Error::InvalidIceCandidate("invalid candidate protocol"));
    }
    let priority = parts.next().ok_or(Error::InvalidIceCandidate(
        "candidate is missing a priority",
    ))?;
    priority
        .parse::<u32>()
        .map_err(|_| Error::InvalidIceCandidate("invalid candidate priority"))?;
    let address = parts.next().ok_or(Error::InvalidIceCandidate(
        "candidate is missing an address",
    ))?;
    validate_candidate_address(address)?;
    let port = parts
        .next()
        .ok_or(Error::InvalidIceCandidate("candidate is missing a port"))?;
    let port = port
        .parse::<u16>()
        .map_err(|_| Error::InvalidIceCandidate("invalid candidate port"))?;
    if port == 0 {
        return Err(Error::InvalidIceCandidate("invalid candidate port"));
    }
    let kind = parts.next().ok_or(Error::InvalidIceCandidate(
        "candidate is missing a type marker",
    ))?;
    if !kind.eq_ignore_ascii_case("typ") {
        return Err(Error::InvalidIceCandidate(
            "candidate is missing a type marker",
        ));
    }
    let candidate_type = parts
        .next()
        .ok_or(Error::InvalidIceCandidate("candidate is missing a type"))?;
    if !matches!(candidate_type, "host" | "srflx" | "prflx" | "relay") {
        return Err(Error::InvalidIceCandidate("invalid candidate type"));
    }
    Ok(())
}

fn validate_candidate_address(address: &str) -> Result<()> {
    if let Ok(ip) = address.parse::<IpAddr>() {
        if ip.is_unspecified() || ip.is_multicast() {
            return Err(Error::InvalidIceCandidate(
                "candidate address is not routable",
            ));
        }
        return Ok(());
    }
    // Browsers may publish mDNS host candidates. Accept a narrow `.local`
    // hostname form, but never an arbitrary hostname that would require DNS.
    let lowered = address.to_ascii_lowercase();
    if lowered.ends_with(".local")
        && lowered.len() <= 255
        && lowered.trim_end_matches(".local").split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        })
    {
        return Ok(());
    }
    Err(Error::InvalidIceCandidate(
        "candidate address must be an IP literal or mDNS name",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_normal_host_candidates() {
        assert!(
            validate_ice_candidate("candidate:1 1 udp 2122260223 192.0.2.10 50000 typ host")
                .is_ok()
        );
        assert!(
            validate_ice_candidate(
                "candidate:2 1 udp 1686052607 203.0.113.5 60000 typ srflx raddr 0.0.0.0 rport 0"
            )
            .is_ok()
        );
        assert!(
            validate_ice_candidate(
                "candidate:3 1 udp 2122260223 3f8d2a1b-1234.local 50000 typ host"
            )
            .is_ok()
        );
    }

    #[test]
    fn rejects_probe_friendly_or_malformed_candidates() {
        for candidate in [
            "candidate:1 1 udp 1 0.0.0.0 50000 typ host",
            "candidate:1 1 udp 1 224.0.0.1 50000 typ host",
            "candidate:1 1 udp 1 localhost 50000 typ host",
            "candidate:1 1 udp 1 192.0.2.1 0 typ host",
            "candidate:1 1 sctp 1 192.0.2.1 50000 typ host",
            "candidate:1 1 udp 1 192.0.2.1 50000 typ trickle",
            "candidate:1 1 udp nope 192.0.2.1 50000 typ host",
        ] {
            assert!(validate_ice_candidate(candidate).is_err(), "{candidate}");
        }
    }

    #[test]
    fn validates_sdp_size_and_embedded_candidates() {
        assert!(validate_sdp("v=0\r\n").is_ok());
        assert!(validate_sdp("v=0\r\na=candidate:1 1 udp 1 0.0.0.0 1 typ host\r\n").is_err());
        let too_many = format!(
            "v=0\r\n{}",
            "a=candidate:1 1 udp 1 192.0.2.1 50000 typ host\r\n".repeat(129)
        );
        assert!(validate_sdp(&too_many).is_err());
        assert!(validate_sdp(&"x".repeat(MAX_SDP_LEN + 1)).is_err());
    }
}
