set(INTERCOM_MAIN_REQUIRES
    esp_event
    esp_driver_gpio
    esp_driver_i2c
    esp_lcd
    driver
    esp_netif
    esp_timer
    esp_wifi
    esp_websocket_client
    json
    nvs_flash
)

if(CONFIG_INTERCOM_OPUS)
    list(APPEND INTERCOM_MAIN_REQUIRES esp-opus)
endif()

idf_component_register(
    SRCS
        "main.c"
        "intercom_protocol.c"
        "pcm_frame_ring.c"
    INCLUDE_DIRS "."
    REQUIRES ${INTERCOM_MAIN_REQUIRES}
)
