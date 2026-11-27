//! The CLI-login proxy on the agent listener. Sandboxes with a login send HTTPS through it
//! (`HTTPS_PROXY`, the agent token as the proxy password). For a login's hosts it ends TLS with a
//! certificate from that login's local CA, swaps its stand-in for the real token in the request
//! headers and forwards to the service (public addresses only) without following redirects; any
//! other public host gets a plain tunnel, and private addresses none. The real token and the CA key never enter a sandbox, nor any log.

use std::collections::HashMap;
use std::convert::Infallible;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::body::Bytes;
use axum::extract::{Request, State};
use axum::http::header::{PROXY_AUTHENTICATE, PROXY_AUTHORIZATION};
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
use crate::logins::Login;

type BoxError = Box<dyn std::error::Error + Send + Sync>;
type ProxyBody = BoxBody<Bytes, BoxError>;

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

/// Each login's local CA and the leaf certificates made with them.
#[derive(Default)]
pub struct Egress {
    cas: tokio::sync::Mutex<HashMap<String, Arc<Ca>>>,
    leaves: Mutex<HashMap<String, Leaf>>,
}

struct Leaf {
    made: Instant,
    config: Arc<ServerConfig>,
    /// The CA it was made with; a login's new CA makes new leaves.
    ca: Arc<Ca>,
}

impl std::fmt::Debug for Egress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Egress").finish_non_exhaustive()
    }
}

struct Ca {
    cert: Vec<u8>,
    issuer: Issuer<'static, KeyPair>,
    domains: Vec<String>,
}

#[derive(Serialize, Deserialize)]
struct StoredCa {
    key: String,
    cert: String,
    domains: Vec<String>,
    #[serde(default)]
    name: String,
}

/// Each login's CA has its own name: TLS clients look the issuer up by name, so CAs sharing
/// one made clients try the wrong CA.
fn ca_name(login: &str) -> String {
    format!("ferrobot CLI login {login}")
}

/// Secret store entry of a login's CA: its key and certificate.
fn ca_secret(login: &str) -> String {
    format!("{}:ca", crate::logins::secret_key(login))
}

