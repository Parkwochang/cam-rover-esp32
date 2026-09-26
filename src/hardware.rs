use anyhow::{bail, Result};
use esp_idf_svc::sys::rover;

pub fn initialize(vertical_flip: bool) -> Result<()> {
    let result = unsafe { rover::rover_hardware_init(vertical_flip) };
    if result != 0 {
        bail!("hardware initialization failed with ESP-IDF error {result}");
    }
    Ok(())
}

pub fn drive(left: i16, right: i16) {
    unsafe { rover::rover_motors_drive(left, right) }
}

pub fn stop() {
    unsafe { rover::rover_motors_stop() }
}

pub fn set_flash(on: bool) {
    unsafe { rover::rover_flash_set(on) }
}

pub struct Frame {
    data: *const u8,
    len: usize,
}

impl Frame {
    pub fn capture() -> Result<Self> {
        let mut data = core::ptr::null();
        let mut len = 0;
        let result = unsafe { rover::rover_camera_capture(&mut data, &mut len) };
        if result != 0 || data.is_null() || len == 0 {
            bail!("camera capture failed with ESP-IDF error {result}");
        }
        Ok(Self { data, len })
    }

    pub fn bytes(&self) -> &[u8] {
        unsafe { core::slice::from_raw_parts(self.data, self.len) }
    }
}

impl Drop for Frame {
    fn drop(&mut self) {
        unsafe { rover::rover_camera_release() }
    }
}

unsafe impl Send for Frame {}
