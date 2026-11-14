//! HTTPS Boot Mock for COMHEART/SCTY TY-FS803G CMIOT/Andlink IF5 onboarding.
//!
//! Serves `/espapi/m2m/rest/devices/cgw`, `/device/inform/bootstrap`,
//! `/device/inform/boot`, and `/device-manage/device/inform/cgwDmReport` over TLS,
//! recording the `(deviceId <-> deviceMac)` mapping for IF5 MQTT provisioning.
use bytes::Bytes;
use http_body_util::{BodyExt, Full, Limited};
use hyper::{
    body::Incoming, header, server::conn::http1, service::service_fn, Method, Request, Response,
    StatusCode,
};
use hyper_util::rt::TokioIo;
use ring::{
    rand::SystemRandom,
    signature::{EcdsaKeyPair, KeyPair, ECDSA_P256_SHA256_ASN1_SIGNING},
};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use rustls::ServerConfig;
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    net::{Ipv4Addr, SocketAddr},
    path::Path,
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::net::TcpListener;
use tokio::sync::{oneshot, Semaphore};
use tokio_rustls::TlsAcceptor;

const MAX_REQUEST_BYTES: usize = 64 * 1024;
const MAX_CONCURRENT_CONNS: usize = 32;
const IO_TIMEOUT: Duration = Duration::from_secs(10);
const DNSMASQ_HOSTS_PATH: &str = "/tmp/hosts/fttr-if5";
const CMIOT_CGW_HOST: &str = "open.home.komect.com";
const CMIOT_RGW_HOST: &str = "rgw.komect.com";

#[derive(Clone, Debug, Default)]
pub struct BootRecord {
    pub mac: String,
    pub device_id: String,
    pub device_sn: String,
    pub pon_sn: String,
    pub ip_address: String,
    pub fw_version: String,
    pub sw_version: String,
    pub last_bootstrap_at: u64,
    pub last_boot_at: u64,
}

#[derive(Default)]
pub struct BootRegistry {
    by_mac: HashMap<String, BootRecord>,
    mac_by_id: HashMap<String, String>,
}

impl BootRegistry {
    pub fn record_bootstrap(
        &mut self,
        mac: &str,
        device_sn: &str,
        pon_sn: &str,
        now: u64,
    ) -> BootRecord {
        let device_id = format!("IF5{mac}");
        let rec = self
            .by_mac
            .entry(mac.to_owned())
            .or_insert_with(|| BootRecord {
                mac: mac.to_owned(),
                device_id: device_id.clone(),
                ..Default::default()
            });
        rec.device_id = device_id.clone();
        if !device_sn.is_empty() {
            rec.device_sn = device_sn.to_owned();
        }
        if !pon_sn.is_empty() {
            rec.pon_sn = pon_sn.to_owned();
        }
        rec.last_bootstrap_at = now;
        self.mac_by_id.insert(device_id, mac.to_owned());
        rec.clone()
    }

    pub fn record_boot(
        &mut self,
        mac: &str,
        device_id: &str,
        ip: &str,
        fw: &str,
        sw: &str,
        device_sn: &str,
        pon_sn: &str,
        now: u64,
    ) -> BootRecord {
        let assigned_id = if device_id.is_empty() || device_id.len() > 31 {
            format!("IF5{mac}")
        } else {
            device_id.to_owned()
        };
        let rec = self
            .by_mac
            .entry(mac.to_owned())
            .or_insert_with(|| BootRecord {
                mac: mac.to_owned(),
                device_id: assigned_id.clone(),
                ..Default::default()
            });
        if rec.device_id != assigned_id && !rec.device_id.is_empty() {
            self.mac_by_id.remove(&rec.device_id);
        }
        rec.device_id = assigned_id.clone();
        if !ip.is_empty() {
            rec.ip_address = ip.to_owned();
        }
        if !fw.is_empty() {
            rec.fw_version = fw.to_owned();
        }
        if !sw.is_empty() {
            rec.sw_version = sw.to_owned();
        }
        if !device_sn.is_empty() {
            rec.device_sn = device_sn.to_owned();
        }
        if !pon_sn.is_empty() {
            rec.pon_sn = pon_sn.to_owned();
        }
        rec.last_boot_at = now;
        self.mac_by_id.insert(assigned_id, mac.to_owned());
        rec.clone()
    }