/// The CA's fixed fields; an issuer needs the same name and key usages as its certificate.
fn ca_params(name: &str, domains: &[String]) -> CertificateParams {
    let mut params = CertificateParams::default();
    let mut dn = DistinguishedName::new();
    dn.push(DnType::CommonName, name);
    params.distinguished_name = dn;
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

fn make_ca(name: &str, domains: &[String]) -> Result<(StoredCa, Ca), rcgen::Error> {
    let key = KeyPair::generate()?;
    let mut params = ca_params(name, domains);
    let now = time::OffsetDateTime::now_utc();
    params.not_before = now - time::Duration::days(1);
    params.not_after = now + time::Duration::days(3650);
    let cert = params.self_signed(&key)?.der().to_vec();
    let stored = StoredCa {
        key: B64.encode(key.serialize_der()),
        cert: B64.encode(&cert),
        domains: domains.to_vec(),
        name: name.to_owned(),
    };
    Ok((
        stored,
        Ca {
            cert,
            issuer: Issuer::new(params, key),
            domains: domains.to_vec(),
        },
    ))
}

fn load_ca(stored: &StoredCa) -> Option<Ca> {
    let key = KeyPair::try_from(B64.decode(&stored.key).ok()?.as_slice()).ok()?;
    let cert = B64.decode(&stored.cert).ok()?;
    Some(Ca {
        cert,
        issuer: Issuer::new(ca_params(&stored.name, &stored.domains), key),
        domains: stored.domains.clone(),
    })
}

/// The login's CA, kept in the secret store and limited to its hosts; changed hosts make a new
/// one, so a CA never vouches for more than its login.
async fn ca(state: &AppState, login: &Login) -> Result<Arc<Ca>, String> {
    let mut cas = state.egress.cas.lock().await;
    if let Some(ca) = cas.get(&login.id).filter(|c| c.domains == login.hosts) {
        return Ok(ca.clone());
    }
    let secrets = &state.settings.secrets;
    let key = ca_secret(&login.id);
    let stored = secrets.get(&key).await.map_err(|e| e.to_string())?;
    let loaded = stored
        .and_then(|raw| serde_json::from_str::<StoredCa>(&raw).ok())
        .filter(|s| s.domains == login.hosts && s.name == ca_name(&login.id))
        .as_ref()
        .and_then(load_ca);
    let ca = if let Some(ca) = loaded {
        ca
    } else {
        let (stored, ca) = make_ca(&ca_name(&login.id), &login.hosts).map_err(|e| e.to_string())?;
        let raw = serde_json::to_string(&stored).map_err(|e| e.to_string())?;
        secrets.set(&key, &raw).await.map_err(|e| e.to_string())?;
        ca
    };
    let ca = Arc::new(ca);
    cas.insert(login.id.clone(), ca.clone());
    Ok(ca)
}

/// Forgets a deleted login's CA.
pub async fn forget(state: &AppState, login: &str) -> Result<(), String> {
    state.egress.cas.lock().await.remove(login);
    state
        .settings
        .secrets
        .delete(&ca_secret(login))
        .await
        .map_err(|e| e.to_string())
}

fn pem(der: &[u8]) -> String {
    let der = B64.encode(der);
    let lines: Vec<&str> = der
        .as_bytes()
        .chunks(64)
        .map(|c| std::str::from_utf8(c).unwrap_or_default())
        .collect();
    format!(
        "-----BEGIN CERTIFICATE-----\n{}\n-----END CERTIFICATE-----\n",
        lines.join("\n")
    )
}

/// The TLS setup that presents `host` a certificate from `login`'s CA.
async fn leaf(state: &AppState, login: &Login, host: &str) -> Result<Arc<ServerConfig>, String> {
    let ca = ca(state, login).await?;
    if let Some(leaf) = crate::lock(&state.egress.leaves).get(host)
        && leaf.made.elapsed() < LEAF_FOR
        && Arc::ptr_eq(&leaf.ca, &ca)
    {
        return Ok(leaf.config.clone());
    }
    let key = KeyPair::generate().map_err(|e| e.to_string())?;
    let mut params = CertificateParams::new(vec![host.to_owned()]).map_err(|e| e.to_string())?;
    let mut name = DistinguishedName::new();
    name.push(DnType::CommonName, host);
    params.distinguished_name = name;
    let now = time::OffsetDateTime::now_utc();
    params.not_before = now - time::Duration::days(1);
    params.not_after = now + time::Duration::days(30);
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    params.use_authority_key_identifier_extension = true;
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
    crate::lock(&state.egress.leaves).insert(
        host.to_owned(),
        Leaf {
            made: Instant::now(),
            config: config.clone(),
            ca,
        },
    );
    Ok(config)
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
    if let Some((login, real)) = crate::logins::for_host(&state, agent.id, &host).await {
        let http = match login_client(&state, &host).await {
            Ok(http) => http,
            Err(reason) => return refuse(StatusCode::FORBIDDEN, reason),
        };
        let tls = match leaf(&state, &login, &host).await {
            Ok(tls) => tls,
            Err(e) => {
                tracing::warn!(%host, error = %e, "no certificate for a login host");
                return refuse(
                    StatusCode::BAD_GATEWAY,
                    "the login proxy has no certificate",
                );
            }
        };
        let swap = Swap {
            base: state
                .settings
                .login_upstream
                .clone()
                .unwrap_or_else(|| format!("https://{host}")),
            http,
            stand_in: login.stand_in(),
            real,
        };
        tokio::spawn(async move {
            if let Ok(io) = upgrade.await {
                swap_tunnel(io, tls, Arc::new(swap)).await;
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

/// The client for a login host's requests: its address looked up once and required to be
/// public, so the real token cannot be sent to this computer or the local network.
async fn login_client(state: &AppState, host: &str) -> Result<reqwest::Client, &'static str> {
    let builder = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(10));
    let builder = if state.settings.login_upstream.is_some() {
        builder
    } else {
        builder.resolve(host, public_addr(host).await?)
    };
    builder
        .build()
        .map_err(|_| "the login proxy cannot reach the service")
}

/// Where a login tunnel forwards to and which token it swaps.
struct Swap {
    base: String,
    http: reqwest::Client,
    stand_in: String,
    real: String,
}

/// Ends TLS for a login host and forwards each request with the real token.
async fn swap_tunnel(io: hyper::upgrade::Upgraded, tls: Arc<ServerConfig>, swap: Arc<Swap>) {
    let Ok(stream) = TlsAcceptor::from(tls).accept(TokioIo::new(io)).await else {
        return;
    };
    let service = hyper::service::service_fn(move |req| forward(swap.clone(), req));
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
    swap: Arc<Swap>,
    req: hyper::Request<Incoming>,
) -> Result<hyper::Response<ProxyBody>, Infallible> {
    // Only headers carry the token: a swapped URL could come back in a redirect or error page.
    let path = req.uri().path_and_query().map_or("/", |p| p.as_str());
    let url = format!("{}{path}", swap.base.trim_end_matches('/'));
    let (parts, body) = req.into_parts();
    let mut headers = parts.headers;
    for name in HOP {
        headers.remove(name);
    }
    headers.remove(axum::http::header::HOST);
    swap_credentials(&mut headers, &swap.stand_in, &swap.real);
    let sent = swap
        .http
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
    // A service that echoes request headers must not hand the sandbox the real token.
    replace_in(&mut headers, &swap.real, &swap.stand_in);
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

/// Request headers that carry the real token. Only these: a service that repeats some other
/// header the agent sets (an error quoting `User-Agent`) would hand the token back.
const CREDENTIAL_HEADERS: [&str; 4] = ["authorization", "x-api-key", "private-token", "x-auth-token"];

/// [`replace_in`] for the credential headers only.
fn swap_credentials(headers: &mut HeaderMap, from: &str, to: &str) {
    for name in CREDENTIAL_HEADERS {
        if let axum::http::header::Entry::Occupied(mut entry) = headers.entry(name) {
            for value in entry.iter_mut() {
                if let Some(replaced) = replace(value, from, to) {
                    *value = replaced;
                }
            }
        }
    }
}

/// Replaces `from` with `to` in every header value that carries it, also inside `Basic`
/// credentials (git's `base64(user:token)`); replaced values are marked sensitive.
fn replace_in(headers: &mut HeaderMap, from: &str, to: &str) {
    for value in headers.values_mut() {
        if let Some(replaced) = replace(value, from, to) {
            *value = replaced;
        }
    }
}

fn replace(value: &HeaderValue, from: &str, to: &str) -> Option<HeaderValue> {
    let text = value.to_str().ok()?;
    let replaced = if text.contains(from) {
        text.replace(from, to)
    } else {
        let (scheme, credential) = text.split_once(' ')?;
        if !scheme.eq_ignore_ascii_case("basic") {
            return None;
        }
        let decoded = String::from_utf8(B64.decode(credential.trim()).ok()?).ok()?;
        if !decoded.contains(from) {
            return None;
        }
        format!("{scheme} {}", B64.encode(decoded.replace(from, to)))
    };
    let mut header = HeaderValue::from_str(&replaced).ok()?;
    header.set_sensitive(true);
    Some(header)
}

/// What the sandbox's TLS clients trust: its own bundle plus the CAs of its `logins`.
pub async fn trust_files(
    state: &AppState,
    system_bundle: &str,
    logins: &[Login],
) -> Result<(String, String), String> {
    let mut cas = String::new();
    for login in logins {
        cas.push_str(&pem(&ca(state, login).await?.cert));
    }
    let mut bundle = system_bundle.to_owned();
    if !bundle.ends_with('\n') && !bundle.is_empty() {
        bundle.push('\n');
    }
    bundle.push_str(&cas);
    Ok((cas, bundle))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_stand_in_is_swapped_in_every_form() {
        let real = "ghp_real";
        let stand_in = "ferrobot-stand-in-github";
        let get = |v: &str| {
            replace(&HeaderValue::from_str(v).unwrap(), stand_in, real)
                .map(|h| h.to_str().unwrap().to_owned())
        };
        assert_eq!(
            get(&format!("token {stand_in}")).as_deref(),
            Some("token ghp_real")
        );
        assert_eq!(
            get(&format!("Bearer {stand_in}")).as_deref(),
            Some("Bearer ghp_real")
        );
        assert_eq!(
            get(stand_in).as_deref(),
            Some("ghp_real"),
            "a bare key header"
        );
        let basic = B64.encode(format!("x-access-token:{stand_in}"));
        let swapped = get(&format!("Basic {basic}")).unwrap();
        let decoded = B64.decode(swapped.trim_start_matches("Basic ")).unwrap();
        assert_eq!(decoded, b"x-access-token:ghp_real");
        assert_eq!(get("Bearer something-else"), None, "only the stand-in");
        assert_eq!(
            get("Bearer ferrobot-stand-in-example"),
            None,
            "not another login's"
        );
        assert_eq!(get(&format!("Basic {}", B64.encode("u:other"))), None);
        assert!(
            replace(&HeaderValue::from_static("Bearer x"), "x", "y")
                .unwrap()
                .is_sensitive()
        );
    }

    #[test]
    fn the_token_goes_only_into_credential_headers() {
        let stand_in = "ferrobot-stand-in-example";
        let mut headers = HeaderMap::new();
        for (name, value) in [
            ("authorization", format!("Bearer {stand_in}")),
            ("x-api-key", stand_in.to_owned()),
            ("user-agent", stand_in.to_owned()),
            ("accept", format!("{stand_in}/json")),
        ] {
            headers.insert(name, HeaderValue::from_str(&value).unwrap());
        }
        swap_credentials(&mut headers, stand_in, "real");
        assert_eq!(headers["authorization"], "Bearer real");
        assert_eq!(headers["x-api-key"], "real");
        assert_eq!(headers["user-agent"], stand_in, "a header a service may echo back");
        assert_eq!(headers["accept"], format!("{stand_in}/json").as_str());
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
        let (stored, made) = make_ca(&ca_name("github"), &domains).unwrap();
        let loaded = load_ca(&stored).unwrap();
        assert_eq!(loaded.cert, made.cert);
        let named = |der: &[u8], name: &str| der.windows(name.len()).any(|w| w == name.as_bytes());
        assert!(named(&made.cert, "ferrobot CLI login github"));
        let (_, other) = make_ca(&ca_name("example"), &["api.example.com".into()]).unwrap();
        assert!(
            !named(&other.cert, "login github"),
            "every login's CA has its own name"
        );
        let key = KeyPair::generate().unwrap();
        let leaf = CertificateParams::new(vec!["api.github.com".to_owned()])
            .unwrap()
            .signed_by(&key, &loaded.issuer)
            .unwrap();
        assert!(!leaf.der().is_empty());
    }
}
