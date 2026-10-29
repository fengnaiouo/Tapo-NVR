# Third-party software / 第三方軟體

Tapo NVR is licensed under the [GNU AGPL-3.0](LICENSE). The Windows installer also bundles the following programs, which are distributed under their own licenses and run as separate processes.

Tapo NVR 採用 [GNU AGPL-3.0](LICENSE) 授權。Windows 安裝檔另外附帶下列程式，它們依各自的授權散布，並以獨立程序執行。

| Program / 程式 | License / 授權 | Source / 原始碼 |
|---|---|---|
| [go2rtc](https://github.com/AlexxIT/go2rtc) | MIT | <https://github.com/AlexxIT/go2rtc> |
| [FFmpeg](https://ffmpeg.org/) (gyan.dev essentials build, `--enable-gpl --enable-version3`) | GPL-3.0-or-later | <https://ffmpeg.org/download.html> · build: <https://github.com/GyanD/codexffmpeg> |

Rust crates used by Tapo NVR are listed in [`Cargo.lock`](Cargo.lock); their licenses can be listed with `cargo license` or `cargo about`.

Tapo NVR 使用的 Rust 套件列於 [`Cargo.lock`](Cargo.lock)，可用 `cargo license` 或 `cargo about` 列出各套件授權。

"TP-Link" and "Tapo" are trademarks of TP-Link. This project is not affiliated with TP-Link.

「TP-Link」與「Tapo」為 TP-Link 的商標，本專案與 TP-Link 無關。
