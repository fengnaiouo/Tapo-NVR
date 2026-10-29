//! 精簡的 ONVIF Profile S 用戶端：WS-UsernameToken 驗證、PTZ、預設點、PullPoint 事件。

use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use chrono::{TimeZone, Utc};
use serde::Serialize;
use sha1::{Digest, Sha1};
use tokio::sync::Mutex;

const NS_DECL: &str = concat!(
    r#"xmlns:tds="http://www.onvif.org/ver10/device/wsdl" "#,
    r#"xmlns:trt="http://www.onvif.org/ver10/media/wsdl" "#,
    r#"xmlns:tptz="http://www.onvif.org/ver20/ptz/wsdl" "#,
    r#"xmlns:tev="http://www.onvif.org/ver10/events/wsdl" "#,
    r#"xmlns:wsnt="http://docs.oasis-open.org/wsn/b-2" "#,
    r#"xmlns:tt="http://www.onvif.org/ver10/schema""#
);

#[derive(Debug, Clone)]
struct Services {
    ptz: Option<String>,
    events: Option<String>,
    profile_token: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Preset {
    pub token: String,
    pub name: String,
}

#[derive(Debug, Clone)]
pub struct OnvifEvent {
    pub topic: String,
    pub name: String,
    pub value: String,
}

pub struct OnvifClient {
    http: reqwest::Client,
    device_url: String,
    username: String,
    password: String,
    /// 攝影機時間 - 本機時間（秒），避免時鐘誤差造成驗證失敗
    time_offset: AtomicI64,
    services: Mutex<Option<Services>>,
}

pub struct Subscription {
    address: String,
}

impl OnvifClient {
    pub fn new(host: &str, port: u16, username: &str, password: &str) -> Self {
        Self {
            http: reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(5))
                .build()
                .expect("http client"),
            device_url: format!("http://{host}:{port}/onvif/device_service"),
            username: username.into(),
            password: password.into(),
            time_offset: AtomicI64::new(0),
            services: Mutex::new(None),
        }
    }

    fn security_header(&self) -> String {
        let mut nonce = [0u8; 16];
        getrandom::getrandom(&mut nonce).expect("rng");
        let now = Utc::now() + chrono::Duration::seconds(self.time_offset.load(Ordering::Relaxed));
        let created = now.format("%Y-%m-%dT%H:%M:%SZ").to_string();
        let mut h = Sha1::new();
        h.update(nonce);
        h.update(created.as_bytes());
        h.update(self.password.as_bytes());
        let digest = B64.encode(h.finalize());
        format!(
            r#"<Security s:mustUnderstand="1" xmlns="http://docs.oasis-open.org/wss/2004/01/oasis-200401-wss-wssecurity-secext-1.0.xsd"><UsernameToken><Username>{}</Username><Password Type="http://docs.oasis-open.org/wss/2004/01/oasis-200401-wss-username-token-profile-1.0#PasswordDigest">{}</Password><Nonce EncodingType="http://docs.oasis-open.org/wss/2004/01/oasis-200401-wss-soap-message-security-1.0#Base64Binary">{}</Nonce><Created xmlns="http://docs.oasis-open.org/wss/2004/01/oasis-200401-wss-wssecurity-utility-1.0.xsd">{}</Created></UsernameToken></Security>"#,
            xml_escape(&self.username),
            digest,
            B64.encode(nonce),
            created
        )
    }

