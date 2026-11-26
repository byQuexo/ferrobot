//! The CLI-login proxy on the agent listener. Sandboxes with a login send HTTPS through it
//! (`HTTPS_PROXY`, the agent token as the proxy password). For a login's hosts it ends TLS with a
//! certificate from ferrobot's local CA, swaps the stand-in token for the real one and forwards
//! to the service without following redirects; any other public host gets a plain tunnel, and
//! private addresses none. The real token and the CA key never enter a sandbox, nor any log.

use std::collections::HashMap;
use std::convert::Infallible;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use axum::body::Bytes;
use axum::extract::{Request, State};
use axum::http::header::{AUTHORIZATION, PROXY_AUTHENTICATE, PROXY_AUTHORIZATION};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use futures_util::TryStreamExt;
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyDataStream, BodyExt, Full, StreamBody};
use hyper::body::{Frame, Incoming};
use hyper_util::rt::TokioIo;
use rcgen::{
    BasicConstraints, CertificateParams, DistinguishedName, DnType, ExtendedKeyUsagePurpose,
    GeneralSubtree, IsCa, Issuer, KeyPair, KeyUsagePurpose, NameConstraints,
};
use serde::{Deserialize, Serialize};
use tokio_rustls::TlsAcceptor;
use tokio_rustls::rustls::ServerConfig;
use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};

use crate::AppState;
use crate::logins::LoginKind;

type BoxError = Box<dyn std::error::Error + Send + Sync>;
type ProxyBody = BoxBody<Bytes, BoxError>;

/// Secret store entry of the local CA: its key and certificate.
const CA_SECRET: &str = "logins:ca";
/// Leaf certificates are made again after this, well before they expire.
const LEAF_FOR: Duration = Duration::from_secs(7 * 24 * 3600);
/// Request headers that belong to one hop only.
const HOP: [&str; 8] = [
    "connection",
    "proxy-connection",
    "keep-alive",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
    "proxy-authorization",
];

/// The local CA and the leaf certificates made with it.
#[derive(Default)]
pub struct Egress {
    ca: tokio::sync::OnceCell<Ca>,
    leaves: Mutex<HashMap<String, (Instant, Arc<ServerConfig>)>>,
    http: OnceLock<reqwest::Client>,
}

impl std::fmt::Debug for Egress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Egress").finish_non_exhaustive()
    }
}

struct Ca {
    cert: Vec<u8>,
    issuer: Issuer<'static, KeyPair>,
}

#[derive(Serialize, Deserialize)]
struct StoredCa {
    key: String,
    cert: String,
    domains: Vec<String>,
}

fn domains() -> Vec<String> {
    LoginKind::ALL
        .iter()
        .map(|k| k.domain().to_owned())
        .collect()
}

/// The CA's fixed fields; an issuer needs the same name and key usages as its certificate.
fn ca_params(domains: &[String]) -> CertificateParams {
    let mut params = CertificateParams::default();
    let mut name = DistinguishedName::new();
    name.push(DnType::CommonName, "ferrobot CLI logins");
    params.distinguished_name = name;
    params.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
    params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    // Even misused, the CA cannot vouch for any other site.
    params.name_constraints = Some(NameConstraints {
        permitted_subtrees: domains
            .iter()
            .cloned()
            .map(GeneralSubtree::DnsName)
            .collect(),
        excluded_subtrees: Vec::new(),
    });
    params
}

fn make_ca(domains: &[String]) -> Result<(StoredCa, Ca), rcgen::Error> {
    let key = KeyPair::generate()?;
    let mut params = ca_params(domains);
    let now = time::OffsetDateTime::now_utc();
    params.not_before = now - time::Duration::days(1);
    params.not_after = now + time::Duration::days(3650);
    let cert = params.self_signed(&key)?.der().to_vec();
    let stored = StoredCa {
        key: B64.encode(key.serialize_der()),
        cert: B64.encode(&cert),
        domains: domains.to_vec(),
    };
    Ok((
        stored,
        Ca {
            cert,
            issuer: Issuer::new(params, key),
        },
    ))
}

fn load_ca(stored: &StoredCa) -> Option<Ca> {
    let key = KeyPair::try_from(B64.decode(&stored.key).ok()?.as_slice()).ok()?;
    let cert = B64.decode(&stored.cert).ok()?;
    Some(Ca {
        cert,
        issuer: Issuer::new(ca_params(&stored.domains), key),
    })
}

