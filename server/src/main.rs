use std::io::Write;
use std::path::PathBuf;

use anyhow::Result;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

fn usage() -> ! {
    eprintln!(
        "用法 / Usage:\n  tapo-nvr [--data data] [--import config.toml]   啟動伺服器 / start the server\n  tapo-nvr check [--data data]                    測試攝影機 / test cameras\n  tapo-nvr reset-password [--data data]           重設管理員密碼 / reset the admin password\n\n設定存在 <資料夾>/tapo-nvr.db（Windows 以 DPAPI 加密機密；Linux 以資料夾內權限 600 的金鑰檔加密）。\nSettings are stored in <data>/tapo-nvr.db. go2rtc / ffmpeg are looked up next to the executable (bin/) and in PATH."
    );
    std::process::exit(2);
}

fn main() -> Result<()> {
    tapo_nvr::kill_children_on_exit();
    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info,tower_http=warn".into()))
        .with(tracing_subscriber::fmt::layer())
        .with(tapo_nvr::log_layer())
        .init();

    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut command = "run";
    let mut data_dir = PathBuf::from("data");
    let mut legacy = PathBuf::from("config.toml");
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--data" | "-d" => {
                i += 1;
                data_dir = args.get(i).map(PathBuf::from).unwrap_or_else(|| usage());
            }
            "--import" | "--config" | "-c" => {
                i += 1;
                legacy = args.get(i).map(PathBuf::from).unwrap_or_else(|| usage());
            }
            "reset-password" | "hash-password" => command = "reset-password",
            "check" => command = "check",
            "run" => command = "run",
            _ => usage(),
        }
        i += 1;
    }

    let rt = tokio::runtime::Runtime::new()?;
    let code = match command {
        "reset-password" => {
            reset_password_cmd(&data_dir)?;
            0
        }
        "check" => {
            rt.block_on(tapo_nvr::check(&data_dir))?;
            0
        }
        _ => rt.block_on(async {
            let server = tapo_nvr::Server::start(tapo_nvr::Options {
                data_dir,
                legacy_config: Some(legacy),
                go2rtc_bin: None,
                ffmpeg_bin: None,
                can_restart: std::env::var("TAPO_NVR_SUPERVISED").is_ok(),
                desktop_embedded: false,
            })
            .await?;
            let token = server.shutdown_handle();
            let stop = tokio::spawn(async move {
                let _ = tokio::signal::ctrl_c().await;
                tracing::info!("正在關閉…");
                token.cancel();
            });
            let code = server.wait().await;
            stop.abort();
            anyhow::Ok(code)
        })?,
    };
    rt.shutdown_timeout(std::time::Duration::from_secs(2));
    std::process::exit(code);
}

/// 忘記密碼時使用：直接把新的管理員密碼寫入資料庫（伺服器需先關閉）
fn reset_password_cmd(data_dir: &std::path::Path) -> Result<()> {
    let read = |prompt: &str| -> Result<String> {
        print!("{prompt}");
        std::io::stdout().flush()?;
        let mut s = String::new();
        std::io::stdin().read_line(&mut s)?;
        Ok(s.trim_end_matches(['\r', '\n']).to_string())
    };
    let cfg = tapo_nvr::read_config(data_dir)?;
    println!("資料庫 / Database: {}", data_dir.join(tapo_nvr::store::DB_FILE).display());
    let user = read(&format!("管理員帳號 / Username [{}]: ", cfg.auth.username))?;
    let a = read("新密碼 / New password: ")?;
    let b = read("再輸入一次 / Repeat: ")?;
    if a != b {
        anyhow::bail!("兩次輸入不一致 / Passwords do not match");
    }
    if a.chars().count() < 8 {
        anyhow::bail!("密碼至少 8 個字元 / At least 8 characters");
    }
    let hash = tapo_nvr::hash_password(&a)?;
    tapo_nvr::modify_config(data_dir, |c| {
        if !user.trim().is_empty() {
            c.auth.username = user.trim().to_string();
        }
        c.auth.password_hash = hash;
    })?;
    println!("已重設管理員密碼 / Admin password has been reset.");
    Ok(())
}