    async fn call(
        &self,
        url: &str,
        action: &str,
        body: &str,
        auth: bool,
        addressing: bool,
        timeout: Duration,
    ) -> Result<String> {
        let mut header = String::new();
        if auth {
            header.push_str(&self.security_header());
        }
        if addressing {
            header.push_str(&format!(
                r#"<a:Action s:mustUnderstand="1">{}</a:Action><a:To s:mustUnderstand="1">{}</a:To>"#,
                xml_escape(action),
                xml_escape(url)
            ));
        }
        let envelope = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?><s:Envelope xmlns:s="http://www.w3.org/2003/05/soap-envelope" xmlns:a="http://www.w3.org/2005/08/addressing"><s:Header>{header}</s:Header><s:Body {NS_DECL}>{body}</s:Body></s:Envelope>"#
        );
        let resp = self
            .http
            .post(url)
            .timeout(timeout)
            .header(
                "Content-Type",
                format!(r#"application/soap+xml; charset=utf-8; action="{action}""#),
            )
            .body(envelope)
            .send()
            .await
            .with_context(|| tr!("無法連線 ONVIF {url}", "Cannot connect to ONVIF {url}"))?;
        let status = resp.status();
        let text = resp.text().await?;
        if !status.is_success() || text.contains(":Fault>") || text.contains("<Fault>") {
            bail!("{}", tr!("ONVIF 錯誤 ({status})：{}", "ONVIF error ({status}): {}", fault_reason(&text)));
        }
        Ok(text)
    }

    async fn sync_time(&self) {
        let body = "<tds:GetSystemDateAndTime/>";
        let action = "http://www.onvif.org/ver10/device/wsdl/GetSystemDateAndTime";
        let Ok(xml) = self
            .call(&self.device_url, action, body, false, false, Duration::from_secs(8))
            .await
        else {
            return;
        };
        let Ok(doc) = roxmltree::Document::parse(&xml) else { return };
        let Some(utc) = find(&doc.root(), "UTCDateTime") else { return };
        let num = |parent: &str, name: &str| -> Option<u32> {
            find(&utc, parent)
                .and_then(|p| find(&p, name))
                .and_then(|n| n.text())
                .and_then(|t| t.trim().parse().ok())
        };
        if let (Some(y), Some(mo), Some(d), Some(h), Some(mi), Some(s)) = (
            num("Date", "Year"),
            num("Date", "Month"),
            num("Date", "Day"),
            num("Time", "Hour"),
            num("Time", "Minute"),
            num("Time", "Second"),
        ) {
            if let chrono::LocalResult::Single(cam) =
                Utc.with_ymd_and_hms(y as i32, mo, d, h, mi, s)
            {
                let offset = (cam - Utc::now()).num_seconds();
                self.time_offset.store(offset, Ordering::Relaxed);
            }
        }
    }

    async fn services(&self) -> Result<Services> {
        let mut guard = self.services.lock().await;
        if let Some(s) = guard.as_ref() {
            return Ok(s.clone());
        }
        self.sync_time().await;

        let xml = self
            .call(
                &self.device_url,
                "http://www.onvif.org/ver10/device/wsdl/GetCapabilities",
                "<tds:GetCapabilities><tds:Category>All</tds:Category></tds:GetCapabilities>",
                true,
                false,
                Duration::from_secs(10),
            )
            .await?;
        let doc = roxmltree::Document::parse(&xml)?;
        let xaddr = |section: &str| {
            find(&doc.root(), section)
                .and_then(|n| find(&n, "XAddr"))
                .and_then(|n| n.text())
                .map(|t| t.trim().to_string())
        };
        let media = xaddr("Media").unwrap_or_else(|| self.device_url.clone());
        let ptz = xaddr("PTZ");
        let events = xaddr("Events");

        let xml = self
            .call(
                &media,
                "http://www.onvif.org/ver10/media/wsdl/GetProfiles",
                "<trt:GetProfiles/>",
                true,
                false,
                Duration::from_secs(10),
            )
            .await?;
        let doc = roxmltree::Document::parse(&xml)?;
        let profile_token = doc
            .descendants()
            .find(|n| n.tag_name().name() == "Profiles")
            .and_then(|n| n.attribute("token"))
            .ok_or_else(|| anyhow!("{}", tr!("攝影機沒有回傳任何 media profile", "The camera returned no media profile")))?
            .to_string();

        let s = Services { ptz, events, profile_token };
        *guard = Some(s.clone());
        Ok(s)
    }

