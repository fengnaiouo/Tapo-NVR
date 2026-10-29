use std::io::Write;
use std::path::PathBuf;

use anyhow::Result;

fn usage() -> ! {
    eprintln!(
        "用法 / Usage:\n  tapo-nvr [--data data] [--import config.toml]   啟動伺服器 / start the server\n  tapo-nvr check [--data data]                    測試攝影機 / test cameras\n\n設定存在 data/tapo-nvr.db（機密以 Windows DPAPI 加密）。目前資料夾若有舊版 config.toml 會自動匯入。"
    );
    std::process::exit(2);
}

fn main() -> Result<()> {
    tapo_nvr::kill_children_on_exit();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,tower_http=warn".into()),
        )
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
            "hash-password" => command = "hash-password",
            "check" => command = "check",
            "run" => command = "run",
            _ => usage(),
        }
        i += 1;
    }

    let rt = tokio::runtime::Runtime::new()?;
    let code = match command {
        "hash-password" => {
            hash_password_cmd()?;
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

fn hash_password_cmd() -> Result<()> {
    let read = |prompt: &str| -> Result<String> {
        print!("{prompt}");
        std::io::stdout().flush()?;
        let mut s = String::new();
        std::io::stdin().read_line(&mut s)?;
        Ok(s.trim_end_matches(['\r', '\n']).to_string())
    };
    let a = read("輸入新密碼：")?;
    let b = read("再輸入一次：")?;
    if a != b {
        anyhow::bail!("兩次輸入不一致");
    }
    if a.chars().count() < 8 {
        anyhow::bail!("密碼至少 8 個字元");
    }
    println!("\n請貼到 config.toml 的 [auth] password_hash：\n{}", tapo_nvr::hash_password(&a)?);
    Ok(())
}
