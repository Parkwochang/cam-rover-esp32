# Cam Rover ESP32 (Rust)

[English README](README.md)

Keyestudio KS5024 ESP32-CAM 4WD 로봇용 Rust 펌웨어입니다. Wi-Fi, HTTP 제어, 이동 판단, 안전 정지 로직은 Rust로 작성했습니다. 카메라·모터 PWM·플래시 LED의 저수준 제어는 작은 C 컴포넌트가 ESP-IDF와 `esp32-camera` API를 호출합니다. 빌드에 Arduino IDE는 필요하지 않습니다.

## 제공 기능

- 로봇 자체 WPA2 Wi-Fi AP: `cam-rover` (기본 암호: `camrover`)
- 주변 2.4GHz Wi-Fi 검색, 실제 접속·IP 할당 검증 성공 후에만 접속 정보 저장
- 집 Wi-Fi에 연결되어도 로봇 AP(`http://192.168.71.1`) 유지; 공유기가 할당한 IP로도 접속 가능
- OV2640/OV3660 MJPEG 영상 스트림
- 전진, 후진, 좌우 제자리 회전, 네 방향 대각선 주행, 정지
- 가청 소음을 줄인 20kHz 모터 PWM과 85~255 속도 조절, GPIO4 플래시 LED
- 마지막 이동 명령 이후 700ms가 지나면 모터 자동 정지

## Rust와 C가 연결되는 방식

`.h` 파일은 언어 경계에서 공유할 함수의 이름과 입출력 타입을 선언하고, `.c` 파일은 함수를 구현합니다. `CMakeLists.txt`는 ESP-IDF 컴포넌트의 규약 파일로, 컴파일할 C 소스·헤더 폴더·의존 컴포넌트를 지정합니다. `Cargo.toml`은 `esp-idf-sys`에 컴포넌트 폴더와 Rust 바인딩 생성에 사용할 헤더를 알려줍니다. `#include`는 C 컴파일러에 함수와 타입 선언을 보여 주는 것이지 SDK 전체를 소스 파일에 복사하는 명령이 아닙니다. 빌드 과정에서 C 구현체와 Rust 앱을 각각 컴파일하고 최종 연결합니다.

예를 들어 `rover_hardware.c`는 다음을 포함한 헤더들을 읽습니다.

```c
#include "rover_hardware.h"
#include "driver/gpio.h"
#include "driver/ledc.h"
#include "esp_camera.h"
#include "esp_psram.h"
```

`CMakeLists.txt`의 `REQUIRES esp32-camera driver esp_psram`이 해당 헤더 경로와 라이브러리 의존성을 제공하고, C 컴파일 시 선언을 확인한 뒤 구현체는 최종 펌웨어에 연결됩니다.

```mermaid
flowchart TD
    A["Cargo.toml: extra_components"] --> B["esp-idf-sys 빌드"]
    B --> C[".embuild/의 ESP-IDF SDK 5.4.3"]
    B --> D["target/.../managed_components/의 esp32-camera"]
    B -->|"등록된 컴포넌트 읽기"| E["CMakeLists.txt: SRCS, INCLUDE_DIRS, REQUIRES"]
    E --> F["rover_hardware.c 컴파일"]
    C -->|"#include driver/gpio.h, driver/ledc.h, esp_psram.h"| F
    D -->|"#include esp_camera.h"| F
    H["rover_hardware.h: C 함수 선언"] -->|"#include rover_hardware.h"| F
    H --> G["Rust rover FFI 바인딩 생성"]
    F --> J["C 목적 파일과 Rust 코드 연결"]
    G --> J
    R["src/*.rs"] --> J
    J --> I["target/.../release/의 ESP32 ELF"]
```

보드에서 이동 명령과 안전 정지는 다음 경로로 동작합니다.

```mermaid
flowchart LR
    A["웹 화면 또는 라즈베리 파이 Go 서버"] -->|"GET /api/move"| B["Rust HTTP 서버 :80"]
    B --> C["Motion 파싱·RoverState 갱신"]
    C --> D["src/hardware.rs · 생성된 FFI"]
    D --> E["rover_hardware.c: LEDC PWM"]
    E --> F["L298N 모터"]
    G["안전 태스크: 100ms마다 검사"] -.->|"last_command 확인"| C
    G -->|"마지막 명령에서 700ms 초과"| H["hardware::stop()"]
    H --> F
```

