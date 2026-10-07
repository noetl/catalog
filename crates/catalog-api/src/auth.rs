//! The internal-token check, mirroring `noetl/server`'s `/api/internal/*` semantics.

use axum::http::{HeaderMap, StatusCode};

use crate::ApiState;

/// Verify the bearer token against the token in state.
///
/// Failure modes, identical to the server's:
///
/// * **503** when the expected token is unconfigured. ⚠ Deliberate: a privileged surface
///   gets **no permissive default**. An unset token must not mean "allow", because that
///   is the shape in which a misconfigured deploy silently opens a write path.
/// * **403** when the `Authorization` header is missing, is not `Bearer`, or the token
///   does not match.
pub fn require_internal_token(
    state: &ApiState,
    headers: &HeaderMap,
) -> Result<(), (StatusCode, String)> {
    let Some(expected) = state.token.as_deref() else {
        tracing::warn!("catalog write called but no internal token is configured; 503");
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            format!(
                "catalog write API not configured: {} is unset on the server",
                crate::TOKEN_ENV
            ),
        ));
    };
    let Some(header) = headers.get("authorization") else {
        return Err((
            StatusCode::FORBIDDEN,
            "catalog write API requires an Authorization: Bearer header".into(),
        ));
    };
    let Ok(value) = header.to_str() else {
        return Err((
            StatusCode::FORBIDDEN,
            "Authorization header is not valid UTF-8".into(),
        ));
    };
    let Some(token) = value.strip_prefix("Bearer ") else {
        return Err((
            StatusCode::FORBIDDEN,
            "Authorization header must use the Bearer scheme".into(),
        ));
    };
    if !constant_time_eq(token.trim().as_bytes(), expected.trim().as_bytes()) {
        return Err((StatusCode::FORBIDDEN, "invalid internal API token".into()));
    }
    Ok(())
}

/// ⚠ Length-independent compare, so failure time does not reveal how much of the token
/// matched. The early length check leaks only the length, which the header already does.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constant_time_eq_is_still_correct() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"ab"));
        assert!(constant_time_eq(b"", b""));
    }
}
