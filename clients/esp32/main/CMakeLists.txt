idf_component_register(
    SRCS
        "main.c"
        "intercom_protocol.c"
        "pcm_frame_ring.c"
    INCLUDE_DIRS "."
    REQUIRES
        esp_event
        esp_driver_gpio
        esp_driver_i2c
        driver
        esp_netif
        esp_timer
        esp_wifi
        esp_websocket_client
        json
        nvs_flash
)
