//! 區網攝影機掃描：同時使用
//! - ONVIF WS-Discovery（UDP 3702）：所有支援 ONVIF 的攝影機（Tapo 需開啟「第三方相容性」）
//! - TP-Link 裝置探索（UDP 20002，TDP）：所有 Tapo 裝置，即使沒開 ONVIF 也找得到，並回報型號、韌體、MAC
//!
//! 許多 Wi-Fi 路由器會過濾多播 / 廣播，因此兩種探測都會再對同網段每個位址送單播。

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;
use std::time::Duration;

use base64::{engine::general_purpose::STANDARD as B64, Engine};
use serde::Serialize;
use tokio::net::UdpSocket;

/// TDP 請求需附 RSA 公鑰（裝置用來加密我們用不到的欄位），任何合法公鑰皆可
const TDP_PUBLIC_KEY: &str = include_str!("discovery_key.pub");
const ONVIF_PORT: u16 = 3702;
const TDP_PORT: u16 = 20002;

#[derive(Debug, Clone, Default, Serialize)]
pub struct Found {
    pub host: String,
    /// ONVIF 服務埠（有回應 WS-Discovery 時）
    pub onvif_port: Option<u16>,
    /// 攝影機在 Tapo App 中的名稱
    pub name: String,
    pub model: String,
    pub hardware: String,
    pub firmware: String,
    pub mac: String,
    /// SMART.IPCAMERA / SMART.TAPODOORBELL / SMART.TAPOHUB …
    pub device_type: String,
    /// 由 TP-Link 探索找到（確定是 Tapo 裝置）
    pub tapo: bool,
    pub rtsp_open: bool,
    pub onvif_open: bool,
}

pub async fn probe(wait: Duration) -> Vec<Found> {
    probe_with(wait, &[]).await
}

/// 解析 "192.168.50.0/24" 或單一 IP "192.168.50.20"；只接受 /22–/32（最多 1024 個位址）
pub fn parse_subnet(s: &str) -> Option<(Ipv4Addr, u8)> {
    let s = s.trim();
    let (ip, len) = match s.split_once('/') {
        Some((ip, len)) => (ip.trim().parse::<Ipv4Addr>().ok()?, len.trim().parse::<u8>().ok()?),
        None => (s.parse::<Ipv4Addr>().ok()?, 32),
    };
    (22..=32).contains(&len).then_some((ip, len))
}

/// 本機網段 + 使用者指定的其他網段（例如另一台路由器後面、或經由路由可達的網段）
pub async fn probe_with(wait: Duration, extra: &[(Ipv4Addr, u8)]) -> Vec<Found> {
    let locals: Vec<(Ipv4Addr, Ipv4Addr)> = if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|i| match i.addr {
            if_addrs::IfAddr::V4(v4) if !v4.ip.is_loopback() && !v4.ip.is_link_local() => {
                Some((v4.ip, v4.netmask))
            }
            _ => None,
        })
        .collect();
    let mut tasks: Vec<_> = locals
        .into_iter()
        .map(|(ip, mask)| tokio::spawn(probe_on(ip, mask, wait)))
        .collect();
    for &(ip, len) in extra {
        let mask = if len == 0 { 0 } else { u32::MAX << (32 - u32::from(len)) };
        tasks.push(tokio::spawn(probe_remote(Ipv4Addr::from(u32::from(ip) & mask), len, wait)));
    }

    let mut found: HashMap<String, Found> = HashMap::new();
    for t in tasks {
        for f in t.await.unwrap_or_default() {
            match found.get_mut(&f.host) {
                Some(e) => merge(e, f),
                None => {
                    found.insert(f.host.clone(), f);
                }
            }
        }
    }

    // 檢查 RTSP / ONVIF 埠是否開啟（決定能用標準串流還是需要 Tapo 私有串流）
    let checks: Vec<_> = found
        .values()
        .map(|f| {
            let host = f.host.clone();
            let onvif = f.onvif_port.unwrap_or(2020);
            tokio::spawn(async move { (host.clone(), port_open(&host, 554).await, port_open(&host, onvif).await) })
        })
        .collect();
    for c in checks {
        if let Ok((host, rtsp, onvif)) = c.await {
            if let Some(f) = found.get_mut(&host) {
                f.rtsp_open = rtsp;
                f.onvif_open = onvif;
            }
        }
    }

    let mut out: Vec<Found> = found.into_values().collect();
    out.sort_by_key(|f| f.host.parse::<Ipv4Addr>().map(u32::from).unwrap_or(u32::MAX));
    out
}

async fn port_open(host: &str, port: u16) -> bool {
    matches!(
        tokio::time::timeout(Duration::from_millis(1200), tokio::net::TcpStream::connect((host, port))).await,
        Ok(Ok(_))
    )
}

fn merge(into: &mut Found, from: Found) {
    let fill = |a: &mut String, b: String| {
        if a.is_empty() {
            *a = b;
        }
    };
    if into.onvif_port.is_none() {
        into.onvif_port = from.onvif_port;
    }
    fill(&mut into.name, from.name);
    fill(&mut into.model, from.model);
    fill(&mut into.hardware, from.hardware);
    fill(&mut into.firmware, from.firmware);
    fill(&mut into.mac, from.mac);
    fill(&mut into.device_type, from.device_type);
    into.tapo |= from.tapo;
}

