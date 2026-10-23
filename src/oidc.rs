//! Owner sign-in through an OpenID Connect provider (Pocket ID): authorization
//! code flow with PKCE, ending in a signed session cookie that the admin API
//! accepts alongside the admin token.

use anyhow::{Context, Result, bail};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use ring::{hmac, rand::SecureRandom};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::{
    sync::Mutex,
    time::{SystemTime, UNIX_EPOCH},
};

pub const SESSION_COOKIE: &str = "fh_session";
pub const STATE_COOKIE: &str = "fh_oidc";
/// Sessions slide: each use more than a day after issue renews them.
pub const SESSION_SECONDS: u64 = 365 * 24 * 3600;
pub const SESSION_RENEW_AFTER: u64 = 24 * 3600;
pub const STATE_SECONDS: u64 = 600;

pub struct OidcConfig {
    pub issuer: String,
    pub client_id: String,
    pub client_secret: String,
    /// Where the provider sends the browser back to (`…/auth/callback`).
    pub redirect_url: String,
    /// Sign-in needs membership in one of these groups; empty allows anyone the provider lets through.
    pub allowed_groups: Vec<String>,
}

#[derive(Clone, Deserialize)]
struct Discovery {
    issuer: String,
    authorization_endpoint: String,
    token_endpoint: String,
}

pub struct Oidc {
    config: OidcConfig,
    discovery: Mutex<Option<Discovery>>,
}

/// Login state kept in a short-lived cookie between `/auth/login` and the callback.
#[derive(Serialize, Deserialize)]
pub struct LoginState {
    pub state: String,
    pub nonce: String,
    pub verifier: String,
    pub exp: u64,
}

/// The signed-in owner, as stored in the session cookie.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Session {
    pub sub: String,
    pub name: String,
    pub exp: u64,
}

#[derive(Deserialize)]
struct IdClaims {
    iss: String,
    sub: String,
    aud: Audience,
    exp: u64,
    nonce: Option<String>,
    name: Option<String>,
    preferred_username: Option<String>,
    #[serde(default)]
    groups: Vec<String>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Audience {
    One(String),
    Many(Vec<String>),
}

impl Audience {
    fn contains(&self, id: &str) -> bool {
        match self {
            Audience::One(aud) => aud == id,
            Audience::Many(auds) => auds.iter().any(|aud| aud == id),
        }
    }
}

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}

pub fn random_token() -> String {
    let mut bytes = [0u8; 32];
    ring::rand::SystemRandom::new()
        .fill(&mut bytes)
        .expect("system randomness");
    URL_SAFE_NO_PAD.encode(bytes)
}

/// Signs cookies with a key derived from the admin token, so rotating the
/// token signs every browser out.
pub struct Signer(hmac::Key);

impl Signer {
    pub fn new(admin_token: &str) -> Self {
        let root = hmac::Key::new(hmac::HMAC_SHA256, admin_token.as_bytes());
        let derived = hmac::sign(&root, b"feedback-hub cookie key v1");
        Self(hmac::Key::new(hmac::HMAC_SHA256, derived.as_ref()))
    }

    /// `kind` keeps one cookie's value from being replayed as another's.
    pub fn sign<T: Serialize>(&self, kind: &str, value: &T) -> String {
        let payload = URL_SAFE_NO_PAD.encode(serde_json::to_vec(value).expect("serializable"));
        let tag = hmac::sign(&self.0, format!("{kind}.{payload}").as_bytes());
        format!("{payload}.{}", URL_SAFE_NO_PAD.encode(tag.as_ref()))
    }

    /// Returns the value if the signature matches; callers check expiry.
    pub fn verify<T: DeserializeOwned>(&self, kind: &str, cookie: &str) -> Option<T> {
        let (payload, tag) = cookie.split_once('.')?;
        let tag = URL_SAFE_NO_PAD.decode(tag).ok()?;
        hmac::verify(&self.0, format!("{kind}.{payload}").as_bytes(), &tag).ok()?;
        serde_json::from_slice(&URL_SAFE_NO_PAD.decode(payload).ok()?).ok()
    }
}

