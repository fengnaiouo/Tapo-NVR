# Tapo NVR 伺服器映像檔（linux/amd64、linux/arm64）
# 執行檔由 GitHub Actions 先在各架構原生編譯，放在 dist/linux-<arch>/（見 .github/workflows/release-linux.yml）
FROM debian:bookworm-slim

ARG TARGETARCH
RUN apt-get update \
 && apt-get install -y --no-install-recommends ffmpeg ca-certificates libssl3 tzdata \
 && rm -rf /var/lib/apt/lists/*

COPY dist/linux-${TARGETARCH}/tapo-nvr /usr/local/bin/tapo-nvr
COPY dist/linux-${TARGETARCH}/go2rtc /usr/local/bin/go2rtc
COPY LICENSE THIRD_PARTY_NOTICES.md /usr/share/doc/tapo-nvr/
RUN chmod 0755 /usr/local/bin/tapo-nvr /usr/local/bin/go2rtc \
 && mkdir -p /data /recordings

# 結束碼 3（網頁上的「重新啟動」）由容器的 restart 策略重新啟動
ENV TAPO_NVR_SUPERVISED=1 \
    TZ=Asia/Taipei
VOLUME ["/data", "/recordings"]
EXPOSE 8080/tcp 8555/tcp 8555/udp
WORKDIR /data
ENTRYPOINT ["/usr/local/bin/tapo-nvr", "--data", "/data"]

LABEL org.opencontainers.image.title="Tapo NVR" \
      org.opencontainers.image.description="Monitoring, recording, playback and motion alerts for TP-Link Tapo cameras" \
      org.opencontainers.image.source="https://github.com/fengnaiouo/Tapo-NVR" \
      org.opencontainers.image.licenses="AGPL-3.0-or-later"
