#include "rover_hardware.h"

#include "driver/gpio.h"
#include "driver/ledc.h"
#include "esp_camera.h"
#include "esp_check.h"
#include "esp_err.h"
#include "esp_log.h"
#include "esp_psram.h"

#define CAMERA_PIN_PWDN 32
#define CAMERA_PIN_RESET -1
#define CAMERA_PIN_XCLK 0
#define CAMERA_PIN_SIOD 26
#define CAMERA_PIN_SIOC 27
#define CAMERA_PIN_D7 35
#define CAMERA_PIN_D6 34
#define CAMERA_PIN_D5 39
#define CAMERA_PIN_D4 36
#define CAMERA_PIN_D3 21
#define CAMERA_PIN_D2 19
#define CAMERA_PIN_D1 18
#define CAMERA_PIN_D0 5
#define CAMERA_PIN_VSYNC 25
#define CAMERA_PIN_HREF 23
#define CAMERA_PIN_PCLK 22

#define MOTOR_RIGHT_1 14
#define MOTOR_RIGHT_2 15
#define MOTOR_LEFT_1 13
#define MOTOR_LEFT_2 12
#define FLASH_PIN 4

static const char *TAG = "rover-hardware";
static camera_fb_t *active_frame = NULL;
static bool camera_ready = false;

static uint32_t magnitude(int16_t value) {
    int32_t wide = value;
    if (wide < 0) {
        wide = -wide;
    }
    return (uint32_t)(wide > 255 ? 255 : wide);
}

static void set_motor_channel(ledc_channel_t channel, uint32_t duty) {
    ESP_ERROR_CHECK(ledc_set_duty(LEDC_HIGH_SPEED_MODE, channel, duty));
    ESP_ERROR_CHECK(ledc_update_duty(LEDC_HIGH_SPEED_MODE, channel));
}

static esp_err_t init_motor_pwm(void) {
    const ledc_timer_config_t timer = {
        .speed_mode = LEDC_HIGH_SPEED_MODE,
        .duty_resolution = LEDC_TIMER_8_BIT,
        .timer_num = LEDC_TIMER_1,
        // Keep PWM above the audible range to avoid motor whine.
        .freq_hz = 20000,
        .clk_cfg = LEDC_AUTO_CLK,
    };
    ESP_RETURN_ON_ERROR(ledc_timer_config(&timer), TAG, "motor PWM timer");

    const int pins[] = {MOTOR_RIGHT_1, MOTOR_RIGHT_2, MOTOR_LEFT_1, MOTOR_LEFT_2};
    const ledc_channel_t channels[] = {
        LEDC_CHANNEL_4, LEDC_CHANNEL_5, LEDC_CHANNEL_6, LEDC_CHANNEL_7
    };

    for (size_t i = 0; i < 4; ++i) {
        const ledc_channel_config_t config = {
            .gpio_num = pins[i],
            .speed_mode = LEDC_HIGH_SPEED_MODE,
            .channel = channels[i],
            .intr_type = LEDC_INTR_DISABLE,
            .timer_sel = LEDC_TIMER_1,
            .duty = 0,
            .hpoint = 0,
            .flags.output_invert = 0,
        };
        ESP_RETURN_ON_ERROR(ledc_channel_config(&config), TAG, "motor PWM channel");
    }

    return ESP_OK;
}

