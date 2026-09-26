idf_component_register(
    SRCS "rover_hardware.c"
    INCLUDE_DIRS "include"
    REQUIRES esp32-camera driver esp_psram
)