fn pkce_challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(ring::digest::digest(
        &ring::digest::SHA256,
        verifier.as_bytes(),
    ))
}

fn form_encode(pairs: &[(&str, &str)]) -> String {
    fn encode(s: &str) -> String {
        s.bytes()
            .map(|b| match b {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                    (b as char).to_string()
                }
                _ => format!("%{b:02X}"),
            })
            .collect()
    }
    pairs
        .iter()
        .map(|(k, v)| format!("{}={}", encode(k), encode(v)))
        .collect::<Vec<_>>()
        .join("&")
}

impl Oidc {
    pub fn new(config: OidcConfig) -> Self {
        Self {
            config,
            discovery: Mutex::new(None),
        }
    }

    /// Blocking: fetches the provider's metadata once and caches it.
    fn discovery(&self) -> Result<Discovery> {
        if let Some(discovery) = self.discovery.lock().unwrap().clone() {
            return Ok(discovery);
        }
        let url = format!(
            "{}/.well-known/openid-configuration",
            self.config.issuer.trim_end_matches('/')
        );
        let discovery: Discovery = ureq::get(&url)
            .call()
            .with_context(|| format!("fetching {url}"))?
            .body_mut()
            .read_json()?;
        *self.discovery.lock().unwrap() = Some(discovery.clone());
        Ok(discovery)
    }

    /// Blocking: the provider URL to send the browser to, and the state to remember.
    pub fn start(&self) -> Result<(String, LoginState)> {
        let discovery = self.discovery()?;
        let login = LoginState {
            state: random_token(),
            nonce: random_token(),
            verifier: random_token(),
            exp: now() + STATE_SECONDS,
        };
        let challenge = pkce_challenge(&login.verifier);
        let query = form_encode(&[
            ("response_type", "code"),
            ("client_id", &self.config.client_id),
            ("redirect_uri", &self.config.redirect_url),
            ("scope", "openid profile email groups"),
            ("state", &login.state),
            ("nonce", &login.nonce),
            ("code_challenge", &challenge),
            ("code_challenge_method", "S256"),
        ]);
        let separator = if discovery.authorization_endpoint.contains('?') {
            '&'
        } else {
            '?'
        };
        Ok((
            format!("{}{separator}{query}", discovery.authorization_endpoint),
            login,
        ))
    }

    /// Blocking: redeems the authorization code and checks who signed in.
    pub fn finish(&self, login: &LoginState, code: &str) -> Result<Session> {
        let discovery = self.discovery()?;
        #[derive(Deserialize)]
        struct TokenResponse {
            id_token: String,
        }
        let body = form_encode(&[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", &self.config.redirect_url),
            ("client_id", &self.config.client_id),
            ("client_secret", &self.config.client_secret),
            ("code_verifier", &login.verifier),
        ]);
        let tokens: TokenResponse = ureq::post(&discovery.token_endpoint)
            .header("content-type", "application/x-www-form-urlencoded")
            .send(body)
            .context("redeeming the authorization code")?
            .body_mut()
            .read_json()?;
        self.check_id_token(&discovery, login, &tokens.id_token)
    }