static esp_err_t init_camera(bool vertical_flip) {
    const bool psram = esp_psram_is_initialized();
    const camera_config_t config = {
        .pin_pwdn = CAMERA_PIN_PWDN,
        .pin_reset = CAMERA_PIN_RESET,
        .pin_xclk = CAMERA_PIN_XCLK,
        .pin_sccb_sda = CAMERA_PIN_SIOD,
        .pin_sccb_scl = CAMERA_PIN_SIOC,
        .pin_d7 = CAMERA_PIN_D7,
        .pin_d6 = CAMERA_PIN_D6,
        .pin_d5 = CAMERA_PIN_D5,
        .pin_d4 = CAMERA_PIN_D4,
        .pin_d3 = CAMERA_PIN_D3,
        .pin_d2 = CAMERA_PIN_D2,
        .pin_d1 = CAMERA_PIN_D1,
        .pin_d0 = CAMERA_PIN_D0,
        .pin_vsync = CAMERA_PIN_VSYNC,
        .pin_href = CAMERA_PIN_HREF,
        .pin_pclk = CAMERA_PIN_PCLK,
        .xclk_freq_hz = 20000000,
        .ledc_timer = LEDC_TIMER_0,
        .ledc_channel = LEDC_CHANNEL_0,
        .pixel_format = PIXFORMAT_JPEG,
        // HVGA keeps the control stream stable while motors and Wi-Fi are active.
        // VGA is possible, but its larger JPEG bursts are more sensitive to power/noise.
        .frame_size = psram ? FRAMESIZE_HVGA : FRAMESIZE_QVGA,
        .jpeg_quality = psram ? 12 : 14,
        .fb_count = psram ? 2 : 1,
        .fb_location = psram ? CAMERA_FB_IN_PSRAM : CAMERA_FB_IN_DRAM,
        .grab_mode = psram ? CAMERA_GRAB_LATEST : CAMERA_GRAB_WHEN_EMPTY,
        .sccb_i2c_port = 0,
    };

    ESP_RETURN_ON_ERROR(esp_camera_init(&config), TAG, "camera init");

    sensor_t *sensor = esp_camera_sensor_get();
    if (sensor != NULL) {
        sensor->set_vflip(sensor, vertical_flip ? 1 : 0);
        sensor->set_hmirror(sensor, 0);
    }

    return ESP_OK;
}

int32_t rover_hardware_init(bool vertical_flip) {
    gpio_config_t flash = {
        .pin_bit_mask = 1ULL << FLASH_PIN,
        .mode = GPIO_MODE_OUTPUT,
        .pull_up_en = GPIO_PULLUP_DISABLE,
        .pull_down_en = GPIO_PULLDOWN_DISABLE,
        .intr_type = GPIO_INTR_DISABLE,
    };
    ESP_RETURN_ON_ERROR(gpio_config(&flash), TAG, "flash GPIO");
    gpio_set_level(FLASH_PIN, 0);

    ESP_RETURN_ON_ERROR(init_motor_pwm(), TAG, "motors");
    rover_motors_stop();
    const esp_err_t camera_result = init_camera(vertical_flip);
    camera_ready = camera_result == ESP_OK;
    if (!camera_ready) {
        // Camera faults must not prevent AP recovery or the independent motor stop.
        ESP_LOGW(TAG, "camera unavailable (%s); recovery/control remain available", esp_err_to_name(camera_result));
    }
    return ESP_OK;
}

void rover_motors_drive(int16_t left, int16_t right) {
    const uint32_t left_duty = magnitude(left);
    const uint32_t right_duty = magnitude(right);

    set_motor_channel(LEDC_CHANNEL_4, right < 0 ? right_duty : 0);
    set_motor_channel(LEDC_CHANNEL_5, right > 0 ? right_duty : 0);
    set_motor_channel(LEDC_CHANNEL_6, left > 0 ? left_duty : 0);
    set_motor_channel(LEDC_CHANNEL_7, left < 0 ? left_duty : 0);
}

void rover_motors_stop(void) {
    rover_motors_drive(0, 0);
}

void rover_flash_set(bool on) {
    gpio_set_level(FLASH_PIN, on ? 1 : 0);
}

int32_t rover_camera_capture(const uint8_t **data, size_t *length) {
    if (!camera_ready || data == NULL || length == NULL || active_frame != NULL) {
        return ESP_ERR_INVALID_STATE;
    }

    active_frame = esp_camera_fb_get();
    if (active_frame == NULL) {
        return ESP_FAIL;
    }

    *data = active_frame->buf;
    *length = active_frame->len;
    return ESP_OK;
}

void rover_camera_release(void) {
    if (active_frame != NULL) {
        esp_camera_fb_return(active_frame);
        active_frame = NULL;
    }
}
