use std::sync::Arc;

use anyhow::Context;

use axum::extract::{FromRef, FromRequestParts, Path};
use axum::http::StatusCode;
use axum::http::request::Parts;
use axum::response::Response;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;
use subtle::ConstantTimeEq;

pub const INSECURE: &str = "insecure";

#[derive(Clone, Default)]
pub struct Auth {
    key: Option<Arc<[u8]>>,
    allow_insecure: bool,
}

impl Auth {
    pub fn new(key_hex: Option<&str>, allow_insecure: bool) -> anyhow::Result<Self> {
        let key = match key_hex {
            Some(s) if !s.trim().is_empty() => {
                let bytes = hex::decode(s.trim())
                    .context("DUCKXY_KEY must be hex-encoded bytes; generate one with: openssl rand -hex 32")?;
                if bytes.is_empty() {
                    anyhow::bail!("DUCKXY_KEY is empty");
                }
                Some(Arc::from(bytes))
            }
            _ => None,
        };
        if key.is_none() && !allow_insecure {
            anyhow::bail!("set DUCKXY_KEY, or set DUCKXY_ALLOW_INSECURE=true to run unsigned");
        }
        Ok(Self { key, allow_insecure })
    }

    pub fn sign(&self, path: &str) -> Option<String> {
        let key = self.key.as_ref()?;
        let mut mac = <Hmac<Sha256> as KeyInit>::new_from_slice(key).ok()?;
        mac.update(path.as_bytes());
        Some(URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes()))
    }

    fn verify(&self, signature: &str, path: &str) -> bool {
        if signature == INSECURE {
            return self.allow_insecure;
        }
        let Some(key) = self.key.as_ref() else { return false };
        let Ok(provided) = URL_SAFE_NO_PAD.decode(signature.as_bytes()) else { return false };
        let Ok(mut mac) = <Hmac<Sha256> as KeyInit>::new_from_slice(key) else { return false };
        mac.update(path.as_bytes());
        provided.ct_eq(&mac.finalize().into_bytes()).into()
    }
}

pub struct SignedPath(pub String);

impl<S> FromRequestParts<S> for SignedPath
where
    S: Send + Sync,
    Auth: FromRef<S>,
{
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let Path((signature, rest)): Path<(String, String)> =
            Path::from_request_parts(parts, state).await.map_err(|_| forbidden())?;

        let path = format!("/{rest}");
        if Auth::from_ref(state).verify(&signature, &path) { Ok(SignedPath(path)) } else { Err(forbidden()) }
    }
}

fn forbidden() -> Response {
    crate::error(StatusCode::FORBIDDEN, "forbidden")
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    const KEY: &str = "00112233445566778899aabbccddeeff";
    const PATH: &str = "/@dataset:cities/output.geojson";

    fn signed() -> Auth {
        Auth::new(Some(KEY), false).unwrap()
    }

    #[test]
    fn a_signature_it_produced_verifies() {
        let a = signed();
        assert!(a.verify(&a.sign(PATH).unwrap(), PATH));
    }

    #[rstest]
    #[case("/@dataset:other/output.geojson")]
    #[case("/@dataset:cities/output.json")]
    #[case("/@dataset:cities,id:1/output.geojson")]
    fn a_signature_does_not_transfer_to_another_path(#[case] other: &str) {
        let a = signed();
        assert!(!a.verify(&a.sign(PATH).unwrap(), other));
    }

    #[rstest]
    #[case("")]
    #[case("not-base64!!")]
    #[case("AAAA")]
    #[case(INSECURE)]
    fn bad_signatures_are_rejected(#[case] signature: &str) {
        assert!(!signed().verify(signature, PATH));
    }

    #[test]
    fn a_different_key_does_not_verify() {
        let other = Auth::new(Some("ffeeddccbbaa99887766554433221100"), false).unwrap();
        assert!(!signed().verify(&other.sign(PATH).unwrap(), PATH));
    }

    #[test]
    fn insecure_is_accepted_only_when_allowed() {
        assert!(Auth::new(Some(KEY), true).unwrap().verify(INSECURE, PATH));
        assert!(!signed().verify(INSECURE, PATH));
    }

    #[test]
    fn without_a_key_nothing_verifies_even_in_insecure_mode() {
        let a = Auth::new(None, true).unwrap();
        assert!(a.verify(INSECURE, PATH));
        assert!(!a.verify("AAAA", PATH));
        assert!(a.sign(PATH).is_none());
    }

    #[test]
    fn refusing_to_start_unconfigured_is_the_default() {
        assert!(Auth::new(None, false).is_err());
        assert!(Auth::new(Some(""), false).is_err());
    }
}
