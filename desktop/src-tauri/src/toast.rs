//! 系統原生通知（附截圖、點擊開啟該攝影機）。
//! - Windows：WinRT Toast，專用執行緒建立並保留參考，點擊回呼才不會因物件被釋放而失效
//! - Linux / macOS：notify-rust（Linux 走 D-Bus 通知服務）

use std::path::PathBuf;

pub struct Request {
    pub title: String,
    pub body: String,
    pub image: Option<PathBuf>,
    pub on_click: Box<dyn Fn() + Send + Sync + 'static>,
}

#[cfg(windows)]
mod imp {
    use std::collections::VecDeque;
    use std::sync::mpsc::{channel, Sender};
    use std::sync::OnceLock;

    use tauri_winrt_notification::{Sound, Toast};

    use super::Request;

    static TX: OnceLock<Sender<Request>> = OnceLock::new();

    /// 開發中（target 資料夾內執行）沒有註冊 AppUserModelID，借用 PowerShell 的 ID
    fn app_id(identifier: &str) -> String {
        let dev = std::env::current_exe()
            .map(|p| p.to_string_lossy().to_lowercase().contains("\\target\\"))
            .unwrap_or(true);
        if dev { Toast::POWERSHELL_APP_ID.to_string() } else { identifier.to_string() }
    }

    pub fn init(identifier: &str) {
        let id = app_id(identifier);
        let (tx, rx) = channel::<Request>();
        std::thread::Builder::new()
            .name("toast".into())
            .spawn(move || {
                let mut alive: VecDeque<Toast> = VecDeque::new();
                for req in rx {
                    let on_click = std::sync::Arc::new(req.on_click);
                    let build = |app_id: &str| {
                        let cb = on_click.clone();
                        let mut t = Toast::new(app_id)
                            .title(&req.title)
                            .text1(&req.body)
                            .sound(Some(Sound::Default))
                            .on_activated(move |_| {
                                cb();
                                Ok(())
                            });
                        if let Some(img) = req.image.as_ref().filter(|p| p.is_absolute() && p.exists()) {
                            t = t.image(img, "snapshot");
                        }
                        t
                    };
                    let mut toast = build(&id);
                    if let Err(e) = toast.show() {
                        tracing::warn!("通知顯示失敗（{e}），改用替代 ID");
                        toast = build(Toast::POWERSHELL_APP_ID);
                        if let Err(e) = toast.show() {
                            tracing::warn!("通知顯示失敗：{e}");
                            continue;
                        }
                    }
                    alive.push_back(toast);
                    while alive.len() > 30 {
                        alive.pop_front();
                    }
                }
            })
            .expect("toast thread");
        let _ = TX.set(tx);
    }

    pub fn show(req: Request) {
        if let Some(tx) = TX.get() {
            let _ = tx.send(req);
        }
    }
}

#[cfg(not(windows))]
mod imp {
    use super::Request;

    pub fn init(_identifier: &str) {}

    pub fn show(req: Request) {
        // 每則通知一條執行緒：Linux 上會等待使用者點擊（或通知關閉）後結束
        std::thread::spawn(move || {
            let mut n = notify_rust::Notification::new();
            n.summary(&req.title).body(&req.body).appname("Tapo NVR");
            if let Some(img) = req.image.as_ref().filter(|p| p.is_absolute() && p.exists()) {
                n.image_path(&img.to_string_lossy());
            }
            #[cfg(all(unix, not(target_os = "macos")))]
            {
                n.action("default", "Open");
                match n.show() {
                    Ok(handle) => handle.wait_for_action(|action| {
                        if action == "default" {
                            (req.on_click)();
                        }
                    }),
                    Err(e) => tracing::warn!("通知顯示失敗：{e}"),
                }
            }
            #[cfg(target_os = "macos")]
            if let Err(e) = n.show() {
                tracing::warn!("通知顯示失敗：{e}");
            }
        });
    }
}

pub use imp::{init, show};