/// The local CA, made once and kept in the secret store; a new login service makes a new one.
async fn ca(state: &AppState) -> Result<&Ca, String> {
    state
        .egress
        .ca
        .get_or_try_init(|| async {
            let secrets = &state.settings.secrets;
            let want = domains();
            let stored = secrets.get(CA_SECRET).await.map_err(|e| e.to_string())?;
            if let Some(ca) = stored
                .and_then(|raw| serde_json::from_str::<StoredCa>(&raw).ok())
                .filter(|s| s.domains == want)
                .as_ref()
                .and_then(load_ca)
            {
                return Ok(ca);
            }
            let (stored, ca) = make_ca(&want).map_err(|e| e.to_string())?;
            let raw = serde_json::to_string(&stored).map_err(|e| e.to_string())?;
            secrets
                .set(CA_SECRET, &raw)
                .await
                .map_err(|e| e.to_string())?;
            Ok(ca)
        })
        .await
}

/// The CA certificate in PEM, for the sandbox's trust files.
pub async fn ca_pem(state: &AppState) -> Result<String, String> {
    let der = B64.encode(&ca(state).await?.cert);
    let lines: Vec<&str> = der
        .as_bytes()
        .chunks(64)
        .map(|c| std::str::from_utf8(c).unwrap_or_default())
        .collect();
    Ok(format!(
        "-----BEGIN CERTIFICATE-----\n{}\n-----END CERTIFICATE-----\n",
        lines.join("\n")
    ))
}

/// The TLS setup that presents `host` a certificate from the local CA.
async fn leaf(state: &AppState, host: &str) -> Result<Arc<ServerConfig>, String> {
    if let Some((made, config)) = lock(&state.egress.leaves).get(host)
        && made.elapsed() < LEAF_FOR
    {
        return Ok(config.clone());
    }
    let ca = ca(state).await?;
    let key = KeyPair::generate().map_err(|e| e.to_string())?;
    let mut params = CertificateParams::new(vec![host.to_owned()]).map_err(|e| e.to_string())?;
    let mut name = DistinguishedName::new();
    name.push(DnType::CommonName, host);
    params.distinguished_name = name;
    let now = time::OffsetDateTime::now_utc();
    params.not_before = now - time::Duration::days(1);
    params.not_after = now + time::Duration::days(30);
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    let cert = params
        .signed_by(&key, &ca.issuer)
        .map_err(|e| e.to_string())?;
    let chain = vec![cert.der().clone(), CertificateDer::from(ca.cert.clone())];
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.serialize_der()));
    let provider = Arc::new(tokio_rustls::rustls::crypto::aws_lc_rs::default_provider());
    let mut config = ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| e.to_string())?
        .with_no_client_auth()
        .with_single_cert(chain, key)
        .map_err(|e| e.to_string())?;
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    let config = Arc::new(config);
    lock(&state.egress.leaves).insert(host.to_owned(), (Instant::now(), config.clone()));
    Ok(config)
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Hands `CONNECT` requests to the proxy; everything else goes on to the agent API's routes.
pub async fn intercept(State(state): State<AppState>, req: Request, next: Next) -> Response {
    if req.method() == Method::CONNECT {
        connect(state, req).await
    } else {
        next.run(req).await
    }
}

fn refuse(status: StatusCode, reason: &str) -> Response {
    (status, reason.to_owned()).into_response()
}

/// The agent whose token is the proxy password (`Basic base64(user:token)`).
async fn proxy_agent(state: &AppState, headers: &HeaderMap) -> Option<ferrobot_core::Agent> {
    let value = headers.get(PROXY_AUTHORIZATION)?.to_str().ok()?;
    let (scheme, encoded) = value.split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("basic") {
        return None;
    }
    let decoded = String::from_utf8(B64.decode(encoded.trim()).ok()?).ok()?;
    let (_, token) = decoded.split_once(':')?;
    state.store.agent_by_token(token).await.ok().flatten()
}