    /// 測試連線：回傳（支援 PTZ, 支援事件）
    pub async fn capabilities(&self) -> Result<(bool, bool)> {
        self.reset().await;
        let s = self.services().await?;
        Ok((s.ptz.is_some(), s.events.is_some()))
    }

    /// 連線失敗後清除快取，下次重新探索
    pub async fn reset(&self) {
        *self.services.lock().await = None;
    }

    async fn ptz_call(&self, op: &str, inner: &str) -> Result<String> {
        let s = self.services().await?;
        let url = s.ptz.clone().ok_or_else(|| anyhow!("{}", tr!("此攝影機不支援 ONVIF PTZ", "This camera does not support ONVIF PTZ")))?;
        let body = format!(
            "<tptz:{op}><tptz:ProfileToken>{}</tptz:ProfileToken>{inner}</tptz:{op}>",
            xml_escape(&s.profile_token)
        );
        let action = format!("http://www.onvif.org/ver20/ptz/wsdl/{op}");
        let r = self.call(&url, &action, &body, true, false, Duration::from_secs(10)).await;
        if r.is_err() {
            self.reset().await;
        }
        r
    }

    /// 持續移動（-1.0 ~ 1.0），需搭配 stop()
    pub async fn continuous_move(&self, pan: f32, tilt: f32) -> Result<()> {
        let inner = format!(
            r#"<tptz:Velocity><tt:PanTilt x="{:.2}" y="{:.2}"/></tptz:Velocity>"#,
            pan.clamp(-1.0, 1.0),
            tilt.clamp(-1.0, 1.0)
        );
        self.ptz_call("ContinuousMove", &inner).await.map(|_| ())
    }

    /// 相對移動一步（-1.0 ~ 1.0，為可轉動範圍的比例）
    pub async fn relative_move(&self, pan: f32, tilt: f32) -> Result<()> {
        let inner = format!(
            r#"<tptz:Translation><tt:PanTilt x="{:.3}" y="{:.3}"/></tptz:Translation>"#,
            pan.clamp(-1.0, 1.0),
            tilt.clamp(-1.0, 1.0)
        );
        self.ptz_call("RelativeMove", &inner).await.map(|_| ())
    }

    pub async fn stop(&self) -> Result<()> {
        self.ptz_call("Stop", "<tptz:PanTilt>true</tptz:PanTilt><tptz:Zoom>false</tptz:Zoom>")
            .await
            .map(|_| ())
    }

    pub async fn presets(&self) -> Result<Vec<Preset>> {
        let xml = self.ptz_call("GetPresets", "").await?;
        let doc = roxmltree::Document::parse(&xml)?;
        Ok(doc
            .descendants()
            .filter(|n| n.tag_name().name() == "Preset")
            .filter_map(|n| {
                let token = n.attribute("token")?.to_string();
                let name = find(&n, "Name")
                    .and_then(|x| x.text())
                    .map(|t| t.trim().to_string())
                    .unwrap_or_else(|| token.clone());
                Some(Preset { token, name })
            })
            .collect())
    }

    pub async fn goto_preset(&self, token: &str) -> Result<()> {
        let inner = format!("<tptz:PresetToken>{}</tptz:PresetToken>", xml_escape(token));
        self.ptz_call("GotoPreset", &inner).await.map(|_| ())
    }

    pub async fn set_preset(&self, name: &str) -> Result<String> {
        let inner = format!("<tptz:PresetName>{}</tptz:PresetName>", xml_escape(name));
        let xml = self.ptz_call("SetPreset", &inner).await?;
        let doc = roxmltree::Document::parse(&xml)?;
        Ok(find(&doc.root(), "PresetToken")
            .and_then(|n| n.text())
            .map(|t| t.trim().to_string())
            .unwrap_or_default())
    }

    pub async fn remove_preset(&self, token: &str) -> Result<()> {
        let inner = format!("<tptz:PresetToken>{}</tptz:PresetToken>", xml_escape(token));
        self.ptz_call("RemovePreset", &inner).await.map(|_| ())
    }

