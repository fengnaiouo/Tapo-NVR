//! Tapo 私有控制 API（HTTPS 443）：隱私模式、夜視、偵測、追蹤、SD 卡、雲台等 ONVIF 沒有的功能。
//! 協定參考 pytapo（github.com/JurajNyiri/pytapo）與 freeKC/tapo-v4-protocol。
//!
//! 支援兩種登入：
//! - 舊版韌體：encrypt_type 3（AES-128-CBC securePassthrough）
//! - 新版韌體：TPAP（SPAKE2+ P-256，AES-128-CCM）—— 攝影機對舊版登入回 -40211 時自動改用
//!
//! 攝影機有登入次數限制（實測 10 次），因此：
//! - 舊版登入先在本機驗證 device_confirm，密碼錯就不送第二步
//! - 密碼錯誤後停止自動重試，直到使用者修改密碼（設定變更會建立新的用戶端）
//! - 兩次登入之間至少間隔 20 秒，並重複使用 Session

use std::time::{Duration, Instant};

use aes::cipher::{block_padding::Pkcs7, BlockDecryptMut, BlockEncryptMut, KeyIvInit};
use anyhow::{anyhow, bail, Context, Result};
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;

type Aes128CbcEnc = cbc::Encryptor<aes::Aes128>;
type Aes128CbcDec = cbc::Decryptor<aes::Aes128>;

const USER: &str = "admin";
const MIN_LOGIN_INTERVAL: Duration = Duration::from_secs(20);

/// 使用中的加密通道
enum Channel {
    /// 舊版：securePassthrough
    Legacy { lsk: [u8; 16], ivb: [u8; 16], cnonce: String, hashed: String },
    /// 新版 TPAP：/stok=…/ds 的二進位 AES-CCM 通道
    Tpap { key: [u8; 16], nonce: [u8; 12], expires: Instant },
}

struct Session {
    stok: String,
    seq: i64,
    channel: Channel,
}

#[derive(Clone, Copy, PartialEq)]
enum Protocol {
    Legacy,
    Tpap,
}

#[derive(Default)]
struct State {
    session: Option<Session>,
    protocol: Option<Protocol>,
    /// TPAP 成功的密碼格式（避免重新登入時浪費嘗試次數）
    tpap_passcode: Option<usize>,
    /// 實際使用的密碼雜湊（"md5" / "sha256"），供 go2rtc tapo:// 使用
    hash_kind: Option<&'static str>,
    last_login: Option<Instant>,
    /// 密碼錯誤或帳號不被接受：停止自動登入
    auth_error: Option<String>,
    cache: Option<(Instant, Value)>,
}

pub struct TapoClient {
    host: String,
    password: String,
    http: reqwest::Client,
    state: Mutex<State>,
}

pub(crate) fn hex_upper(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02X}")).collect()
}

pub(crate) fn sha256_upper(parts: &[&[u8]]) -> String {
    let mut h = Sha256::new();
    for p in parts {
        h.update(p);
    }
    hex_upper(&h.finalize())
}

fn md5_upper(s: &str) -> String {
    hex_upper(&md5::compute(s.as_bytes()).0)
}

fn md5_lower(s: &str) -> String {
    format!("{:x}", md5::compute(s.as_bytes()))
}

fn random_bytes<const N: usize>() -> [u8; N] {
    let mut b = [0u8; N];
    getrandom::getrandom(&mut b).expect("rng");
    b
}

fn legacy_token(kind: &str, cnonce: &str, nonce: &str, hashed: &str) -> [u8; 16] {
    let key = sha256_upper(&[cnonce.as_bytes(), hashed.as_bytes(), nonce.as_bytes()]);
    let mut h = Sha256::new();
    h.update(kind.as_bytes());
    h.update(cnonce.as_bytes());
    h.update(nonce.as_bytes());
    h.update(key.as_bytes());
    let d = h.finalize();
    let mut out = [0u8; 16];
    out.copy_from_slice(&d[..16]);
    out
}

fn tapo_error(code: i64) -> String {
    let msg = match code {
        -40401 => tr!("Session 已失效", "Session expired"),
        -40210 | -40211 => tr!("攝影機要求新版登入協定", "The camera requires the new login protocol"),
        -40209 => tr!("攝影機拒絕此登入方式", "The camera rejected this login method"),
        -40404 => tr!("登入嘗試過多，攝影機暫時鎖定", "Too many login attempts, the camera is temporarily locked"),
        -40411 | -40413 => tr!("雲端密碼錯誤", "Wrong cloud password"),
        -64303 => tr!("攝影機正在巡航，請先停止", "The camera is cruising, stop it first"),
        -40106 => tr!("攝影機不支援此功能", "This camera does not support this feature"),
        -1 => tr!("攝影機回應錯誤", "Camera error"),
        _ => String::new(),
    };
    if msg.is_empty() {
        tr!("錯誤代碼 {code}", "error code {code}")
    } else {
        format!("{msg} ({code})")
    }
}

fn lockout_seconds(v: &Value) -> i64 {
    [&v["error_info"], &v["data"], &v["result"]["data"]]
        .iter()
        .filter_map(|h| h["sec_left"].as_i64())
        .find(|s| *s > 0)
        .unwrap_or(0)
}