| 기능 | C 브리지에서 호출하는 SDK 함수 | 결과 |
| --- | --- | --- |
| 카메라 초기화 | `esp_psram_is_initialized`, `esp_camera_init`, 센서 반전 설정 | 프레임 버퍼와 JPEG 설정 선택 |
| 영상 촬영·반납 | `esp_camera_fb_get` / `esp_camera_fb_return` | Rust가 JPEG를 MJPEG로 전송한 뒤 버퍼 반납 |
| 모터 구동 | `ledc_timer_config`, `ledc_channel_config`, `ledc_set_duty` | L298N 입력 핀에 20kHz PWM 출력 |
| 조명 제어 | `gpio_config`, `gpio_set_level` | GPIO4 플래시 LED 제어 |

Wi-Fi와 HTTP는 Rust가 `esp-idf-svc`를 통해 직접 사용합니다. 이 프로젝트의 C 브리지는 카메라·모터 PWM·LED에만 사용되며, 촬영한 프레임 데이터는 브리지를 거쳐 별도 81번 포트 MJPEG 서버로 전달됩니다. C 드라이버를 FFI로 호출하는 것은 이 프로젝트의 설계 선택이지 Rust로 하드웨어를 제어할 때 항상 필요한 조건은 아닙니다.

## 소스와 의존성의 위치

| 경로 | 역할 |
| --- | --- |
| `Cargo.toml` | Rust 의존성과 ESP-IDF 5.4.3·`esp32-camera` 2.1.7 설정 |
| `Cargo.lock` | 확정된 Rust 크레이트 버전 |
| `components_esp32.lock` | 생성된 ESP-IDF 컴포넌트 버전. 이 저장소에서는 Git에서 제외 |
| `rust-toolchain.toml`, `.cargo/config.toml` | ESP Xtensa 툴체인, 타깃, 링커, 업로드 실행 설정 |
| `src/*.rs` | Rust 앱, 이동 로직, C 호출을 감싼 래퍼 |
| `src/network.rs` | AP+STA 실행, Wi-Fi 검색·검증, NVS 설정, 연결 실패 시 AP 유지 |
| `src/web/index.html` | `include_str!`로 펌웨어에 포함되는 조종 화면 |
| `components/rover_hardware/include/rover_hardware.h` | Rust 바인딩 생성에 쓰는 C 함수 선언 |
| `components/rover_hardware/rover_hardware.c` | SDK 헤더를 사용한 카메라·모터·LED 구현 |
| `components/rover_hardware/CMakeLists.txt` | C 소스와 ESP-IDF 의존 컴포넌트 등록 |
| `~/.cargo/registry/` | Cargo가 보통 crates.io에서 받는 Rust 라이브러리 소스의 공용 캐시 |
| `~/.rustup/toolchains/esp/` | `espup`이 설치한 ESP용 Rust 컴파일러와 표준 라이브러리 소스 |
| `.embuild/espressif/` | 첫 빌드에 준비하고 재사용하는 ESP-IDF SDK, C 도구, Python 환경 |
| `target/.../managed_components/` | ESP-IDF 컴포넌트 매니저가 받은 `esp32-camera` 등의 컴포넌트 |
| `target/xtensa-esp32-espidf/release/` | 중간 빌드 파일과 최종 ESP32 ELF, 선택적으로 만든 통합 `.bin` |

`target/`은 펌웨어 파일 하나가 아닌 빌드 폴더입니다. 확장자가 없는 `target/xtensa-esp32-espidf/release/cam-rover-esp32`가 `espflash`에 전달하는 ELF입니다. 통합 `.bin`은 별도로 생성할 수 있는 플래시 이미지입니다. `.embuild/`와 `~/.cargo/registry/`는 로봇에서 실행되지 않으며, 로봇은 플래시에 기록된 펌웨어만 실행합니다.

## 하드웨어 배선

| ESP32-CAM | L298N 입력 |
| --- | --- |
| GPIO14 | IN1 (오른쪽) |
| GPIO15 | IN2 (오른쪽) |
| GPIO13 | IN3 (왼쪽) |
| GPIO12 | IN4 (왼쪽) |
| 5V | 5V |
| GND | GND |

L298N의 ENA/ENB 점퍼는 꽂아 둡니다. microSD 핀과 모터 핀이 겹쳐서 이 펌웨어에서는 microSD를 사용할 수 없습니다.

## macOS 개발 도구 설치

ESP32는 Xtensa 타깃이므로 일반적인 Mac용 Rust 설치만으로는 빌드할 수 없습니다. 새 Mac에서 아래를 한 번 실행합니다. Apple Command Line Tools가 이미 있다면 첫 명령은 생략하세요. Rust 설치 프로그램이 요청하면 새 터미널을 여세요.

```bash
xcode-select --install
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"
cargo +stable install espup --locked
espup install
cargo +stable install ldproxy --locked
cargo +stable install espflash --locked
```