    pub async fn subscribe(&self) -> Result<Subscription> {
        self.reset().await;
        let s = self.services().await?;
        let url = s.events.ok_or_else(|| anyhow!("{}", tr!("此攝影機不支援 ONVIF 事件", "This camera does not support ONVIF events")))?;
        let xml = self
            .call(
                &url,
                "http://www.onvif.org/ver10/events/wsdl/EventPortType/CreatePullPointSubscriptionRequest",
                "<tev:CreatePullPointSubscription><tev:InitialTerminationTime>PT600S</tev:InitialTerminationTime></tev:CreatePullPointSubscription>",
                true,
                false,
                Duration::from_secs(10),
            )
            .await?;
        let doc = roxmltree::Document::parse(&xml)?;
        let address = find(&doc.root(), "SubscriptionReference")
            .and_then(|n| find(&n, "Address"))
            .and_then(|n| n.text())
            .map(|t| t.trim().to_string())
            .unwrap_or(url);
        Ok(Subscription { address })
    }

    pub async fn pull(&self, sub: &Subscription) -> Result<Vec<OnvifEvent>> {
        let xml = self
            .call(
                &sub.address,
                "http://www.onvif.org/ver10/events/wsdl/PullPointSubscription/PullMessagesRequest",
                "<tev:PullMessages><tev:Timeout>PT20S</tev:Timeout><tev:MessageLimit>32</tev:MessageLimit></tev:PullMessages>",
                true,
                true,
                Duration::from_secs(35),
            )
            .await?;
        let doc = roxmltree::Document::parse(&xml)?;
        let mut out = Vec::new();
        for msg in doc.descendants().filter(|n| n.tag_name().name() == "NotificationMessage") {
            let topic = find(&msg, "Topic")
                .and_then(|n| n.text())
                .map(|t| t.trim().to_string())
                .unwrap_or_default();
            let Some(data) = find(&msg, "Data") else { continue };
            for item in data.descendants().filter(|n| n.tag_name().name() == "SimpleItem") {
                if let (Some(name), Some(value)) = (item.attribute("Name"), item.attribute("Value")) {
                    out.push(OnvifEvent {
                        topic: topic.clone(),
                        name: name.into(),
                        value: value.into(),
                    });
                }
            }
        }
        Ok(out)
    }

    pub async fn renew(&self, sub: &Subscription) -> Result<()> {
        self.call(
            &sub.address,
            "http://docs.oasis-open.org/wsn/bw-2/SubscriptionManager/RenewRequest",
            "<wsnt:Renew><wsnt:TerminationTime>PT600S</wsnt:TerminationTime></wsnt:Renew>",
            true,
            true,
            Duration::from_secs(10),
        )
        .await
        .map(|_| ())
    }

    pub async fn unsubscribe(&self, sub: &Subscription) {
        let _ = self
            .call(
                &sub.address,
                "http://docs.oasis-open.org/wsn/bw-2/SubscriptionManager/UnsubscribeRequest",
                "<wsnt:Unsubscribe/>",
                true,
                true,
                Duration::from_secs(5),
            )
            .await;
    }
}

fn find<'a, 'i>(node: &roxmltree::Node<'a, 'i>, local: &str) -> Option<roxmltree::Node<'a, 'i>> {
    node.descendants().find(|n| n.is_element() && n.tag_name().name() == local)
}

fn fault_reason(xml: &str) -> String {
    roxmltree::Document::parse(xml)
        .ok()
        .and_then(|doc| {
            let parts: Vec<String> = doc
                .descendants()
                .filter(|n| matches!(n.tag_name().name(), "Text" | "Value" | "faultstring"))
                .filter_map(|n| n.text().map(|t| t.trim().to_string()))
                .filter(|t| !t.is_empty())
                .collect();
            (!parts.is_empty()).then(|| parts.join(" / "))
        })
        .unwrap_or_else(|| xml.chars().take(200).collect())
}

pub fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}