    pub fn lookup_mac(&self, device_id: &str) -> Option<String> {
        self.mac_by_id.get(device_id).cloned()
    }

    pub fn lookup_device_id(&self, mac: &str) -> Option<String> {
        self.by_mac.get(mac).map(|r| r.device_id.clone())
    }

    pub fn snapshot(&self, mac: &str) -> Option<BootRecord> {
        self.by_mac.get(mac).cloned()
    }
}

static REGISTRY: OnceLock<Mutex<BootRegistry>> = OnceLock::new();

pub fn shared_registry() -> &'static Mutex<BootRegistry> {
    REGISTRY.get_or_init(|| Mutex::new(BootRegistry::default()))
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[derive(Clone, Debug)]
pub struct BootMockConfig {
    pub rgw_base_url: String,
    pub mqtt_url: String,
    pub mqtt_user: String,
    pub mqtt_password: String,
    pub mqtt_keepalive: u32,
}

impl BootMockConfig {
    pub fn for_lan(lan_ip: Ipv4Addr, mqtt_port: u16) -> Self {
        Self {
            rgw_base_url: format!("https://{CMIOT_RGW_HOST}:443"),
            mqtt_url: format!("tcp://{lan_ip}:{mqtt_port}"),
            mqtt_user: "fttr".into(),
            mqtt_password: "fttr".into(),
            mqtt_keepalive: 60,
        }
    }
}

pub struct BootMockServer {
    local_addr: SocketAddr,
    _shutdown: oneshot::Sender<()>,
    handle: tokio::task::JoinHandle<Result<(), String>>,
}

impl BootMockServer {
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    pub fn is_finished(&self) -> bool {
        self.handle.is_finished()
    }

    pub async fn start(
        bind_addr: SocketAddr,
        mut config: BootMockConfig,
        san_ip: Option<Ipv4Addr>,
    ) -> Result<Self, String> {
        let listener = TcpListener::bind(bind_addr)
            .await
            .map_err(|e| format!("IF5 Boot Mock bind {bind_addr}: {e}"))?;
        let local_addr = listener
            .local_addr()
            .map_err(|e| format!("IF5 Boot Mock local_addr: {e}"))?;
        if let Ok(override_rgw) = std::env::var("FTTR_IF5_RGW_URL") {
            if !override_rgw.trim().is_empty() {
                config.rgw_base_url = override_rgw;
            }
        } else if bind_addr.port() == 0 {
            config.rgw_base_url = format!("https://{}:{}", local_addr.ip(), local_addr.port());
        }
        if let Ok(override_mqtt) = std::env::var("FTTR_IF5_MQTT_URL") {
            if !override_mqtt.trim().is_empty() {
                config.mqtt_url = override_mqtt;
            }
        }

        let (cert, key) = generate_self_signed_cert(san_ip)?;
        let server_config =
            ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
                .with_safe_default_protocol_versions()
                .map_err(|e| format!("IF5 TLS protocol versions: {e}"))?
                .with_no_client_auth()
                .with_single_cert(vec![cert], key)
                .map_err(|e| format!("IF5 TLS server cert: {e}"))?;
        let acceptor = TlsAcceptor::from(Arc::new(server_config));

        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let config = Arc::new(config);
        let handle = tokio::spawn(serve_loop(listener, acceptor, config, shutdown_rx));
        Ok(Self {
            local_addr,
            _shutdown: shutdown_tx,
            handle,
        })
    }
}