    /// The ID token comes straight from the token endpoint over TLS, so its
    /// signature need not be checked (OpenID Connect Core 3.1.3.7); the
    /// claims still are.
    fn check_id_token(
        &self,
        discovery: &Discovery,
        login: &LoginState,
        id_token: &str,
    ) -> Result<Session> {
        let payload = id_token.split('.').nth(1).context("malformed ID token")?;
        let claims: IdClaims = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(payload)?)
            .context("reading ID token claims")?;
        if claims.iss != discovery.issuer {
            bail!("ID token from unexpected issuer {}", claims.iss);
        }
        if !claims.aud.contains(&self.config.client_id) {
            bail!("ID token is for another client");
        }
        if claims.exp <= now() {
            bail!("ID token has expired");
        }
        if claims.nonce.as_deref() != Some(login.nonce.as_str()) {
            bail!("ID token nonce does not match");
        }
        let allowed = &self.config.allowed_groups;
        if !allowed.is_empty() && !claims.groups.iter().any(|g| allowed.contains(g)) {
            bail!("{} is not in an allowed group", claims.sub);
        }
        Ok(Session {
            name: claims
                .name
                .or(claims.preferred_username)
                .unwrap_or_else(|| claims.sub.clone()),
            sub: claims.sub,
            exp: now() + SESSION_SECONDS,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn oidc(groups: &[&str]) -> Oidc {
        Oidc::new(OidcConfig {
            issuer: "https://id.example.org".into(),
            client_id: "hub".into(),
            client_secret: "s".into(),
            redirect_url: "https://hub.example.org/auth/callback".into(),
            allowed_groups: groups.iter().map(|g| g.to_string()).collect(),
        })
    }

    fn discovery() -> Discovery {
        Discovery {
            issuer: "https://id.example.org".into(),
            authorization_endpoint: "https://id.example.org/authorize".into(),
            token_endpoint: "https://id.example.org/api/oidc/token".into(),
        }
    }

    fn login() -> LoginState {
        LoginState {
            state: "st".into(),
            nonce: "n1".into(),
            verifier: "v".into(),
            exp: now() + 60,
        }
    }

    fn token(claims: serde_json::Value) -> String {
        format!(
            "e30.{}.sig",
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).unwrap())
        )
    }

    fn claims() -> serde_json::Value {
        serde_json::json!({
            "iss": "https://id.example.org", "sub": "u1", "aud": "hub",
            "exp": now() + 60, "nonce": "n1", "name": "Owner", "groups": ["admins"],
        })
    }

    #[test]
    fn accepts_valid_id_token() {
        let session = oidc(&["admins"])
            .check_id_token(&discovery(), &login(), &token(claims()))
            .unwrap();
        assert_eq!(session.sub, "u1");
        assert_eq!(session.name, "Owner");
    }

    #[test]
    fn rejects_bad_claims() {
        let cases = [
            ("iss", serde_json::json!("https://evil.example.org")),
            ("aud", serde_json::json!(["other"])),
            ("exp", serde_json::json!(1)),
            ("nonce", serde_json::json!("n2")),
            ("groups", serde_json::json!(["family"])),
        ];
        for (key, value) in cases {
            let mut claims = claims();
            claims[key] = value;
            assert!(
                oidc(&["admins"])
                    .check_id_token(&discovery(), &login(), &token(claims))
                    .is_err(),
                "{key} should be rejected"
            );
        }
        // Without a group restriction, any member is fine.
        let mut claims = claims();
        claims["groups"] = serde_json::json!([]);
        assert!(
            oidc(&[])
                .check_id_token(&discovery(), &login(), &token(claims))
                .is_ok()
        );
    }

    #[test]
    fn start_uses_pkce() {
        let oidc = oidc(&[]);
        *oidc.discovery.lock().unwrap() = Some(discovery());
        let (url, login) = oidc.start().unwrap();
        assert!(url.starts_with("https://id.example.org/authorize?response_type=code&"));
        assert!(url.contains(&format!(
            "code_challenge={}",
            pkce_challenge(&login.verifier)
        )));
        assert!(url.contains("redirect_uri=https%3A%2F%2Fhub.example.org%2Fauth%2Fcallback"));
        assert!(url.contains(&format!("state={}", login.state)));
    }

    #[test]
    fn signed_cookies_round_trip() {
        let signer = Signer::new("token-0123456789abcdef");
        let session = Session {
            sub: "u1".into(),
            name: "Owner".into(),
            exp: 5,
        };
        let cookie = signer.sign(SESSION_COOKIE, &session);
        assert_eq!(
            signer.verify::<Session>(SESSION_COOKIE, &cookie),
            Some(session)
        );
        // Another kind, another key, or a tampered value don't verify.
        assert!(signer.verify::<Session>(STATE_COOKIE, &cookie).is_none());
        assert!(
            Signer::new("other-token-0123456789")
                .verify::<Session>(SESSION_COOKIE, &cookie)
                .is_none()
        );
        let tampered = format!("e30{}", &cookie[3..]);
        assert!(
            signer
                .verify::<Session>(SESSION_COOKIE, &tampered)
                .is_none()
        );
    }
}