빌드에 사용할 새 터미널을 열 때마다 ESP 환경을 적용합니다.

```bash
source "$HOME/.cargo/env"
source "$HOME/export-esp.sh"
```

`espup`은 ESP용 Rust 툴체인을 설치하고, `ldproxy`는 Rust와 ESP-IDF를 연결하며, `espflash`는 USB로 보드에 기록합니다. 첫 `cargo build`가 지정된 ESP-IDF SDK와 C 도구를 다운로드해 준비합니다. Cargo는 Rust 라이브러리를 `~/.cargo/registry/`에 받고, ESP-IDF 컴포넌트 매니저는 `esp32-camera`와 그 의존성을 빌드 출력 폴더에 받습니다. 이후 빌드는 저장된 파일을 재사용합니다. Arduino IDE를 따로 설치할 필요는 없습니다.

USB 직렬 포트가 보이지 않으면 먼저 케이블과 어댑터를 확인하세요. CH340 기반 어댑터 일부는 macOS 직렬 드라이버가 필요할 수 있습니다.

## 빌드·업로드·실행

아래 명령은 저장소 루트에서 실행합니다. USB 어댑터를 연결할 때 배터리·모터 전원은 끄고, 첫 시험에서는 바퀴를 바닥에서 띄우세요. 예시 포트 이름은 `espflash list-ports`로 나온 실제 값으로 바꿉니다.

```bash
espflash list-ports
cargo build --release
espflash flash --port /dev/cu.usbserial-XXXX \
  target/xtensa-esp32-espidf/release/cam-rover-esp32
```

어댑터가 자동으로 다운로드 모드에 들어가지 못하면 BOOT를 누른 채 RESET을 한 번 누르고 업로드를 다시 시도하세요. 업로드가 끝나면 BOOT에서 손을 떼고 RESET을 누릅니다. 휴대폰을 `cam-rover` Wi-Fi에 기본 암호 `camrover`로 연결한 뒤 `http://192.168.71.1`을 엽니다. 일반적인 펌웨어 업로드는 NVS 설정을 유지하며, 이미 설정한 보드는 로봇 AP를 유지한 채 집 Wi-Fi 재접속을 백그라운드에서 시도합니다.

빌드·업로드·직렬 로그 보기를 한 명령으로 실행하는 방법도 있습니다.

```bash
cargo run --release
```

이 프로젝트에서 `cargo run`은 Mac에서 로봇 프로그램을 실행하지 않습니다. `.cargo/config.toml`의 runner가 `espflash flash --monitor`이므로 **ESP32에 기록하고 보드 로그를 보는 명령**입니다. 직렬 포트가 여러 개라면 `espflash`가 선택을 요청할 수 있습니다. Ctrl-C로 모니터를 종료합니다.

## HTTP 제어와 네트워크 모드

로봇은 매 부팅 때 `http://192.168.71.1`의 WPA2 AP를 켭니다. 저장된 집 Wi-Fi를 선호하면 AP를 유지한 채 백그라운드에서 2.4GHz WPA2-personal 접속을 시도합니다. 성공하면 공유기가 할당한 IP도 사용할 수 있고, 실패하거나 실행 중 연결이 끊겨도 AP가 남습니다. 주변 Wi-Fi 검색은 SSID가 보이는지만 확인합니다. 실제 접속 검증은 인증과 DHCP IP 할당까지 확인하고, 성공한 새 접속 정보만 NVS에 저장합니다.

`POST /api/network`는 HTTP 202를 반환하고 재부팅 없이 백그라운드에서 접속을 검증합니다. `GET /api/network`의 `phase`(`testing`, `connected`, `failed`, `idle`), `last_error`, `sta_configured`, `saved_ssid`, `sta_ip`, `ap_ip`를 조회하세요. `POST /api/wifi/scan`으로 검색을 시작하고 `GET /api/wifi/scan`으로 결과(최대 16개)를 조회합니다. 작업 중 다른 네트워크 명령은 HTTP 409를 반환합니다. 검색·전환 중에는 모터를 정지합니다. AP와 STA가 무선 칩을 공유하므로 채널이 바뀔 때 영상이나 AP 연결이 잠시 끊길 수 있습니다.

```mermaid
flowchart TD
    A["cam-rover AP 항상 유지"] --> B{"명령"}
    B -->|"검색"| C["주변 SSID·신호 세기"]
    C --> B
    B -->|"집 Wi-Fi"| D["입력 또는 저장된 접속 정보"]
    D --> E["STA 인증·DHCP 검증<br/>AP 유지"]
    E -->|"성공"| F["NVS 저장·STA IP 제공<br/>AP IP 유지"]
    E -->|"실패"| G["새 정보 저장 안 함<br/>AP 유지"]
    F -->|"이후 연결 끊김"| G
    B -->|"로봇 AP"| H["STA 연결 종료<br/>AP 유지"]
```