async fn connect(state: AppState, mut req: Request) -> Response {
    let Some(agent) = proxy_agent(&state, req.headers()).await else {
        let mut response = refuse(
            StatusCode::PROXY_AUTHENTICATION_REQUIRED,
            "agent token required",
        );
        response.headers_mut().insert(
            PROXY_AUTHENTICATE,
            HeaderValue::from_static("Basic realm=\"ferrobot\""),
        );
        return response;
    };
    let Some(authority) = req.uri().authority().cloned() else {
        return refuse(StatusCode::BAD_REQUEST, "CONNECT needs host:port");
    };
    if authority.port_u16() != Some(443) {
        return refuse(StatusCode::FORBIDDEN, "only port 443");
    }
    let host = authority
        .host()
        .trim_matches(['[', ']'])
        .to_ascii_lowercase();
    let Some(upgrade) = req.extensions_mut().remove::<hyper::upgrade::OnUpgrade>() else {
        return refuse(StatusCode::BAD_REQUEST, "no tunnel");
    };
    if let Some(real) = crate::logins::for_host(&state, agent.id, &host).await {
        let tls = match leaf(&state, &host).await {
            Ok(tls) => tls,
            Err(e) => {
                tracing::warn!(%host, error = %e, "no certificate for a login host");
                return refuse(
                    StatusCode::BAD_GATEWAY,
                    "the login proxy has no certificate",
                );
            }
        };
        tokio::spawn(async move {
            if let Ok(io) = upgrade.await {
                swap_tunnel(state, io, tls, host, real).await;
            }
        });
    } else {
        let addr = match public_addr(&host).await {
            Ok(addr) => addr,
            Err(reason) => return refuse(StatusCode::FORBIDDEN, reason),
        };
        tokio::spawn(async move {
            if let Ok(io) = upgrade.await
                && let Ok(mut upstream) = tokio::net::TcpStream::connect(addr).await
            {
                let _ = tokio::io::copy_bidirectional(&mut TokioIo::new(io), &mut upstream).await;
            }
        });
    }
    StatusCode::OK.into_response()
}

/// `host`'s address if every address it resolves to is public; the tunnel uses that one, so a
/// second lookup cannot point it elsewhere.
async fn public_addr(host: &str) -> Result<SocketAddr, &'static str> {
    let addrs: Vec<SocketAddr> = tokio::net::lookup_host((host, 443))
        .await
        .map_err(|_| "unknown host")?
        .collect();
    if addrs.iter().any(|a| !is_public(a.ip())) {
        return Err("private addresses are not reachable through the proxy");
    }
    addrs.first().copied().ok_or("unknown host")
}

fn is_public(ip: IpAddr) -> bool {
    match ip.to_canonical() {
        IpAddr::V4(v4) => {
            let [a, b, ..] = v4.octets();
            !(v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || v4.is_documentation()
                || a == 0
                || (a == 100 && (64..128).contains(&b)))
        }
        IpAddr::V6(v6) => {
            let first = v6.segments()[0];
            !(v6.is_loopback()
                || v6.is_unspecified()
                || (first & 0xfe00) == 0xfc00
                || (first & 0xffc0) == 0xfe80)
        }
    }
}

/// Ends TLS for a login host and forwards each request with the real token.
async fn swap_tunnel(
    state: AppState,
    io: hyper::upgrade::Upgraded,
    tls: Arc<ServerConfig>,
    host: String,
    real: String,
) {
    let Ok(stream) = TlsAcceptor::from(tls).accept(TokioIo::new(io)).await else {
        return;
    };
    let service = hyper::service::service_fn(move |req| {
        forward(state.clone(), host.clone(), real.clone(), req)
    });
    let _ = hyper::server::conn::http1::Builder::new()
        .serve_connection(TokioIo::new(stream), service)
        .await;
}

fn plain(status: StatusCode, text: &'static str) -> hyper::Response<ProxyBody> {
    let mut response = hyper::Response::new(
        Full::new(Bytes::from_static(text.as_bytes()))
            .map_err(|never| match never {})
            .boxed(),
    );
    *response.status_mut() = status;
    response
}