/// Starts the production IF5 Boot Mock + OpenWrt dnsmasq host override when
/// configured via `FTTR_MQTT_LAN_ADDR` (and not disabled by `FTTR_IF5_BOOT_MOCK_ADDR=disabled`).
pub async fn start_from_env(lan_addr: SocketAddr) -> Result<Option<BootMockServer>, String> {
    let std::net::IpAddr::V4(lan_ip) = lan_addr.ip() else {
        return Err("IF5 Boot Mock requires IPv4 LAN address".into());
    };
    let bind_addr = match std::env::var("FTTR_IF5_BOOT_MOCK_ADDR") {
        Ok(raw) if raw.trim().eq_ignore_ascii_case("disabled") => return Ok(None),
        Ok(raw) if !raw.trim().is_empty() => raw
            .trim()
            .parse::<SocketAddr>()
            .map_err(|e| format!("invalid FTTR_IF5_BOOT_MOCK_ADDR: {e}"))?,
        _ => SocketAddr::from((lan_ip, 443)),
    };

    #[cfg(not(test))]
    if bind_addr.port() == 443 {
        if let Err(err) = ensure_dnsmasq_hosts(lan_ip, Path::new(DNSMASQ_HOSTS_PATH)) {
            eprintln!("[-] IF5 dnsmasq hosts update skipped: {err}");
        }
    }

    let config = BootMockConfig::for_lan(lan_ip, lan_addr.port());
    let server = BootMockServer::start(bind_addr, config, Some(lan_ip)).await?;
    println!(
        "[+] SCTY IF5 Boot Mock HTTPS ready on {}",
        server.local_addr()
    );
    Ok(Some(server))
}

#[cfg_attr(test, allow(dead_code))]
fn lan_ipv6_from_proc() -> Option<std::net::Ipv6Addr> {
    let raw = std::fs::read_to_string("/proc/net/if_inet6").ok()?;
    let mut fallback = None;
    for line in raw.lines() {
        let mut parts = line.split_whitespace();
        let (Some(hex), _, _, Some(scope), _, Some(iface)) = (
            parts.next(),
            parts.next(),
            parts.next(),
            parts.next(),
            parts.next(),
            parts.next(),
        ) else {
            continue;
        };
        if scope != "00" || hex.len() != 32 || iface != "br-lan" {
            continue;
        }
        if let Ok(bits) = u128::from_str_radix(hex, 16) {
            let addr = std::net::Ipv6Addr::from(bits);
            if !addr.is_loopback() && !addr.is_unspecified() {
                // Prefer ULA (fc00::/7) so sub-ONUs on br-lan can reach it on-link.
                if (addr.segments()[0] & 0xfe00) == 0xfc00 {
                    return Some(addr);
                }
                fallback.get_or_insert(addr);
            }
        }
    }
    fallback
}

#[cfg_attr(test, allow(dead_code))]
fn ensure_dnsmasq_hosts(lan_ip: Ipv4Addr, path: &Path) -> Result<(), String> {
    let Some(dir) = path.parent() else {
        return Ok(());
    };
    if !dir.is_dir() {
        return Ok(());
    }
    let ipv6_line = lan_ipv6_from_proc()
        .map(|ip6| format!("{ip6} {CMIOT_CGW_HOST} {CMIOT_RGW_HOST}\n"))
        .unwrap_or_default();
    let content = format!(
        "# managed by fttrd for CMIOT IF5 Boot Mock\n{lan_ip} {CMIOT_CGW_HOST} {CMIOT_RGW_HOST}\n{ipv6_line}"
    );
    let unchanged = std::fs::read_to_string(path).ok().as_deref() == Some(content.as_str());
    if !unchanged {
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, content.as_bytes()).map_err(|e| format!("write hosts tmp: {e}"))?;
        std::fs::rename(&tmp, path).map_err(|e| format!("rename hosts: {e}"))?;
        let status = std::process::Command::new("/etc/init.d/dnsmasq")
            .arg("reload")
            .status()
            .map_err(|e| format!("dnsmasq reload: {e}"))?;
        if !status.success() {
            return Err(format!("dnsmasq reload exited {status}"));
        }
    }
    Ok(())
}