async fn probe_on(local: Ipv4Addr, mask: Ipv4Addr, wait: Duration) -> Vec<Found> {
    let Ok(sock) = UdpSocket::bind((local, 0)).await else { return Vec::new() };
    let sock = Arc::new(sock);
    let _ = sock.set_multicast_ttl_v4(2);
    let _ = sock.set_broadcast(true);
    let onvif = onvif_probe();
    let tdp = tdp_probe();

    let sender = sock.clone();
    tokio::spawn(async move {
        let m = u32::from(mask);
        let net = u32::from(local) & m;
        let broadcast = Ipv4Addr::from(net | !m);
        for _ in 0..2 {
            let _ = sender.send_to(onvif.as_bytes(), ("239.255.255.250", ONVIF_PORT)).await;
            let _ = sender.send_to(&tdp, (Ipv4Addr::BROADCAST, TDP_PORT)).await;
            let _ = sender.send_to(&tdp, (broadcast, TDP_PORT)).await;
        }
        if !(22..31).contains(&m.leading_ones()) {
            return;
        }
        for host in (net + 1)..(net | !m) {
            if host == u32::from(local) {
                continue;
            }
            let ip = Ipv4Addr::from(host);
            let _ = sender.send_to(onvif.as_bytes(), (ip, ONVIF_PORT)).await;
            let _ = sender.send_to(&tdp, (ip, TDP_PORT)).await;
            if host % 32 == 0 {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        }
    });

    let mut out = Vec::new();
    let deadline = tokio::time::Instant::now() + wait;
    let mut buf = vec![0u8; 65535];
    while let Ok(Ok((n, from))) = tokio::time::timeout_at(deadline, sock.recv_from(&mut buf)).await {
        let data = &buf[..n];
        let parsed = match from.port() {
            TDP_PORT => parse_tdp(data, from.ip()),
            _ => parse_onvif(&String::from_utf8_lossy(data), from.ip()),
        };
        if let Some(f) = parsed {
            out.push(f);
        }
    }
    out
}

/// 非本機網段：只能逐一單播（多播 / 廣播不會跨路由器）
async fn probe_remote(net: Ipv4Addr, len: u8, wait: Duration) -> Vec<Found> {
    let Ok(sock) = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).await else { return Vec::new() };
    let sock = Arc::new(sock);
    let onvif = onvif_probe();
    let tdp = tdp_probe();
    let sender = sock.clone();
    tokio::spawn(async move {
        let base = u32::from(net);
        let count: u32 = if len >= 32 { 1 } else { 1u32 << (32 - u32::from(len)) };
        let hosts: Vec<u32> = if count <= 2 { (base..base + count).collect() } else { ((base + 1)..(base + count - 1)).collect() };
        for (i, host) in hosts.into_iter().enumerate() {
            let ip = Ipv4Addr::from(host);
            let _ = sender.send_to(onvif.as_bytes(), (ip, ONVIF_PORT)).await;
            let _ = sender.send_to(&tdp, (ip, TDP_PORT)).await;
            if i % 32 == 31 {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        }
    });
    let mut out = Vec::new();
    let deadline = tokio::time::Instant::now() + wait;
    let mut buf = vec![0u8; 65535];
    while let Ok(Ok((n, from))) = tokio::time::timeout_at(deadline, sock.recv_from(&mut buf)).await {
        let data = &buf[..n];
        let parsed = match from.port() {
            TDP_PORT => parse_tdp(data, from.ip()),
            _ => parse_onvif(&String::from_utf8_lossy(data), from.ip()),
        };
        if let Some(f) = parsed {
            out.push(f);
        }
    }
    out
}

/// 本機所在的 IPv4 網段（給連線測試判斷「攝影機是否在同一網段」）
pub fn local_networks() -> Vec<(Ipv4Addr, Ipv4Addr)> {
    if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|i| match i.addr {
            if_addrs::IfAddr::V4(v4) if !v4.ip.is_loopback() && !v4.ip.is_link_local() => Some((v4.ip, v4.netmask)),
            _ => None,
        })
        .collect()
}

pub fn is_local_ip(ip: Ipv4Addr) -> bool {
    local_networks().iter().any(|(local, mask)| u32::from(*local) & u32::from(*mask) == u32::from(ip) & u32::from(*mask))
}

// ---------------------------------------------------------------------------
// TP-Link 裝置探索（與 python-kasa 的 _AesDiscoveryQuery 相同格式）

fn tdp_probe() -> Vec<u8> {
    let payload = serde_json::json!({"params": {"rsa_key": TDP_PUBLIC_KEY}}).to_string();
    let mut serial = [0u8; 4];
    getrandom::getrandom(&mut serial).expect("rng");
    let mut q = Vec::with_capacity(16 + payload.len());
    q.push(2); // 版本
    q.push(0); // 訊息類型
    q.extend_from_slice(&1u16.to_be_bytes()); // op code：探測
    q.extend_from_slice(&(payload.len() as u16).to_be_bytes());
    q.push(17); // flags
    q.push(0);
    q.extend_from_slice(&serial);
    q.extend_from_slice(&0x5A6B_7C8Du32.to_be_bytes()); // CRC 初始值，稍後覆寫
    q.extend_from_slice(payload.as_bytes());
    let crc = crc32fast::hash(&q);
    q[12..16].copy_from_slice(&crc.to_be_bytes());
    q
}

