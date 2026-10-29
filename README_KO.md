# Tapo NVR

[English](README.md) | [繁體中文](README_TW.md) | [简体中文](README_CN.md) | [日本語](README_JP.md) | **한국어**

Rust로 개발된 TP-Link Tapo 카메라용 무료 오픈소스 모니터링·녹화 시스템입니다. **Windows 데스크톱 앱**(서버 내장)과 **웹 버전**을 제공하며 두 버전의 기능은 동일합니다. UI는 영어, 번체 중국어, 간체 중국어를 지원합니다(시스템 언어에 따라 자동 선택).

🌐 웹사이트: <https://fengnaiouo.github.io/Tapo-NVR/> · ⬇️ [최신 버전 다운로드](https://github.com/fengnaiouo/Tapo-NVR/releases/latest)

## 기능

| 분류 | 내용 |
|---|---|
| 실시간 영상 | 다중 카메라 그리드, 2K 메인 스트림, LAN에서 저지연 WebRTC, 인터넷에서 MSE, 1–4배 디지털 줌 |
| PTZ | 길게 눌러 연속 이동, 키보드 방향키, 프리셋 추가/이동/삭제(ONVIF 또는 Tapo API) |
| 카메라 제어 | 사생활 보호 모드, 야간 모드, 움직임 감지 및 감도, 사람 감지, 자동 추적, SD 카드 상태 |
| 녹화 | 카메라별 **상시 녹화** 또는 **움직임 감지 시에만 녹화**(전후 영상 포함), 보관 기간과 용량 제한, 디스크 공간 부족 시 자동 정리 |
| 재생 | 24시간 타임라인(15분까지 확대), 이벤트 표시, 0.5–16배속, 스냅샷, **MP4 내보내기** |
| 알림 | 움직임 감지, 스냅샷, 이벤트 기록; Windows 기본 알림, 브라우저 알림, **휴대폰 푸시 알림**(ntfy); 원클릭 **경계 / 해제**, **알림 시간대**(예: 야간만), **카메라 오프라인 / 재연결** 알림 |
| 양방향 오디오 | PC에서 카메라로 말하기(TP-Link 계정 비밀번호 필요) |
| 지원 기종 | LAN 자동 검색(ONVIF + TP-Link TDP), C1xx / C2xx / C3xx / C5xx 등 Tapo 기종 지원; RTSP가 없는 기종은 Tapo 전용 스트림 사용; 기존 및 신규(TPAP) 로그인 방식 지원 |
| 녹화 일정 | 시간대별로 상시 녹화 / 움직임 녹화 / 녹화 안 함 전환 |
| 스마트 감지 | 사람, 차량, 반려동물, 아기 울음, 선 넘기, 영역 침입 감지(카메라별 알림 종류 선택) |
| IP 변경 자동 추적 | 카메라 IP가 바뀌어도 MAC 주소로 자동으로 찾아 설정 갱신 |
| 다중 사용자 | 관리자와 시청·재생·내보내기만 가능한 "보기 전용" 사용자 |
| 시스템 상태 | 연결, 움직임 감지, 녹화, 용량, 경고를 한 페이지에서 확인 |
| 원격 시청 | LAN에서 직접 연결하거나 Cloudflare Tunnel을 통해 외부에서 연결 |
| 설정 | **모든 설정은 앱 또는 웹의 ⚙ 설정 화면에서 변경**, 대부분 즉시 적용; 설정 백업 및 복원(비밀번호 제외), 시스템 로그 보기, "업데이트 및 정보"에서 버전과 업데이트 상태 확인 |
| 데스크톱 앱 | 시스템 트레이 상주, Windows 시작 시 자동 실행, 창을 닫아도 백그라운드에서 계속 녹화, 앱 내 자동 업데이트 |
| 보안 | 설정은 SQLite에 저장되며 **카메라 비밀번호는 Windows DPAPI로 암호화**; 비밀번호는 설정 파일이나 프로세스 명령줄에 나타나지 않음; 세션은 해시만 저장하며 **브라우저 / 앱을 닫거나 다시 시작하면 다시 로그인 필요**; Argon2, 로그인 시도 제한, CSRF 방어; 카메라 비밀번호는 브라우저로 전송되지 않음 |

## 사용 방법

### 1. 카메라 준비(Tapo 앱)

1. 카메라 → ⚙ 설정 → **고급 설정 → 카메라 계정**: RTSP/ONVIF용 사용자 이름과 비밀번호 생성
2. **나 → 타사 서비스 → 타사 호환성**: 켜기
3. 공유기에서 카메라에 **고정 IP** 할당

### 2a. 데스크톱 앱(권장)

