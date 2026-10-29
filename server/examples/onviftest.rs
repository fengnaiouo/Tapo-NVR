//! 診斷 ONVIF 事件拉取（不會隨產品發布）：
//!   cargo run -p tapo-nvr --example onviftest -- <資料夾> <攝影機代號> <等待秒數...>
//! 每個等待秒數各訂閱一次並拉取 3 次，印出完整錯誤鏈；結束時取消訂閱。

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let dir = std::path::PathBuf::from(&args[1]);
    let id = &args[2];
    let waits: Vec<u64> = args[3..].iter().filter_map(|s| s.parse().ok()).collect();
    let cfg = tapo_nvr::read_config(&dir)?;
    let cam = cfg.cameras.iter().find(|c| &c.id == id).expect("camera");
    let client = tapo_nvr::onvif::OnvifClient::new(&cam.host, cam.onvif_port, &cam.username, &cam.password);
    for wait in waits {
        let sub = match client.subscribe().await {
            Ok(s) => s,
            Err(e) => {
                println!("subscribe failed: {e:#}");
                continue;
            }
        };
        println!("== wait={wait}s address={}", sub.address);
        for i in 0..3 {
            let t = std::time::Instant::now();
            match client.pull_wait(&sub, wait).await {
                Ok(ev) => println!("  pull {i}: ok in {:.1}s, {} events", t.elapsed().as_secs_f32(), ev.len()),
                Err(e) => println!("  pull {i}: ERROR after {:.1}s: {e:#}", t.elapsed().as_secs_f32()),
            }
        }
        client.unsubscribe(&sub).await;
    }
    Ok(())
}