async fn serve_loop(
    listener: TcpListener,
    acceptor: TlsAcceptor,
    config: Arc<BootMockConfig>,
    mut shutdown: oneshot::Receiver<()>,
) -> Result<(), String> {
    let limit = Arc::new(Semaphore::new(MAX_CONCURRENT_CONNS));
    loop {
        tokio::select! {
            _ = &mut shutdown => return Ok(()),
            accepted = listener.accept() => {
                let (tcp, _peer) = match accepted {
                    Ok(pair) => pair,
                    Err(e) => return Err(format!("IF5 Boot Mock accept error: {e}")),
                };
                let Ok(permit) = limit.clone().try_acquire_owned() else {
                    drop(tcp);
                    continue;
                };
                let acceptor = acceptor.clone();
                let config = config.clone();
                tokio::spawn(async move {
                    let _permit = permit;
                    let Ok(Ok(tls)) = tokio::time::timeout(IO_TIMEOUT, acceptor.accept(tcp)).await else {
                        return;
                    };
                    let io = TokioIo::new(tls);
                    let service = service_fn(move |req| handle_http(req, config.clone()));
                    let _ = tokio::time::timeout(
                        Duration::from_secs(15),
                        http1::Builder::new().serve_connection(io, service),
                    )
                    .await;
                });
            }
        }
    }
}

async fn handle_http(
    req: Request<Incoming>,
    config: Arc<BootMockConfig>,
) -> Result<Response<Full<Bytes>>, hyper::Error> {
    let path = req.uri().path().to_owned();
    let method = req.method().clone();
    let body = match Limited::new(req.into_body(), MAX_REQUEST_BYTES)
        .collect()
        .await
    {
        Ok(collected) => collected.to_bytes(),
        Err(_) => {
            return Ok(json_response(
                StatusCode::PAYLOAD_TOO_LARGE,
                &json!({"respCode": 1000, "respCont": "Payload too large"}),
            ));
        }
    };

    if method != Method::POST {
        return Ok(json_response(
            StatusCode::METHOD_NOT_ALLOWED,
            &json!({"respCode": 1000, "respCont": "POST required"}),
        ));
    }

    let parsed: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let response = dispatch_post(&path, &parsed, &config, shared_registry(), now_secs());
    Ok(response)
}