1. [Releases](https://github.com/fengnaiouo/Tapo-NVR/releases/latest)에서 `Tapo-NVR_x.y.z_x64-setup.exe`를 다운로드하여 실행
2. 처음 실행할 때 관리자 계정 생성
3. **⚙ 설정 → 카메라**: **LAN 검색** → **추가** → 카메라 계정 입력 → **연결 테스트** → 저장
4. **⚙ 설정 → 녹화**: 녹화 폴더 선택(용량이 큰 드라이브 권장) 후 카메라별 녹화 모드 선택
5. (선택) TP-Link 계정 비밀번호를 입력하면 사생활 보호 모드, 야간 모드, 양방향 오디오 등을 사용할 수 있습니다. 비밀번호만 필요하며 카메라의 로컬 로그인 사용자는 항상 `admin`입니다

- 데이터는 설치 폴더(예: `D:\Tapo NVR\`)에 저장됩니다: `data\`(설정 데이터베이스, 이벤트, 스냅샷), `logs\`, `webview\`. 녹화 파일은 선택한 폴더에 저장됩니다. 업데이트해도 데이터는 유지되며, 제거할 때 "애플리케이션 데이터 삭제"를 선택한 경우에만 삭제됩니다
- 설치 폴더에 쓸 수 없는 경우(예: `C:\Program Files`) `%APPDATA%\com.tapo-nvr.desktop\`를 사용합니다
- 0.2.x에서 업그레이드하면 기존 `config.toml`을 자동으로 가져오고 파일 안의 비밀번호는 삭제됩니다
- **앱 환경설정**에서 **다른 PC에 연결** 모드로 전환하면 시청 전용으로 사용할 수 있습니다

### 2b. 독립 실행형 서버

```powershell
.\scripts\setup.bat     # go2rtc / ffmpeg 다운로드 및 빌드
.\scripts\start.bat     # 실행 후 http://localhost:8080 열기
```

### 다른 기기에서 시청

같은 LAN의 휴대폰이나 PC 브라우저에서 `http://<Tapo NVR을 실행 중인 PC의 IP>:8080`을 엽니다.

**Windows 방화벽**(개인 네트워크만 허용):

```powershell
New-NetFirewallRule -DisplayName "Tapo NVR Web" -Direction Inbound -Protocol TCP -LocalPort 8080 -Profile Private -Action Allow
New-NetFirewallRule -DisplayName "Tapo NVR WebRTC" -Direction Inbound -Protocol UDP -LocalPort 8555 -Profile Private -Action Allow
New-NetFirewallRule -DisplayName "Tapo NVR WebRTC TCP" -Direction Inbound -Protocol TCP -LocalPort 8555 -Profile Private -Action Allow
```

외부에서 시청하려면 [docs/cloudflare-tunnel.md](docs/cloudflare-tunnel.md)(번체 중국어)를 참고하세요.

## 업데이트

데스크톱 앱은 실행 30초 후와 6시간마다 새 버전을 확인합니다. 새 버전이 있으면 앱 창 오른쪽 아래에 업데이트 내용이 담긴 카드가 표시됩니다. **지금 업데이트**를 누르면 진행 표시줄과 함께 다운로드한 뒤 자동으로 설치하고 다시 시작합니다. 시스템 트레이 메뉴 → **업데이트 확인**에서 직접 확인할 수도 있습니다.

## 녹화 안내

- 파일: `{녹화 폴더}\{카메라 ID}\{날짜}\{시-분-초}.mp4`, 일반 동영상 플레이어로 재생 가능
- 녹화 중인 구간은 `.rec.mp4`(비정상 종료에도 손상되지 않음)이며 완료 후 일반 `.mp4`로 변환됩니다
- **움직임 녹화**는 계속 녹화하면서 움직임이 없는 구간을 자동으로 삭제하므로 이벤트 이전 영상도 남습니다
- 2K 상시 녹화는 카메라 1대당 하루 약 15–25 GB를 사용합니다

## 참고 사항

- **양방향 오디오**는 TP-Link 계정 비밀번호가 필요하며 로컬, 데스크톱 앱 또는 HTTPS 환경에서 사용할 수 있습니다. Cloudflare Tunnel을 통해서는 사용할 수 없습니다
- **잠금 방지**: Tapo 카메라는 비밀번호를 여러 번 틀리면 잠기므로, 비밀번호가 틀리면 재시도를 중단합니다
- 이 프로젝트는 TP-Link와 관련이 없습니다. Tapo는 TP-Link의 상표입니다

## 라이선스

[GNU AGPL-3.0](LICENSE). Tapo NVR을 수정하여 다른 사람에게 제공하는 경우(네트워크를 통한 서비스 포함) 같은 라이선스로 소스 코드를 공개해야 합니다. 함께 배포되는 프로그램(go2rtc, FFmpeg)은 [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md)를 참고하세요.
