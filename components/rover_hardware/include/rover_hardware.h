#pragma once

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

int32_t rover_hardware_init(bool vertical_flip);
void rover_motors_drive(int16_t left, int16_t right);
void rover_motors_stop(void);
void rover_flash_set(bool on);

int32_t rover_camera_capture(const uint8_t **data, size_t *length);
void rover_camera_release(void);

#ifdef __cplusplus
}
#endif