새 접속 시도가 실패하면 이전에 저장된 정보는 유지합니다. 선호 모드가 STA라면 다음 부팅에 다시 시도합니다. 실행 중 STA 연결이 끊겨도 백그라운드 감시가 AP 대체 상태를 표시합니다. 웹페이지는 휴대폰의 Wi-Fi를 자동으로 바꿀 수 없으므로 로봇 AP에서 계속 조종하거나, 휴대폰을 직접 집 Wi-Fi로 전환한 뒤 표시된 STA 주소로 접속하세요. AP 채널 변경 때는 잠시 끊길 수 있습니다.

라즈베리 파이도 같은 네트워크에 연결한 후 아래 명령을 보낼 수 있습니다. `ROVER_IP`를 실제 로봇 주소로 바꾸세요. 이동 명령은 700ms보다 짧은 주기로 반복하고, 버튼을 놓으면 `stop`을 보내세요.

```bash
curl "http://ROVER_IP/api/move?direction=forward-left"
curl "http://ROVER_IP/api/move?direction=stop"
curl "http://ROVER_IP/api/speed?value=170"
curl "http://ROVER_IP/api/light?on=1"
curl "http://ROVER_IP/api/network"
curl -X POST "http://ROVER_IP/api/wifi/scan"
curl "http://ROVER_IP/api/wifi/scan"
curl -X POST "http://ROVER_IP/api/network" -H 'Content-Type: application/json' \
  -d '{"mode":"sta","ssid":"YOUR_2_4_GHZ_SSID","password":"YOUR_PASSWORD"}'
curl -X POST "http://ROVER_IP/api/network" -H 'Content-Type: application/json' \
  -d '{"mode":"sta"}' # 저장된 정보로 재접속
curl -X POST "http://ROVER_IP/api/network" -H 'Content-Type: application/json' \
  -d '{"mode":"ap"}'
```

`GET /api/network`는 실제/선호 모드, AP·STA 주소, 진행 상태, 복구 여부를 반환하지만 암호는 반환하지 않습니다. `{ "mode": "sta" }`는 저장된 접속 정보로 재시도하며 잘못된 입력은 400을 반환합니다. HTTP 제어와 영상에는 애플리케이션 인증·암호화가 없으므로 신뢰할 수 있는 로컬 네트워크에서만 사용하고 80/81 포트를 외부에 개방하지 마세요. 통제되지 않은 환경에서는 기본 로봇 AP 암호를 변경하세요.

## 빌드 시점 설정

로봇 AP 이름·암호와 카메라 상하 반전 설정은 펌웨어에 컴파일되어 들어갑니다. 집 Wi-Fi 정보는 실행 중 설정합니다. AP WPA2 암호는 8~63바이트여야 합니다. 카메라 조립 방향에 맞춰 기본값은 상하 반전입니다.

```bash
ROVER_WIFI_SSID=my-rover \
ROVER_WIFI_PASSWORD=change-me \
ROVER_VIDEO_FLIP=0 \
cargo build --release

espflash flash --port /dev/cu.usbserial-XXXX \
  target/xtensa-esp32-espidf/release/cam-rover-esp32
```

기본 상하 반전을 사용하지 않을 때만 `ROVER_VIDEO_FLIP=0`을 지정하세요. 빌드 시점 값을 바꾼 뒤에는 다시 빌드하고 업로드해야 합니다. Mac 환경변수만 바꿔서는 로봇에 이미 기록된 펌웨어가 바뀌지 않습니다.

## 안전과 문제 해결

- 첫 모터 시험에서는 바퀴를 띄우세요. 안전 태스크가 마지막 이동 명령 700ms 뒤 모터를 정지시킵니다.
- ESP32-CAM은 2.4GHz Wi-Fi를 사용합니다. AP·STA 양쪽에서 제어와 영상은 암호화되지 않은 HTTP이므로 인터넷에 포트 포워딩하지 마세요.
- 주행 중 재부팅되거나 영상이 끊기면 배터리 전압, 공통 GND, L298N 전원 배선을 확인하세요.
- 직렬 포트가 없으면 케이블·어댑터·드라이버·권한을 확인하세요. 업로드 연결에 실패하면 위의 BOOT + RESET 순서를 시도하세요.
- 첫 빌드는 SDK를 다운로드·컴파일하므로 오래 걸릴 수 있습니다. `target/`을 삭제하면 소스가 아니라 빌드 결과를 버리게 됩니다.