fn dispatch_post(
    path: &str,
    body: &Value,
    config: &BootMockConfig,
    registry: &Mutex<BootRegistry>,
    now: u64,
) -> Response<Full<Bytes>> {
    match path {
        "/espapi/m2m/rest/devices/cgw" => json_response(
            StatusCode::OK,
            &json!({
                "resultCode": "0",
                "gwAddress2": config.rgw_base_url,
            }),
        ),
        "/device/inform/bootstrap" => {
            let Some(raw_mac) = body["deviceMac"].as_str() else {
                return json_response(
                    StatusCode::BAD_REQUEST,
                    &json!({"respCode": 1000, "respCont": "Missing deviceMac"}),
                );
            };
            let Some(mac) = crate::mqtt_identity::normalize_target(raw_mac) else {
                return json_response(
                    StatusCode::BAD_REQUEST,
                    &json!({"respCode": 1000, "respCont": "Invalid deviceMac"}),
                );
            };
            let device_sn = body["deviceExtInfo"]["deviceSn"].as_str().unwrap_or("");
            let pon_sn = body["deviceExtInfo"]["ponSn"].as_str().unwrap_or("");
            let rec = registry
                .lock()
                .unwrap()
                .record_bootstrap(&mac, device_sn, pon_sn, now);
            json_response(
                StatusCode::OK,
                &json!({
                    "gwToken": "fttr-gw-token",
                    "deviceid": rec.device_id,
                    "deviceToken": "0000000000000000",
                    "andLinkToken": "fttr-andlink-token",
                    "dmToken": "fttr-dm-token",
                }),
            )
        }
        "/device/inform/boot" => {
            let Some(raw_mac) = body["deviceMac"].as_str() else {
                return json_response(
                    StatusCode::BAD_REQUEST,
                    &json!({"respCode": 1000, "respCont": "Missing deviceMac"}),
                );
            };
            let Some(mac) = crate::mqtt_identity::normalize_target(raw_mac) else {
                return json_response(
                    StatusCode::BAD_REQUEST,
                    &json!({"respCode": 1000, "respCont": "Invalid deviceMac"}),
                );
            };
            let device_id = body["deviceId"].as_str().unwrap_or("");
            let ip = body["ipAddress"].as_str().unwrap_or("");
            let fw = body["firmwareVersion"].as_str().unwrap_or("");
            let sw = body["softwareVersion"].as_str().unwrap_or("");
            let device_sn = body["XData"]["deviceSn"].as_str().unwrap_or("");
            let pon_sn = body["XData"]["ponSn"].as_str().unwrap_or("");
            let rec = registry
                .lock()
                .unwrap()
                .record_boot(&mac, device_id, ip, fw, sw, device_sn, pon_sn, now);
            crate::provisioning::observe_mqtt(&mac, now);
            json_response(
                StatusCode::OK,
                &json!({
                    "userBind": 1,
                    "mqttUrl": config.mqtt_url,
                    "mqttClientId": rec.device_id,
                    "mqttUser": config.mqtt_user,
                    "mqttPassword": config.mqtt_password,
                    "mqttKeepalive": config.mqtt_keepalive,
                    "mqttRetry": [5, 10, 30],
                    "httpsUrl": config.rgw_base_url,
                }),
            )
        }
        "/device-manage/device/inform/cgwDmReport" => {
            json_response(StatusCode::OK, &json!({"resultCode": "0"}))
        }
        _ => json_response(
            StatusCode::NOT_FOUND,
            &json!({"respCode": 1000, "respCont": "Unsupported IF5 endpoint"}),
        ),
    }
}

fn json_response(status: StatusCode, body: &Value) -> Response<Full<Bytes>> {
    let encoded = serde_json::to_vec(body).unwrap_or_else(|_| b"{}".to_vec());
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "application/json; charset=utf-8")
        .header(header::CONTENT_LENGTH, encoded.len())
        .header(header::CONNECTION, "close")
        .body(Full::new(Bytes::from(encoded)))
        .expect("static HTTP response builder is valid")
}

// --- Minimal ASN.1 DER encoder for a self-signed P-256 ECDSA X.509 v3 cert ---

fn der_tlv(tag: u8, value: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(value.len() + 4);
    out.push(tag);
    let len = value.len();
    if len < 0x80 {
        out.push(len as u8);
    } else if len <= 0xff {
        out.push(0x81);
        out.push(len as u8);
    } else {
        out.push(0x82);
        out.push((len >> 8) as u8);
        out.push((len & 0xff) as u8);
    }
    out.extend_from_slice(value);
    out
}

fn der_seq(parts: &[&[u8]]) -> Vec<u8> {
    let mut inner = Vec::new();
    for p in parts {
        inner.extend_from_slice(p);
    }
    der_tlv(0x30, &inner)
}

fn der_set(inner: &[u8]) -> Vec<u8> {
    der_tlv(0x31, inner)
}

fn der_bit_string(bytes: &[u8]) -> Vec<u8> {
    let mut inner = Vec::with_capacity(bytes.len() + 1);
    inner.push(0x00); // unused bits
    inner.extend_from_slice(bytes);
    der_tlv(0x03, &inner)
}