impl TapoClient {
    pub fn new(host: &str, cloud_password: &str) -> Self {
        Self {
            host: host.to_string(),
            password: cloud_password.to_string(),
            http: reqwest::Client::builder()
                .danger_accept_invalid_certs(true)
                .timeout(Duration::from_secs(10))
                .connect_timeout(Duration::from_secs(5))
                .build()
                .expect("http client"),
            state: Mutex::new(State::default()),
        }
    }

    fn base(&self) -> String {
        format!("https://{}", self.host)
    }

    fn post(&self, url: &str, content_type: &str) -> reqwest::RequestBuilder {
        self.http
            .post(url)
            .header("Referer", self.base())
            .header("Accept", content_type)
            .header("User-Agent", "Tapo CameraClient Android")
            .header("requestByApp", "true")
            .header("Content-Type", content_type)
    }

    async fn post_login(&self, params: Value) -> Result<Value> {
        let body = json!({"method": "login", "params": params}).to_string();
        self.post(&format!("{}/", self.base()), "application/json; charset=UTF-8")
            .body(body)
            .send()
            .await
            .with_context(|| tr!("無法連線攝影機控制埠 443", "Cannot reach the camera control port 443"))?
            .json()
            .await
            .with_context(|| tr!("攝影機回應格式錯誤", "Unexpected camera response"))
    }