async fn forward(
    state: AppState,
    host: String,
    real: String,
    req: hyper::Request<Incoming>,
) -> Result<hyper::Response<ProxyBody>, Infallible> {
    let base = state
        .settings
        .login_upstream
        .clone()
        .unwrap_or_else(|| format!("https://{host}"));
    let path = req.uri().path_and_query().map_or("/", |p| p.as_str());
    let url = format!("{}{path}", base.trim_end_matches('/'));
    let (parts, body) = req.into_parts();
    let mut headers = parts.headers;
    for name in HOP {
        headers.remove(name);
    }
    headers.remove(axum::http::header::HOST);
    if let Some(swapped) = headers.get(AUTHORIZATION).and_then(|v| swap(v, &real)) {
        headers.insert(AUTHORIZATION, swapped);
    }
    let http = state.egress.http.get_or_init(|| {
        reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(10))
            .build()
            .unwrap_or_default()
    });
    let sent = http
        .request(parts.method, url)
        .headers(headers)
        .body(reqwest::Body::wrap_stream(BodyDataStream::new(body)))
        .send()
        .await;
    let Ok(upstream) = sent else {
        return Ok(plain(StatusCode::BAD_GATEWAY, "the service did not answer"));
    };
    let status = upstream.status();
    let mut headers = upstream.headers().clone();
    for name in HOP {
        headers.remove(name);
    }
    let mut response = hyper::Response::new(
        StreamBody::new(
            upstream
                .bytes_stream()
                .map_ok(Frame::data)
                .map_err(|e| Box::new(e) as BoxError),
        )
        .boxed(),
    );
    *response.status_mut() = status;
    *response.headers_mut() = headers;
    Ok(response)
}

/// `value` with the stand-in replaced by `real`, if it carries the stand-in: `token X`,
/// `Bearer X`, or git's `Basic base64(user:X)`.
fn swap(value: &HeaderValue, real: &str) -> Option<HeaderValue> {
    let stand_in = crate::logins::STAND_IN;
    let (scheme, credential) = value.to_str().ok()?.split_once(' ')?;
    let swapped = match scheme.to_ascii_lowercase().as_str() {
        "token" | "bearer" if credential == stand_in => format!("{scheme} {real}"),
        "basic" => {
            let decoded = String::from_utf8(B64.decode(credential).ok()?).ok()?;
            match decoded.split_once(':') {
                Some((user, pass)) if pass == stand_in => {
                    format!("{scheme} {}", B64.encode(format!("{user}:{real}")))
                }
                _ => return None,
            }
        }
        _ => return None,
    };
    let mut header = HeaderValue::from_str(&swapped).ok()?;
    header.set_sensitive(true);
    Some(header)
}

/// What the sandbox's TLS clients trust: its own bundle plus the local CA.
pub async fn trust_files(
    state: &AppState,
    system_bundle: &str,
) -> Result<(String, String), String> {
    let ca = ca_pem(state).await?;
    let mut bundle = system_bundle.to_owned();
    if !bundle.ends_with('\n') && !bundle.is_empty() {
        bundle.push('\n');
    }
    bundle.push_str(&ca);
    Ok((ca, bundle))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_stand_in_is_swapped_in_every_form() {
        let real = "ghp_real";
        let stand_in = crate::logins::STAND_IN;
        let get = |v: &str| {
            swap(&HeaderValue::from_str(v).unwrap(), real).map(|h| h.to_str().unwrap().to_owned())
        };
        assert_eq!(
            get(&format!("token {stand_in}")).as_deref(),
            Some("token ghp_real")
        );
        assert_eq!(
            get(&format!("Bearer {stand_in}")).as_deref(),
            Some("Bearer ghp_real")
        );
        let basic = B64.encode(format!("x-access-token:{stand_in}"));
        let swapped = get(&format!("Basic {basic}")).unwrap();
        let decoded = B64.decode(swapped.trim_start_matches("Basic ")).unwrap();
        assert_eq!(decoded, b"x-access-token:ghp_real");
        assert_eq!(get("Bearer something-else"), None, "only the stand-in");
        assert_eq!(get(&format!("Basic {}", B64.encode("u:other"))), None);
    }

    #[test]
    fn private_addresses_are_not_public() {
        for ip in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.169.254",
            "100.64.0.1",
            "0.0.0.0",
            "::1",
            "fd00::1",
            "fe80::1",
            "::ffff:127.0.0.1",
        ] {
            assert!(!is_public(ip.parse().unwrap()), "{ip}");
        }
        for ip in ["140.82.112.3", "2606:50c0:8000::153"] {
            assert!(is_public(ip.parse().unwrap()), "{ip}");
        }
    }

    #[test]
    fn the_ca_reloads_and_signs_only_under_its_domains() {
        let domains = vec!["github.com".to_owned()];
        let (stored, made) = make_ca(&domains).unwrap();
        let loaded = load_ca(&stored).unwrap();
        assert_eq!(loaded.cert, made.cert);
        let key = KeyPair::generate().unwrap();
        let leaf = CertificateParams::new(vec!["api.github.com".to_owned()])
            .unwrap()
            .signed_by(&key, &loaded.issuer)
            .unwrap();
        assert!(!leaf.der().is_empty());
    }
}
