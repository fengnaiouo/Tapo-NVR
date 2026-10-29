//! 列出資料庫中的攝影機設定（不顯示密碼，不會隨產品發布）：
//!   cargo run -p tapo-nvr --example listcams -- <資料夾>

fn main() -> anyhow::Result<()> {
    let dir = std::path::PathBuf::from(std::env::args().nth(1).expect("data dir"));
    let cfg = tapo_nvr::read_config(&dir)?;
    for c in &cfg.cameras {
        println!(
            "id={} name={} host={} stream={:?} rtsp_port={} onvif_port={} user={} has_password={} has_cloud_password={} motion={} record={:?}",
            c.id,
            c.name,
            c.host,
            c.stream,
            c.rtsp_port,
            c.onvif_port,
            c.username,
            !c.password.is_empty(),
            !c.cloud_password.is_empty(),
            c.motion,
            c.record
        );
    }
    if cfg.cameras.is_empty() {
        println!("（沒有攝影機）");
    }
    Ok(())
}