fn parse_tdp(data: &[u8], from: IpAddr) -> Option<Found> {
    let v: serde_json::Value = serde_json::from_slice(data.get(16..)?).ok()?;
    let r = &v["result"];
    let device_type = r["device_type"].as_str()?.to_string();
    // 只保留攝影機、門鈴與 Hub（插座、燈泡等不列出）
    let t = device_type.to_ascii_uppercase();
    if !(t.contains("CAMERA") || t.contains("DOORBELL") || t.contains("HUB")) {
        return None;
    }
    let s = |k: &str| r[k].as_str().unwrap_or("").to_string();
    // 型號格式如 "C210(EU)"，去掉地區
    let model = s("device_model").split('(').next().unwrap_or("").trim().to_string();
    Some(Found {
        host: r["ip"].as_str().map(str::to_string).unwrap_or_else(|| from.to_string()),
        name: decode_name(&s("device_name")),
        model,
        hardware: s("hardware_version"),
        firmware: s("firmware_version"),
        mac: s("mac").replace('-', ":"),
        device_type,
        tapo: true,
        ..Default::default()
    })
}

/// Tapo 的裝置名稱常以 Base64 傳送
fn decode_name(s: &str) -> String {
    if s.is_empty() {
        return String::new();
    }
    B64.decode(s)
        .ok()
        .and_then(|b| String::from_utf8(b).ok())
        .filter(|t| !t.is_empty() && t.chars().all(|c| !c.is_control()))
        .unwrap_or_else(|| s.to_string())
}

// ---------------------------------------------------------------------------
// ONVIF WS-Discovery

fn onvif_probe() -> String {
    let mut b = [0u8; 16];
    getrandom::getrandom(&mut b).expect("rng");
    let h: String = b.iter().map(|x| format!("{x:02x}")).collect();
    let uuid = format!("{}-{}-{}-{}-{}", &h[0..8], &h[8..12], &h[12..16], &h[16..20], &h[20..32]);
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?><e:Envelope xmlns:e="http://www.w3.org/2003/05/soap-envelope" xmlns:w="http://schemas.xmlsoap.org/ws/2004/08/addressing" xmlns:d="http://schemas.xmlsoap.org/ws/2005/04/discovery" xmlns:dn="http://www.onvif.org/ver10/network/wsdl"><e:Header><w:MessageID>uuid:{uuid}</w:MessageID><w:To e:mustUnderstand="true">urn:schemas-xmlsoap-org:ws:2005:04:discovery</w:To><w:Action e:mustUnderstand="true">http://schemas.xmlsoap.org/ws/2005/04/discovery/Probe</w:Action></e:Header><e:Body><d:Probe><d:Types>dn:NetworkVideoTransmitter</d:Types></d:Probe></e:Body></e:Envelope>"#
    )
}

fn parse_onvif(xml: &str, from: IpAddr) -> Option<Found> {
    let doc = roxmltree::Document::parse(xml).ok()?;
    let text = |name: &str| {
        doc.descendants()
            .find(|n| n.tag_name().name() == name)
            .and_then(|n| n.text())
            .unwrap_or("")
            .to_string()
    };
    let xaddrs = text("XAddrs");
    let scopes = text("Scopes");

    let (mut host, mut port) = (from.to_string(), 80u16);
    if let Some(url) = xaddrs.split_whitespace().find(|u| u.starts_with("http://")) {
        let authority = url["http://".len()..].split('/').next().unwrap_or("");
        let (h, p) = match authority.rsplit_once(':') {
            Some((h, p)) => (h, p.parse().unwrap_or(80)),
            None => (authority, 80),
        };
        if !h.is_empty() {
            host = h.to_string();
            port = p;
        }
    } else if xaddrs.is_empty() {
        return None;
    }

    let scope = |key: &str| {
        scopes
            .split_whitespace()
            .find_map(|s| s.strip_prefix(&format!("onvif://www.onvif.org/{key}/")))
            .map(percent_decode)
            .unwrap_or_default()
    };
    Some(Found {
        host,
        onvif_port: Some(port),
        name: scope("name"),
        model: scope("hardware"),
        ..Default::default()
    })
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    #[test]
    fn subnets() {
        use std::net::Ipv4Addr;
        assert_eq!(super::parse_subnet("192.168.50.0/24"), Some((Ipv4Addr::new(192, 168, 50, 0), 24)));
        assert_eq!(super::parse_subnet(" 192.168.50.20 "), Some((Ipv4Addr::new(192, 168, 50, 20), 32)));
        assert_eq!(super::parse_subnet("10.0.0.0/8"), None); // 太大
        assert_eq!(super::parse_subnet("abc"), None);
    }
}