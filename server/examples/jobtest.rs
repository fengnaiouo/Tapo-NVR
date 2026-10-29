//! 驗證自動更新的 Job Object 問題（不會隨產品發布）：
//!   cargo run -p tapo-nvr --example jobtest -- <marker 檔> [release]
//! 模擬 tauri-plugin-updater：ShellExecuteW 啟動子程序後立刻 exit(0)。
//! 子程序 3 秒後寫入 marker 檔；檔案出現代表子程序沒有被一起終止。

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let marker = args.get(1).expect("marker path").clone();
    let release = args.get(2).is_some_and(|a| a == "release");
    tapo_nvr::kill_children_on_exit();
    if release {
        tapo_nvr::release_children_on_exit();
    }
    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::UI::Shell::ShellExecuteW;
        let wide = |s: &str| s.encode_utf16().chain(std::iter::once(0)).collect::<Vec<u16>>();
        let file = wide("powershell.exe");
        let params = wide(&format!("-NoProfile -WindowStyle Hidden -Command \"Start-Sleep 3; Set-Content -Path '{marker}' -Value ok\""));
        let verb = wide("open");
        let r = ShellExecuteW(std::ptr::null_mut(), verb.as_ptr(), file.as_ptr(), params.as_ptr(), std::ptr::null(), 0);
        println!("ShellExecuteW = {}", r as isize);
    }
    std::process::exit(0);
}
