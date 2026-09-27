# Cam Rover ESP32 (Rust)

Keyestudio KS5024 ESP32-CAM 4WD 로봇용 Rust 펌웨어입니다. 애플리케이션, Wi-Fi, HTTP 제어 및 안전 로직은 Rust로 작성했고, OV2640 카메라와 LEDC 모터 PWM의 저수준 초기화만 Espressif ESP-IDF C 드라이버를 얇게 감싸 사용합니다.

Rust 도구 설치부터 빌드·업로드·보드 실행까지의 시각적 설명은 [HTML 빌드 안내서](docs/rust-build-guide.html)를 참고하세요.

## 제공 기능

- 로봇 자체 WPA2 Wi-Fi AP (`cam-rover` / `camrover`)
- `http://192.168.71.1` 모바일 제어 화면
- OV2640/OV3660 MJPEG 영상 스트림
- 전진, 후진, 좌/우 제자리 회전, 네 방향 대각선 주행, 정지
- 가청 소음을 줄인 20kHz PWM 속도 조절(85~255)과 GPIO4 플래시 LED
- 마지막 이동 명령 후 700ms가 지나면 자동 정지하는 dead-man 안전장치

## 하드웨어 배선

공식 KS5024 문서와 동일합니다.

| ESP32-CAM | L298N |
| --- | --- |
| GPIO14 | IN1 (오른쪽) |
| GPIO15 | IN2 (오른쪽) |
| GPIO13 | IN3 (왼쪽) |
| GPIO12 | IN4 (왼쪽) |
| 5V | 5V |
| GND | GND |

L298N의 ENA/ENB 점퍼는 꽂아 둡니다. microSD 핀과 모터 핀이 겹치므로 이 펌웨어에서는 microSD를 사용할 수 없습니다.

## macOS 개발 환경

ESP32는 Xtensa라서 일반 Rust 설치만으로는 빌드되지 않습니다. 먼저 Apple Command Line Tools와 Rust/ESP 도구를 설치합니다.

```bash
xcode-select --install
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"
cargo +stable install espup --locked
espup install
source "$HOME/export-esp.sh"
cargo +stable install ldproxy --locked
cargo +stable install espflash --locked
```

CH340 포트가 나타나지 않을 때만 첨부된 `/Users/changwoo/Downloads/MAC/CH34xVCPDriver.dmg` 드라이버를 설치하세요. 최근 macOS는 드라이버 없이 인식되는 경우도 있습니다.

## 빌드와 업로드

1. 배터리/모터 전원은 끄고 USB-C로 ESP32-CAM 어댑터를 연결합니다.
2. BOOT 버튼을 누른 채 RESET을 한 번 눌러 다운로드 모드로 진입합니다.
3. 포트를 확인하고 업로드합니다.

```bash
espflash list-ports
cargo build --release
espflash flash --monitor --port /dev/cu.wchusbserialXXXX \
  target/xtensa-esp32-espidf/release/cam-rover-esp32
```

업로드가 끝나면 BOOT에서 손을 떼고 RESET을 누릅니다. 휴대폰에서 `cam-rover`에 연결한 뒤 `http://192.168.71.1`을 엽니다.

Wi-Fi 이름과 암호, 영상 상하 반전은 빌드 환경변수로 바꿀 수 있습니다. 암호는 8자 이상이어야 합니다.

```bash
ROVER_WIFI_SSID=my-rover \
ROVER_WIFI_PASSWORD=change-me \
ROVER_VIDEO_FLIP=0 \
cargo build --release
espflash flash --monitor --port /dev/cu.wchusbserialXXXX \
  target/xtensa-esp32-espidf/release/cam-rover-esp32
```

카메라는 조립 방향에 맞춰 기본적으로 상하 반전됩니다. 반전하지 않으려면
`ROVER_VIDEO_FLIP=0`으로 빌드하세요.

## 주의

- 로봇 바퀴를 바닥에서 띄운 상태로 처음 시험하세요.
- ESP32-CAM은 2.4GHz Wi-Fi만 지원합니다.
- 영상 스트림은 로컬 AP 내부의 암호화되지 않은 HTTP입니다. 인터넷에 포트 포워딩하지 마세요.
- 전원 강하로 재부팅되면 배터리 상태, 공통 GND, L298N 5V 점퍼와 배선을 먼저 확인하세요.