    /// 已登入時回傳使用的密碼雜湊方式
    pub async fn hash_kind(&self) -> Option<&'static str> {
        self.state.lock().await.hash_kind
    }

    async fn login(&self, st: &mut State) -> Result<()> {
        if let Some(e) = &st.auth_error {
            bail!("{}", tr!("{e}（請到設定修正 TP-Link 雲端密碼）", "{e} (fix the TP-Link cloud password in Settings)"));
        }
        if let Some(t) = st.last_login {
            if t.elapsed() < MIN_LOGIN_INTERVAL {
                let s = (MIN_LOGIN_INTERVAL - t.elapsed()).as_secs() + 1;
                bail!("{}", tr!("登入過於頻繁，請 {s} 秒後再試", "Logging in too often, retry in {s} seconds"));
            }
        }
        st.last_login = Some(Instant::now());
        match st.protocol {
            Some(Protocol::Tpap) => self.login_tpap(st).await,
            _ => self.login_legacy(st).await,
        }
    }

    // ------------------------------------------------------------------
    // 舊版登入：encrypt_type 3

    async fn login_legacy(&self, st: &mut State) -> Result<()> {
        let cnonce = hex_upper(&random_bytes::<8>());
        let resp = self
            .post_login(json!({"cnonce": cnonce, "encrypt_type": "3", "username": USER}))
            .await?;
        let data = &resp["result"]["data"];
        let code = data["code"].as_i64().or(resp["error_code"].as_i64()).unwrap_or(0);
        // 新版韌體：改用 TPAP（這個探測不帶密碼）
        if code == -40211 || code == -40210 || resp["error_code"].as_i64() == Some(-40211) {
            tracing::info!("[tapo {}] 攝影機使用新版 TPAP 登入", self.host);
            st.protocol = Some(Protocol::Tpap);
            return self.login_tpap(st).await;
        }
        let secs = lockout_seconds(&resp);
        if secs > 0 {
            bail!("{}", tr!("攝影機因登入失敗次數過多暫時鎖定，請 {secs} 秒後再試", "The camera is locked after too many failed logins, retry in {secs} seconds"));
        }
        let (Some(nonce), Some(confirm)) = (data["nonce"].as_str(), data["device_confirm"].as_str()) else {
            bail!("{}", tr!("登入失敗：{}", "Login failed: {}", tapo_error(code)));
        };
        st.protocol = Some(Protocol::Legacy);

        // 在本機驗證密碼：device_confirm = SHA256(cnonce + 雜湊密碼 + nonce) + nonce + cnonce
        let candidates = [("sha256", sha256_upper(&[self.password.as_bytes()])), ("md5", md5_upper(&self.password))];
        let Some((kind, hashed)) = candidates.into_iter().find(|(_, h)| {
            let expect = sha256_upper(&[cnonce.as_bytes(), h.as_bytes(), nonce.as_bytes()]);
            confirm == format!("{expect}{nonce}{cnonce}")
        }) else {
            let msg = match data["time"].as_i64() {
                Some(n) => tr!("TP-Link 雲端密碼錯誤（攝影機剩餘嘗試次數 {n}）", "Wrong TP-Link cloud password ({n} attempts left on the camera)"),
                None => tr!("TP-Link 雲端密碼錯誤", "Wrong TP-Link cloud password"),
            };
            st.auth_error = Some(msg.clone());
            bail!("{msg}");
        };

        let digest = sha256_upper(&[hashed.as_bytes(), cnonce.as_bytes(), nonce.as_bytes()]);
        let resp = self
            .post_login(json!({
                "cnonce": cnonce,
                "encrypt_type": "3",
                "digest_passwd": format!("{digest}{cnonce}{nonce}"),
                "username": USER,
            }))
            .await?;
        let result = &resp["result"];
        let (Some(stok), Some(seq)) = (result["stok"].as_str(), result["start_seq"].as_i64()) else {
            bail!("{}", tr!("登入失敗：{}", "Login failed: {}", tapo_error(resp["error_code"].as_i64().unwrap_or(0))));
        };
        if result["user_group"].as_str().is_some_and(|g| g != "root") {
            let msg = tr!("此帳號沒有控制權限，請使用攝影機擁有者的 TP-Link 帳號密碼", "This account cannot control the camera; use the camera owner's TP-Link account");
            st.auth_error = Some(msg.clone());
            bail!("{msg}");
        }
        st.hash_kind = Some(kind);
        st.session = Some(Session {
            stok: stok.to_string(),
            seq,
            channel: Channel::Legacy {
                lsk: legacy_token("lsk", &cnonce, nonce, &hashed),
                ivb: legacy_token("ivb", &cnonce, nonce, &hashed),
                cnonce,
                hashed,
            },
        });
        tracing::info!("[tapo {}] 已登入控制 API（舊版協定）", self.host);
        Ok(())
    }

    // ------------------------------------------------------------------
    // 新版登入：TPAP（SPAKE2+）

    async fn login_tpap(&self, st: &mut State) -> Result<()> {
        let discover = self.post_login(json!({"sub_method": "discover"})).await.unwrap_or(Value::Null);
        let tpap = &discover["result"]["tpap"];
        let username = if tpap["user_hash_type"].as_i64() == Some(1) {
            sha256_upper(&[USER.as_bytes()])
        } else {
            md5_lower(USER)
        };
        let passcodes = [md5_lower(&self.password), sha256_upper(&[self.password.as_bytes()])];
        // 重新登入時只用上次成功的格式，避免浪費嘗試次數
        let order: Vec<usize> = match st.tpap_passcode {
            Some(i) => vec![i],
            None => vec![0, 1],
        };
        let mut last_err = None;
        for i in order {
            match self.tpap_exchange(&username, &passcodes[i]).await {
                Ok(session) => {
                    st.tpap_passcode = Some(i);
                    st.hash_kind = Some(if i == 0 { "md5" } else { "sha256" });
                    st.session = Some(session);
                    tracing::info!("[tapo {}] 已登入控制 API（TPAP）", self.host);
                    return Ok(());
                }
                Err(TpapError::Refused) => last_err = Some(TpapError::Refused),
                Err(e) => return Err(anyhow!(e.to_string())),
            }
        }
        let msg = match last_err {
            Some(_) | None => tr!("TP-Link 雲端密碼錯誤", "Wrong TP-Link cloud password"),
        };
        st.auth_error = Some(msg.clone());
        bail!("{msg}")
    }

    async fn tpap_exchange(&self, username: &str, passcode: &str) -> Result<Session, TpapError> {
        let user_random = random_bytes::<32>();
        let reg = self
            .post_login(json!({
                "sub_method": "pake_register",
                "username": username,
                "user_random": B64.encode(user_random),
                "cipher_suites": [1],
                "encryption": ["aes_128_ccm"],
                "passcode_type": "userpw",
            }))
            .await
            .map_err(|e| TpapError::Other(e.to_string()))?;
        let res = &reg["result"];
        if res.is_null() {
            return Err(TpapError::Other(tr!("登入失敗：{}", "Login failed: {}", tapo_error(reg["error_code"].as_i64().unwrap_or(0)))));
        }
        let credential = spake2p::apply_extra_crypt(passcode, &res["extra_crypt"]).map_err(TpapError::Other)?;
        let field = |k: &str| res[k].as_str().and_then(|s| B64.decode(s).ok()).ok_or_else(|| TpapError::Other(format!("pake_register: {k}")));
        let client = spake2p::Client::new(
            &field("dev_salt")?,
            res["iterations"].as_u64().or_else(|| res["iterations"].as_str().and_then(|s| s.parse().ok())).unwrap_or(10000) as u32,
            &field("dev_share")?,
            &user_random,
            &field("dev_random")?,
            &credential,
        )
        .map_err(TpapError::Other)?;

        let share = self
            .post_login(json!({
                "sub_method": "pake_share",
                "user_share": B64.encode(&client.x_bytes),
                "user_confirm": B64.encode(client.user_confirm),
            }))
            .await
            .map_err(|e| TpapError::Other(e.to_string()))?;
        let r = &share["result"];
        if r.is_null() {
            let secs = lockout_seconds(&share);
            if secs > 0 {
                return Err(TpapError::Other(tr!("攝影機因登入失敗次數過多暫時鎖定，請 {secs} 秒後再試", "The camera is locked after too many failed logins, retry in {secs} seconds")));
            }
            return match share["error_code"].as_i64() {
                Some(-40401) => Err(TpapError::Refused),
                code => Err(TpapError::Other(tr!("登入失敗：{}", "Login failed: {}", tapo_error(code.unwrap_or(0))))),
            };
        }
        let dev_confirm = r["dev_confirm"].as_str().and_then(|s| B64.decode(s).ok()).unwrap_or_default();
        if !client.device_confirm_ok(&dev_confirm) {
            return Err(TpapError::Other(tr!("攝影機身分驗證失敗", "Camera authentication mismatch")));
        }
        let (key, nonce) = client.session_key_and_nonce();
        let stok = r["stok"].as_str().unwrap_or_default().to_string();
        let seq = r["start_seq"].as_i64().or_else(|| r["start_seq"].as_str().and_then(|s| s.parse().ok())).unwrap_or(1);
        let expired = r["expired"].as_u64().unwrap_or(3600).max(60);
        Ok(Session { stok, seq, channel: Channel::Tpap { key, nonce, expires: Instant::now() + Duration::from_secs(expired) } })
    }

    // ------------------------------------------------------------------
    // 加密通道

    /// 送出一個 multipleRequest，Session 失效時自動重新登入一次
    async fn send(&self, req: &Value) -> Result<Value> {
        let mut st = self.state.lock().await;
        for attempt in 0..2 {
            let expired = matches!(&st.session, Some(Session { channel: Channel::Tpap { expires, .. }, .. }) if Instant::now() >= *expires);
            if st.session.is_none() || expired {
                st.session = None;
                self.login(&mut st).await?;
            }
            let s = st.session.as_mut().unwrap();
            let seq = s.seq;
            s.seq += 1;
            let result = match &s.channel {
                Channel::Legacy { lsk, ivb, cnonce, hashed } => {
                    self.send_legacy(&s.stok, seq, *lsk, *ivb, cnonce, hashed, req).await
                }
                Channel::Tpap { key, nonce, .. } => self.send_tpap(&s.stok, seq, key, nonce, req).await,
            };
            match result {
                Ok(v) if v["error_code"].as_i64().is_some_and(|c| c == -40401 || c == -40421) && attempt == 0 => {
                    st.session = None;
                }
                Ok(v) => return Ok(v),
                Err(SendError::Session) if attempt == 0 => st.session = None,
                Err(SendError::Session) => {
                    st.session = None;
                    bail!("{}", tr!("攝影機控制失敗", "Camera control failed"));
                }
                Err(SendError::Other(e)) => {
                    st.session = None;
                    return Err(e);
                }
            }
        }
        bail!("{}", tr!("攝影機控制失敗", "Camera control failed"))
    }

    #[allow(clippy::too_many_arguments)]
    async fn send_legacy(
        &self,
        stok: &str,
        seq: i64,
        lsk: [u8; 16],
        ivb: [u8; 16],
        cnonce: &str,
        hashed: &str,
        req: &Value,
    ) -> Result<Value, SendError> {
        let plain = req.to_string();
        let enc = Aes128CbcEnc::new(&lsk.into(), &ivb.into()).encrypt_padded_vec_mut::<Pkcs7>(plain.as_bytes());
        let full = json!({"method": "securePassthrough", "params": {"request": B64.encode(enc)}}).to_string();
        let tag_key = sha256_upper(&[hashed.as_bytes(), cnonce.as_bytes()]);
        let tag = sha256_upper(&[tag_key.as_bytes(), full.as_bytes(), seq.to_string().as_bytes()]);
        let resp: Value = self
            .post(&format!("{}/stok={stok}/ds", self.base()), "application/json; charset=UTF-8")
            .header("Seq", seq.to_string())
            .header("Tapo_tag", tag)
            .body(full)
            .send()
            .await
            .map_err(|e| SendError::Other(anyhow!(tr!("無法連線攝影機控制埠 443：{e}", "Cannot reach the camera control port 443: {e}"))))?
            .json()
            .await
            .unwrap_or(Value::Null);
        if let Some(b64) = resp["result"]["response"].as_str() {
            let decrypted = B64
                .decode(b64)
                .ok()
                .and_then(|ct| Aes128CbcDec::new(&lsk.into(), &ivb.into()).decrypt_padded_vec_mut::<Pkcs7>(&ct).ok())
                .and_then(|pt| serde_json::from_slice::<Value>(&pt).ok());
            return decrypted.ok_or(SendError::Session);
        }
        match resp["error_code"].as_i64() {
            Some(-40401) | Some(-1) | None => Err(SendError::Session),
            Some(code) => Err(SendError::Other(anyhow!(tr!("攝影機回應錯誤：{}", "Camera error: {}", tapo_error(code))))),
        }
    }

    async fn send_tpap(&self, stok: &str, seq: i64, key: &[u8; 16], nonce: &[u8; 12], req: &Value) -> Result<Value, SendError> {
        use ccm::aead::{Aead, KeyInit};
        let cipher = Ccm::new(key.into());
        let nonce_for = |s: u32| {
            let mut n = [0u8; 13];
            n[..8].copy_from_slice(&nonce[..8]);
            n[8..12].copy_from_slice(&s.to_be_bytes());
            n
        };
        let seq32 = seq as u32;
        let n = nonce_for(seq32);
        let ct = cipher
            .encrypt(ccm::Nonce::<ccm::consts::U12>::from_slice(&n[..12]), req.to_string().as_bytes())
            .map_err(|_| SendError::Other(anyhow!("encrypt")))?;
        let mut body = seq32.to_be_bytes().to_vec();
        body.extend_from_slice(&ct);
        let raw = self
            .post(&format!("{}/stok={stok}/ds", self.base()), "application/octet-stream")
            .body(body)
            .send()
            .await
            .map_err(|e| SendError::Other(anyhow!(tr!("無法連線攝影機控制埠 443：{e}", "Cannot reach the camera control port 443: {e}"))))?
            .bytes()
            .await
            .map_err(|e| SendError::Other(anyhow!(e)))?;
        if raw.first() == Some(&b'{') {
            let code = serde_json::from_slice::<Value>(&raw).ok().and_then(|v| v["error_code"].as_i64());
            return match code {
                Some(-40401) | Some(-40421) | None => Err(SendError::Session),
                Some(c) => Err(SendError::Other(anyhow!(tr!("攝影機回應錯誤：{}", "Camera error: {}", tapo_error(c))))),
            };
        }
        if raw.len() < 20 {
            return Err(SendError::Session);
        }
        let reply_seq = u32::from_be_bytes([raw[0], raw[1], raw[2], raw[3]]);
        let rn = nonce_for(reply_seq);
        let plain = cipher
            .decrypt(ccm::Nonce::<ccm::consts::U12>::from_slice(&rn[..12]), &raw[4..])
            .map_err(|_| SendError::Session)?;
        serde_json::from_slice(&plain).map_err(|_| SendError::Session)
    }

    /// 一次送出多個方法，回傳各自的 result（失敗的為 Err 訊息）
    pub async fn multi(&self, calls: &[(&str, Value)]) -> Result<Vec<Result<Value, String>>> {
        let requests: Vec<Value> = calls.iter().map(|(m, p)| json!({"method": m, "params": p})).collect();
        let resp = self.send(&json!({"method": "multipleRequest", "params": {"requests": requests}})).await?;
        if let Some(code) = resp["error_code"].as_i64().filter(|c| *c != 0) {
            bail!("{}", tr!("攝影機回應錯誤：{}", "Camera error: {}", tapo_error(code)));
        }
        let responses = resp["result"]["responses"].as_array().cloned().unwrap_or_default();
        Ok((0..calls.len())
            .map(|i| {
                let r = responses.get(i).cloned().unwrap_or(Value::Null);
                match r["error_code"].as_i64() {
                    Some(0) | None if !r["result"].is_null() => Ok(r["result"].clone()),
                    Some(0) => Ok(Value::Null),
                    Some(code) => Err(tapo_error(code)),
                    None => Err(tr!("無回應", "No response")),
                }
            })
            .collect())
    }

    pub async fn call(&self, method: &str, params: Value) -> Result<Value> {
        self.multi(&[(method, params)]).await?.remove(0).map_err(|e| anyhow!(e))
    }

    // ------------------------------------------------------------------
    // 高階功能

    /// 讀取目前狀態（快取 15 秒）。不支援的項目為 null，介面會隱藏
    pub async fn status(&self, force: bool) -> Result<Value> {
        if !force {
            if let Some((t, v)) = &self.state.lock().await.cache {
                if t.elapsed() < Duration::from_secs(15) {
                    return Ok(v.clone());
                }
            }
        }
        let r = self
            .multi(&[
                ("getDeviceInfo", json!({"device_info": {"name": ["basic_info"]}})),
                ("getLensMaskConfig", json!({"lens_mask": {"name": ["lens_mask_info"]}})),
                ("getLightFrequencyInfo", json!({"image": {"name": "common"}})),
                ("getDetectionConfig", json!({"motion_detection": {"name": ["motion_det"]}})),
                ("getPersonDetectionConfig", json!({"people_detection": {"name": ["detection"]}})),
                ("getTargetTrackConfig", json!({"target_track": {"name": ["target_track_info"]}})),
                ("getSdCardStatus", json!({"harddisk_manage": {"table": ["hd_info"]}})),
            ])
            .await?;
        let ok = |i: usize| r[i].as_ref().ok().cloned().unwrap_or(Value::Null);
        let on = |v: &Value| v.as_str().map(|s| s == "on");

        let info = ok(0);
        let basic = &info["device_info"]["basic_info"];
        let motion = ok(3);
        let md = &motion["motion_detection"]["motion_det"];
        let sensitivity = md["digital_sensitivity"]
            .as_str()
            .and_then(|s| s.parse::<u32>().ok())
            .map(|n| if n <= 20 { "low" } else if n <= 50 { "medium" } else { "high" })
            .or_else(|| md["sensitivity"].as_str().map(|s| if s == "normal" { "medium" } else { s }));

        let sd = ok(6);
        let disks: Vec<Value> = sd["harddisk_manage"]["hd_info"]
            .as_array()
            .map(|list| {
                list.iter()
                    .flat_map(|item| item.as_object().into_iter().flat_map(|o| o.values().cloned()))
                    .map(|d| {
                        json!({
                            "status": d["status"],
                            "detect_status": d["detect_status"],
                            "total": d["total_space"],
                            "free": d["free_space"],
                            "video_total": d["video_total_space"],
                            "video_free": d["video_free_space"],
                            "record_start": d["record_start_time"],
                            "type": d["type"],
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();

        let v = json!({
            "model": basic["device_model"].as_str().or(basic["dev_name"].as_str()),
            "hardware": basic["hw_version"],
            "firmware": basic["sw_version"],
            "device_type": basic["device_type"],
            "privacy": on(&ok(1)["lens_mask"]["lens_mask_info"]["enabled"]),
            "night_vision": ok(2)["image"]["common"]["inf_type"].as_str(),
            "motion_detection": on(&md["enabled"]),
            "motion_sensitivity": sensitivity,
            "person_detection": on(&ok(4)["people_detection"]["detection"]["enabled"]),
            "auto_track": on(&ok(5)["target_track"]["target_track_info"]["enabled"]),
            "sd_cards": if r[6].is_ok() { json!(disks) } else { Value::Null },
        });
        self.state.lock().await.cache = Some((Instant::now(), v.clone()));
        Ok(v)
    }

    pub async fn invalidate(&self) {
        self.state.lock().await.cache = None;
    }

    pub async fn set_privacy(&self, on: bool) -> Result<()> {
        self.call("setLensMaskConfig", json!({"lens_mask": {"lens_mask_info": {"enabled": onoff(on)}}})).await?;
        Ok(())
    }

    pub async fn set_night_vision(&self, mode: &str) -> Result<()> {
        if !matches!(mode, "auto" | "on" | "off") {
            bail!("{}", tr!("夜視模式只能是 auto / on / off", "Night vision must be auto / on / off"));
        }
        self.call("setDayNightModeConfig", json!({"image": {"common": {"inf_type": mode}}})).await?;
        Ok(())
    }

    pub async fn set_motion_detection(&self, enabled: Option<bool>, sensitivity: Option<&str>) -> Result<()> {
        let mut det = serde_json::Map::new();
        if let Some(e) = enabled {
            det.insert("enabled".into(), json!(onoff(e)));
        }
        if let Some(s) = sensitivity {
            let (num, label) = match s {
                "low" => ("20", "low"),
                "medium" => ("50", "medium"),
                "high" => ("80", "high"),
                _ => bail!("{}", tr!("靈敏度只能是 low / medium / high", "Sensitivity must be low / medium / high")),
            };
            det.insert("digital_sensitivity".into(), json!(num));
            det.insert("sensitivity".into(), json!(label));
        }
        self.call("setDetectionConfig", json!({"motion_detection": {"motion_det": det}})).await?;
        Ok(())
    }

    pub async fn set_person_detection(&self, on: bool) -> Result<()> {
        self.call("setPersonDetectionConfig", json!({"people_detection": {"detection": {"enabled": onoff(on)}}}))
            .await?;
        Ok(())
    }

    pub async fn set_auto_track(&self, on: bool) -> Result<()> {
        self.call("setTargetTrackConfig", json!({"target_track": {"target_track_info": {"enabled": onoff(on)}}}))
            .await?;
        Ok(())
    }

    // 雲台（沒有 ONVIF 的機型使用）

    /// 相對移動一步：pan/tilt 為 -1.0 ~ 1.0
    pub async fn motor_move(&self, pan: f32, tilt: f32) -> Result<()> {
        let x = (pan * 10.0).round() as i32;
        let y = (tilt * 10.0).round() as i32;
        self.call("motorMove", json!({"motor": {"move": {"x_coord": x.to_string(), "y_coord": y.to_string()}}}))
            .await?;
        Ok(())
    }

    pub async fn presets(&self) -> Result<Vec<crate::onvif::Preset>> {
        let r = self.call("getPresetConfig", json!({"preset": {"name": ["preset"]}})).await?;
        let p = &r["preset"]["preset"];
        let ids = p["id"].as_array().cloned().unwrap_or_default();
        let names = p["name"].as_array().cloned().unwrap_or_default();
        Ok(ids
            .iter()
            .enumerate()
            .filter_map(|(i, id)| {
                let token = id.as_str().map(str::to_string).or_else(|| id.as_i64().map(|n| n.to_string()))?;
                let name = names.get(i).and_then(|n| n.as_str()).unwrap_or(&token).to_string();
                Some(crate::onvif::Preset { token, name })
            })
            .collect())
    }

    pub async fn goto_preset(&self, id: &str) -> Result<()> {
        self.call("motorMoveToPreset", json!({"preset": {"goto_preset": {"id": id}}})).await?;
        Ok(())
    }

    pub async fn save_preset(&self, name: &str) -> Result<()> {
        // 方法名稱的拼字錯誤是攝影機韌體本身的
        self.call("addMotorPostion", json!({"preset": {"set_preset": {"name": name, "save_ptz": "1"}}})).await?;
        Ok(())
    }

    pub async fn delete_preset(&self, id: &str) -> Result<()> {
        self.call("deletePreset", json!({"preset": {"remove_preset": {"id": [id]}}})).await?;
        Ok(())
    }
}

type Ccm = ccm::Ccm<aes::Aes128, ccm::consts::U16, ccm::consts::U12>;

enum SendError {
    /// Session 失效或無法解密：重新登入一次
    Session,
    Other(anyhow::Error),
}

#[derive(Debug)]
enum TpapError {
    /// 密碼不被接受（-40401），可換另一種格式再試
    Refused,
    Other(String),
}

impl std::fmt::Display for TpapError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TpapError::Refused => write!(f, "refused"),
            TpapError::Other(s) => write!(f, "{s}"),
        }
    }
}

fn onoff(b: bool) -> &'static str {
    if b { "on" } else { "off" }
}

/// SPAKE2+（P-256，RFC 9383 M/N 點）與 TP-Link 的密碼轉換，移植自 pytapo/transport/tpap/spake2p.py
pub(crate) mod spake2p {
    use hmac::{Hmac, Mac};
    use num_bigint::BigUint;
    use p256::elliptic_curve::sec1::{FromEncodedPoint, ToEncodedPoint};
    use p256::elliptic_curve::PrimeField;
    use p256::{AffinePoint, EncodedPoint, ProjectivePoint, Scalar};
    use serde_json::Value;
    use sha2::{Digest, Sha256};

    type HmacSha256 = Hmac<Sha256>;

    const M_HEX: &str = "02886e2f97ace46e55ba9dd7242579f2993b64e16ef3dcab95afd497333d8fa12f";
    const N_HEX: &str = "03d8bbd6c639c62937b04d997f38c3770719c629d7014d49a24b4f98baa1292b49";
    const ORDER_HEX: &str = "ffffffff00000000ffffffffffffffffbce6faada7179e84f3b9cac2fc632551";

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
    }

    fn point(bytes: &[u8]) -> Result<ProjectivePoint, String> {
        let ep = EncodedPoint::from_bytes(bytes).map_err(|_| "bad point".to_string())?;
        Option::<AffinePoint>::from(AffinePoint::from_encoded_point(&ep))
            .map(ProjectivePoint::from)
            .ok_or_else(|| "bad point".to_string())
    }

    fn encode(p: &ProjectivePoint) -> Vec<u8> {
        p.to_affine().to_encoded_point(false).as_bytes().to_vec()
    }

    fn order() -> BigUint {
        BigUint::parse_bytes(ORDER_HEX.as_bytes(), 16).unwrap()
    }

    fn scalar(n: &BigUint) -> Scalar {
        let mut b = n.to_bytes_be();
        while b.len() < 32 {
            b.insert(0, 0);
        }
        let mut arr = [0u8; 32];
        arr.copy_from_slice(&b[b.len() - 32..]);
        Option::from(Scalar::from_repr(arr.into())).unwrap_or(Scalar::ZERO)
    }

    fn be32(n: &BigUint) -> Vec<u8> {
        let mut b = n.to_bytes_be();
        while b.len() < 32 {
            b.insert(0, 0);
        }
        b
    }

    pub fn hkdf(ikm: &[u8], salt: Option<&[u8]>, info: &[u8], len: usize) -> Vec<u8> {
        let hk = hkdf::Hkdf::<Sha256>::new(Some(salt.unwrap_or(&[0u8; 32])), ikm);
        let mut out = vec![0u8; len];
        hk.expand(info, &mut out).expect("hkdf length");
        out
    }

    fn hmac(key: &[u8], data: &[u8]) -> [u8; 32] {
        let mut m = HmacSha256::new_from_slice(key).unwrap();
        m.update(data);
        m.finalize().into_bytes().into()
    }

    pub struct Client {
        pub x_bytes: Vec<u8>,
        pub user_confirm: [u8; 32],
        kcb: Vec<u8>,
        shared_key: Vec<u8>,
    }

    impl Client {
        pub fn new(
            dev_salt: &[u8],
            iterations: u32,
            dev_share: &[u8],
            user_random: &[u8],
            dev_random: &[u8],
            credential: &str,
        ) -> Result<Self, String> {
            let mut dk = [0u8; 80];
            pbkdf2::pbkdf2_hmac::<Sha256>(credential.as_bytes(), dev_salt, iterations, &mut dk);
            let n = order();
            let w0n = BigUint::from_bytes_be(&dk[..40]) % &n;
            let w1n = BigUint::from_bytes_be(&dk[40..]) % &n;
            let (w0, w1) = (scalar(&w0n), scalar(&w1n));

            let m = point(&hex(M_HEX))?;
            let np = point(&hex(N_HEX))?;
            let y = point(dev_share)?;
            let mut rnd = [0u8; 32];
            getrandom::getrandom(&mut rnd).map_err(|e| e.to_string())?;
            let x = scalar(&(BigUint::from_bytes_be(&rnd) % (&n - 1u32) + 1u32));

            let xp = ProjectivePoint::GENERATOR * x + m * w0;
            let h = y - np * w0;
            let x_bytes = encode(&xp);
            let y_bytes = encode(&y);

            let mut ctx = Sha256::new();
            ctx.update(b"PAKE V1");
            ctx.update(user_random);
            ctx.update(dev_random);
            let context = ctx.finalize().to_vec();

            let chunks: [Vec<u8>; 10] = [
                context,
                vec![],
                vec![],
                encode(&m),
                encode(&np),
                x_bytes.clone(),
                y_bytes.clone(),
                encode(&(h * x)),
                encode(&(h * w1)),
                be32(&w0n),
            ];
            let mut transcript = Vec::new();
            for c in &chunks {
                transcript.extend_from_slice(&(c.len() as u64).to_le_bytes());
                transcript.extend_from_slice(c);
            }
            let ke = Sha256::digest(&transcript);
            let conf = hkdf(&ke, None, b"ConfirmationKeys", 64);
            let user_confirm = hmac(&conf[..32], &y_bytes);
            let shared_key = hkdf(&ke, None, b"SharedKey", 32);
            Ok(Self { x_bytes, user_confirm, kcb: conf[32..].to_vec(), shared_key })
        }

        pub fn device_confirm_ok(&self, dev_confirm: &[u8]) -> bool {
            let mut m = HmacSha256::new_from_slice(&self.kcb).unwrap();
            m.update(&self.x_bytes);
            m.verify_slice(dev_confirm).is_ok()
        }

        pub fn session_key_and_nonce(&self) -> ([u8; 16], [u8; 12]) {
            let k = hkdf(&self.shared_key, Some(b"tp-kdf-salt-aes128-key"), b"tp-kdf-info-aes128-key", 32);
            let n = hkdf(&self.shared_key, Some(b"tp-kdf-salt-aes128-iv"), b"tp-kdf-info-aes128-iv", 32);
            let mut key = [0u8; 16];
            key.copy_from_slice(&k[..16]);
            let mut nonce = [0u8; 12];
            nonce.copy_from_slice(&n[..12]);
            (key, nonce)
        }
    }

    /// 依 pake_register 回傳的 extra_crypt 轉換密碼
    pub fn apply_extra_crypt(passcode: &str, extra: &Value) -> Result<String, String> {
        if extra.is_null() || extra.as_object().is_some_and(|o| o.is_empty()) {
            return Ok(passcode.to_string());
        }
        let kind = extra["type"].as_str().unwrap_or("").to_ascii_lowercase();
        if kind != "password_shadow" {
            return Err(format!("unsupported extra_crypt type {kind}"));
        }
        let params = &extra["params"];
        let id = params["passwd_id"].as_i64().or_else(|| params["passwd_id"].as_str().and_then(|s| s.parse().ok())).unwrap_or(0);
        match id {
            5 => Ok(sha256_crypt(passcode, params["passwd_prefix"].as_str().unwrap_or(""))),
            2 => {
                use sha1::Digest as _;
                Ok(sha1::Sha1::digest(passcode.as_bytes()).iter().map(|b| format!("{b:02x}")).collect())
            }
            other => Err(format!("unsupported password_shadow passwd_id {other}")),
        }
    }

    const CRYPT_B64: &[u8] = b"./0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

    fn to64(mut v: u32, n: usize) -> String {
        let mut s = String::new();
        for _ in 0..n {
            s.push(CRYPT_B64[(v & 0x3f) as usize] as char);
            v >>= 6;
        }
        s
    }

    fn repeat(digest: &[u8], len: usize) -> Vec<u8> {
        digest.iter().cycle().take(len).copied().collect()
    }

    /// SHA-256-crypt（"$5$"，Drepper）
    pub fn sha256_crypt(key: &str, prefix: &str) -> String {
        let rest = prefix.strip_prefix("$5$").unwrap_or(prefix);
        let (mut rounds, mut explicit, mut rest) = (5000usize, false, rest);
        if let Some(r) = rest.strip_prefix("rounds=") {
            if let Some((num, tail)) = r.split_once('$') {
                rounds = num.parse::<usize>().unwrap_or(5000).clamp(1000, 999_999_999);
                rest = tail;
                explicit = true;
            }
        }
        let end = rest.find('$').filter(|e| *e > 0).unwrap_or(rest.len());
        let salt: String = rest[..end].chars().take(16).collect();
        let (k, s) = (key.as_bytes(), salt.as_bytes());

        let b = Sha256::new().chain_update(k).chain_update(s).chain_update(k).finalize();
        let mut a = Sha256::new().chain_update(k).chain_update(s).chain_update(repeat(&b, k.len()));
        let mut n = k.len();
        while n > 0 {
            a.update(if n & 1 == 1 { &b[..] } else { k });
            n >>= 1;
        }
        let a = a.finalize();
        let mut dp = Sha256::new();
        for _ in 0..k.len() {
            dp.update(k);
        }
        let p = repeat(&dp.finalize(), k.len());
        let mut ds = Sha256::new();
        for _ in 0..(16 + a[0] as usize) {
            ds.update(s);
        }
        let sb = repeat(&ds.finalize(), s.len());

        let mut c = a.to_vec();
        for i in 0..rounds {
            let mut h = Sha256::new();
            h.update(if i & 1 == 1 { &p[..] } else { &c[..] });
            if i % 3 != 0 {
                h.update(&sb);
            }
            if i % 7 != 0 {
                h.update(&p);
            }
            h.update(if i & 1 == 1 { &c[..] } else { &p[..] });
            c = h.finalize().to_vec();
        }
        const ORDER: [(usize, usize, usize); 10] = [
            (0, 10, 20), (21, 1, 11), (12, 22, 2), (3, 13, 23), (24, 4, 14),
            (15, 25, 5), (6, 16, 26), (27, 7, 17), (18, 28, 8), (9, 19, 29),
        ];
        let mut enc: String = ORDER
            .iter()
            .map(|&(x, y, z)| to64(((c[x] as u32) << 16) | ((c[y] as u32) << 8) | c[z] as u32, 4))
            .collect();
        enc.push_str(&to64(((c[31] as u32) << 8) | c[30] as u32, 3));
        let rounds_part = if explicit { format!("rounds={rounds}$") } else { String::new() };
        format!("$5${rounds_part}{salt}${enc}")
    }

    #[cfg(test)]
    mod tests {
        #[test]
        fn sha256_crypt_known_vectors() {
            // Drepper 規格書的測試向量（已用 `openssl passwd -5` 交叉驗證）
            assert_eq!(
                super::sha256_crypt("Hello world!", "$5$saltstring"),
                "$5$saltstring$5B8vYYiY.CVt1RlTTf8KbXBH3hsxY/GNooZaBBGWEc5"
            );
            assert_eq!(
                super::sha256_crypt("Hello world!", "$5$rounds=10000$saltstringsaltstring"),
                "$5$rounds=10000$saltstringsaltst$3xv.VbSHBb41AL9AvLeujZkZRBAwqFMz2.opqey6IcA"
            );
        }
    }
}