fn generate_self_signed_cert(
    san_ip: Option<Ipv4Addr>,
) -> Result<(CertificateDer<'static>, PrivateKeyDer<'static>), String> {
    const OID_ECDSA_SHA256: &[u8] = &[0x06, 0x08, 0x2a, 0x86, 0x48, 0xce, 0x3d, 0x04, 0x03, 0x02];
    const OID_EC_PUBLIC_KEY: &[u8] = &[0x06, 0x07, 0x2a, 0x86, 0x48, 0xce, 0x3d, 0x02, 0x01];
    const OID_PRIME256V1: &[u8] = &[0x06, 0x08, 0x2a, 0x86, 0x48, 0xce, 0x3d, 0x03, 0x01, 0x07];
    const OID_COMMON_NAME: &[u8] = &[0x06, 0x03, 0x55, 0x04, 0x03];
    const OID_SUBJECT_ALT_NAME: &[u8] = &[0x06, 0x03, 0x55, 0x1d, 0x11];

    let rng = SystemRandom::new();
    let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, &rng)
        .map_err(|_| "generate P-256 keypair")?;
    let key_pair = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, pkcs8.as_ref(), &rng)
        .map_err(|_| "parse generated P-256 pkcs8")?;

    let version = der_tlv(0xa0, &der_tlv(0x02, &[0x02])); // v3
    let serial = der_tlv(0x02, &[0x01]);
    let sig_alg = der_seq(&[OID_ECDSA_SHA256]);

    let cn_attr = der_seq(&[OID_COMMON_NAME, &der_tlv(0x0c, CMIOT_CGW_HOST.as_bytes())]);
    let name = der_seq(&[&der_set(&cn_attr)]);

    let validity = der_seq(&[
        &der_tlv(0x17, b"240101000000Z"),
        &der_tlv(0x17, b"491231235959Z"),
    ]);

    let spki_alg = der_seq(&[OID_EC_PUBLIC_KEY, OID_PRIME256V1]);
    let spki = der_seq(&[&spki_alg, &der_bit_string(key_pair.public_key().as_ref())]);

    let dns_cgw = der_tlv(0x82, CMIOT_CGW_HOST.as_bytes());
    let dns_rgw = der_tlv(0x82, CMIOT_RGW_HOST.as_bytes());
    let mut san_items: Vec<&[u8]> = vec![&dns_cgw, &dns_rgw];
    let ip_entry;
    if let Some(ip) = san_ip {
        ip_entry = der_tlv(0x87, &ip.octets());
        san_items.push(&ip_entry);
    }
    let san_val = der_tlv(0x04, &der_seq(&san_items));
    let san_ext = der_seq(&[OID_SUBJECT_ALT_NAME, &san_val]);
    let exts = der_tlv(0xa3, &der_seq(&[&san_ext]));

    let tbs = der_seq(&[
        &version, &serial, &sig_alg, &name, &validity, &name, &spki, &exts,
    ]);
    let sig = key_pair
        .sign(&rng, &tbs)
        .map_err(|_| "sign self-signed X.509 certificate")?;
    let cert_der = der_seq(&[&tbs, &sig_alg, &der_bit_string(sig.as_ref())]);

    Ok((
        CertificateDer::from(cert_der),
        PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(pkcs8.as_ref().to_vec())),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
    use rustls::pki_types::{ServerName, UnixTime};
    use rustls::{ClientConfig, DigitallySignedStruct, SignatureScheme};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[derive(Debug)]
    struct TrustAny;
    impl ServerCertVerifier for TrustAny {
        fn verify_server_cert(
            &self,
            _end_entity: &CertificateDer<'_>,
            _intermediates: &[CertificateDer<'_>],
            _server_name: &ServerName<'_>,
            _ocsp_response: &[u8],
            _now: UnixTime,
        ) -> Result<ServerCertVerified, rustls::Error> {
            Ok(ServerCertVerified::assertion())
        }
        fn verify_tls12_signature(
            &self,
            _message: &[u8],
            _cert: &CertificateDer<'_>,
            _dss: &DigitallySignedStruct,
        ) -> Result<HandshakeSignatureValid, rustls::Error> {
            Ok(HandshakeSignatureValid::assertion())
        }
        fn verify_tls13_signature(
            &self,
            _message: &[u8],
            _cert: &CertificateDer<'_>,
            _dss: &DigitallySignedStruct,
        ) -> Result<HandshakeSignatureValid, rustls::Error> {
            Ok(HandshakeSignatureValid::assertion())
        }
        fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
            vec![
                SignatureScheme::ECDSA_NISTP256_SHA256,
                SignatureScheme::RSA_PSS_SHA256,
            ]
        }
    }

    #[test]
    fn boot_mock_https_serves_cgw_bootstrap_boot_without_respcode_on_success() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let mut cfg = BootMockConfig::for_lan(Ipv4Addr::new(192, 0, 2, 1), 1883);
            cfg.rgw_base_url = "https://rgw.komect.com:443".into();
            let server = BootMockServer::start(
                "127.0.0.1:0".parse().unwrap(),
                cfg,
                Some(Ipv4Addr::LOCALHOST),
            )
            .await
            .unwrap();

            let client_cfg = ClientConfig::builder_with_provider(Arc::new(
                rustls::crypto::ring::default_provider(),
            ))
            .with_safe_default_protocol_versions()
            .unwrap()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(TrustAny))
            .with_no_client_auth();
            let connector = tokio_rustls::TlsConnector::from(Arc::new(client_cfg));

            async fn post(
                connector: &tokio_rustls::TlsConnector,
                addr: SocketAddr,
                path: &str,
                body: &str,
            ) -> (u16, String) {
                let tcp = tokio::net::TcpStream::connect(addr).await.unwrap();
                let domain = ServerName::try_from("open.home.komect.com").unwrap();
                let mut tls = connector.connect(domain, tcp).await.unwrap();
                let req = format!(
                    "POST {path} HTTP/1.1\r\nHost: open.home.komect.com\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                tls.write_all(req.as_bytes()).await.unwrap();
                let mut raw = Vec::new();
                tls.read_to_end(&mut raw).await.unwrap();
                let text = String::from_utf8(raw).unwrap();
                let (head, payload) = text.split_once("\r\n\r\n").unwrap();
                let code = head.split_whitespace().nth(1).unwrap().parse().unwrap();
                (code, payload.to_owned())
            }

            let (c1, b1) = post(
                &connector,
                server.local_addr(),
                "/espapi/m2m/rest/devices/cgw",
                r#"{"deviceType":"FTTR"}"#,
            )
            .await;
            assert_eq!(c1, 200);
            assert!(!b1.contains("respCode"));
            assert!(b1.contains("gwAddress2"));

            let (c2, b2) = post(
                &connector,
                server.local_addr(),
                "/device/inform/bootstrap",
                r#"{"deviceMac":"02:aa:bb:cc:dd:ee","deviceExtInfo":{"deviceSn":"TESTSN0001"}}"#,
            )
            .await;
            assert_eq!(c2, 200);
            assert!(!b2.contains("respCode"));
            let j2: Value = serde_json::from_str(&b2).unwrap();
            assert_eq!(j2["deviceid"], "IF502AABBCCDDEE");
            assert!(j2["gwToken"].is_string());
            assert!(j2["deviceToken"].is_string());
            assert!(j2["andLinkToken"].is_string());

            let (c3, b3) = post(
                &connector,
                server.local_addr(),
                "/device/inform/boot",
                r#"{"deviceId":"IF502AABBCCDDEE","deviceMac":"02:aa:bb:cc:dd:ee","ipAddress":"192.0.2.50"}"#,
            )
            .await;
            assert_eq!(c3, 200);
            assert!(!b3.contains("respCode"));
            let j3: Value = serde_json::from_str(&b3).unwrap();
            assert_eq!(j3["mqttUrl"], "tcp://192.0.2.1:1883");
            assert_eq!(j3["mqttClientId"], "IF502AABBCCDDEE");
            assert_eq!(j3["mqttKeepalive"], 60);
            assert_eq!(
                shared_registry().lock().unwrap().lookup_mac("IF502AABBCCDDEE").as_deref(),
                Some("02AABBCCDDEE")
            );
        });
    }
}
