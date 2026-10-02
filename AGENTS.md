# Rover firmware rules

- Keep the ESP32's 700 ms deadman stop independent of the Raspberry Pi. Network loss must stop the motors.
- Preserve the existing AP recovery route, Wi-Fi scan/validation, NVS credential behavior, and HTTP API contract unless the matching hub change is ready.
- All non-stop motor commands must pass the controller lease. Stop must remain callable even when the lease or token is unavailable.
- Never return or log Wi-Fi passwords or the rover API token. Do not commit real credentials or build secrets.
- Bound HTTP request bodies, frame buffers, and blocking operations. Do not hold the motor-state lock while writing an HTTP response.
- Keep hardware pin ownership in `components/rover_hardware` and Rust hardware calls in `src/hardware.rs`.
- Before a PR, run `cargo fmt --check`, the available Rust tests, and an ESP target build. Report hardware-only checks separately.
