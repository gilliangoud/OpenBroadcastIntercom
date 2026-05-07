#include <errno.h>
#include <inttypes.h>
#include <math.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "cJSON.h"
#include "driver/gpio.h"
#include "driver/i2c_master.h"
#include "driver/i2s.h"
#include "driver/spi_master.h"
#include "esp_check.h"
#include "esp_event.h"
#include "esp_heap_caps.h"
#include "esp_lcd_panel_io.h"
#include "esp_lcd_panel_ops.h"
#include "esp_lcd_panel_vendor.h"
#include "esp_log.h"
#include "esp_netif.h"
#include "esp_system.h"
#include "esp_task_wdt.h"
#include "esp_timer.h"
#include "esp_wifi.h"
#include "esp_websocket_client.h"
#include "freertos/FreeRTOS.h"
#include "freertos/event_groups.h"
#include "freertos/queue.h"
#include "freertos/semphr.h"
#include "freertos/task.h"
#include "lwip/netdb.h"
#include "nvs.h"
#include "nvs_flash.h"
#include "sdkconfig.h"
#include "soc/soc_caps.h"
#include "sys/socket.h"
#include "sys/time.h"
#include "unistd.h"

#include "intercom_protocol.h"
#include "pcm_frame_ring.h"

#if CONFIG_INTERCOM_OPUS
#include "opus.h"
#endif

#ifndef CONFIG_INTERCOM_CLIENT_UID
#define CONFIG_INTERCOM_CLIENT_UID ""
#endif

#define WIFI_CONNECTED_BIT BIT0
#define MAX_CHANNELS 12
#define MAX_DIRECT_USERS 8
#define MAX_BUTTONS 4
#define MAX_BUTTON_ID 24
#define MAX_BUTTON_LABEL 32
#define MAX_UNIT_NAME 48
#define MAX_ALERT_MESSAGE 72
#define MAX_UI_STATUS 64
#define CONTROL_SEND_TIMEOUT_MS 1000
#define CONTROL_SEND_MUTEX_TIMEOUT_MS 1500
#define CONTROL_RX_MAX_BYTES 16384
#define CONTROL_PING_INTERVAL_US 30000000LL
#define CONTROL_CAPTURE_HEALTH_SEND_INTERVAL_US 5000000LL
#define BUTTON_DEBOUNCE_US 30000
#define CAPTURE_HEALTH_REPORT_US 1000000
#define CAPTURE_CLIP_THRESHOLD 32600
#define CAPTURE_HIGH_PASS_ALPHA 0.995f
#define AUDIO_CUE_AMPLITUDE 1200.0f
#define AUDIO_CUE_PI 3.14159265358979323846f
#define AUDIO_CUE_SWITCH_RAMP_MS 24
#define AUDIO_CUE_RECONNECT_INTERVAL_US 4000000LL
#define AUDIO_CUE_SINE_LUT_SIZE 256
#define PLAYBACK_CODEC_MUTE_GATE_ENABLED 0
#define PLAYBACK_CODEC_ACTIVE_THRESHOLD 8
#define PLAYBACK_CODEC_UNMUTE_PREROLL_FRAMES 2
#define PLAYBACK_CODEC_IDLE_MUTE_FRAMES 3
#define PLAYBACK_STARTUP_SETTLE_MS 80
#define PLAYBACK_I2S_GAP_WARN_US (IC_FRAME_MS * 2500)
#define PLAYBACK_I2S_SLOW_WRITE_WARN_US (IC_FRAME_MS * 8000)
#define PLAYBACK_I2S_WARN_THROTTLE_US 1000000LL
#define ESP32_AUDIO_HW_SAMPLE_RATE IC_PCM48_SAMPLE_RATE
#define ESP32_AUDIO_HW_SAMPLES_PER_FRAME IC_PCM48_SAMPLES_PER_FRAME
#define ESP32_AUDIO_HW_CHANNELS 2
#define ESP32_AUDIO_HW_BITS_PER_SAMPLE 16
#define ESP32_AUDIO_HW_FRAME_BYTES (ESP32_AUDIO_HW_SAMPLES_PER_FRAME * ESP32_AUDIO_HW_CHANNELS * sizeof(int16_t))
#define DISPLAY_WIDTH 240
#define DISPLAY_HEIGHT 240
#define DISPLAY_PIXELS (DISPLAY_WIDTH * DISPLAY_HEIGHT)
#define DISPLAY_REFRESH_MIN_US 250000LL
#define DISPLAY_IDLE_REFRESH_US 2000000LL
#define DISPLAY_TRANSIENT_US 1500000LL

#ifndef CONFIG_INTERCOM_DISPLAY_ST7789
#define CONFIG_INTERCOM_DISPLAY_ST7789 0
#endif

#ifndef CONFIG_INTERCOM_DISPLAY_SPI_HOST
#define CONFIG_INTERCOM_DISPLAY_SPI_HOST 2
#endif

#ifndef CONFIG_INTERCOM_DISPLAY_SPI_MOSI_GPIO
#define CONFIG_INTERCOM_DISPLAY_SPI_MOSI_GPIO -1
#endif

#ifndef CONFIG_INTERCOM_DISPLAY_SPI_SCLK_GPIO
#define CONFIG_INTERCOM_DISPLAY_SPI_SCLK_GPIO -1
#endif

#ifndef CONFIG_INTERCOM_DISPLAY_SPI_CS_GPIO
#define CONFIG_INTERCOM_DISPLAY_SPI_CS_GPIO -1
#endif

#ifndef CONFIG_INTERCOM_DISPLAY_DC_GPIO
#define CONFIG_INTERCOM_DISPLAY_DC_GPIO -1
#endif

#ifndef CONFIG_INTERCOM_DISPLAY_RST_GPIO
#define CONFIG_INTERCOM_DISPLAY_RST_GPIO -1
#endif

#ifndef CONFIG_INTERCOM_DISPLAY_BACKLIGHT_GPIO
#define CONFIG_INTERCOM_DISPLAY_BACKLIGHT_GPIO -1
#endif

#ifndef CONFIG_INTERCOM_DISPLAY_SPI_CLOCK_HZ
#define CONFIG_INTERCOM_DISPLAY_SPI_CLOCK_HZ 40000000
#endif

#ifndef CONFIG_INTERCOM_SIDETONE_CODEC_BYPASS_GAIN_PERCENT
#define CONFIG_INTERCOM_SIDETONE_CODEC_BYPASS_GAIN_PERCENT 25
#endif

#ifndef CONFIG_INTERCOM_SIDETONE_MIC_BYPASS_GAIN_PERCENT
#define CONFIG_INTERCOM_SIDETONE_MIC_BYPASS_GAIN_PERCENT 100
#endif

#ifndef CONFIG_INTERCOM_NOTIFICATION_GAIN_PERCENT
#define CONFIG_INTERCOM_NOTIFICATION_GAIN_PERCENT 50
#endif

#ifndef CONFIG_INTERCOM_PLAYBACK_IDLE_FLOOR_ENABLED
#define CONFIG_INTERCOM_PLAYBACK_IDLE_FLOOR_ENABLED 1
#endif

#ifndef CONFIG_INTERCOM_PLAYBACK_IDLE_FLOOR_AMPLITUDE
#define CONFIG_INTERCOM_PLAYBACK_IDLE_FLOOR_AMPLITUDE 1
#endif

#ifndef CONFIG_INTERCOM_PLAYBACK_TASK_STACK_SIZE
#define CONFIG_INTERCOM_PLAYBACK_TASK_STACK_SIZE 6144
#endif

#ifndef CONFIG_INTERCOM_CAPTURE_TASK_STACK_SIZE
#define CONFIG_INTERCOM_CAPTURE_TASK_STACK_SIZE 6144
#endif

#ifndef CONFIG_INTERCOM_PLAYBACK_PREFILL_FRAMES
#define CONFIG_INTERCOM_PLAYBACK_PREFILL_FRAMES 2
#endif

#ifndef CONFIG_INTERCOM_AUDIO_TX_QUEUE_PACKETS
#define CONFIG_INTERCOM_AUDIO_TX_QUEUE_PACKETS 12
#endif

#ifndef CONFIG_INTERCOM_UDP_TASK_STACK_SIZE
#define CONFIG_INTERCOM_UDP_TASK_STACK_SIZE 8192
#endif

#define ES8388_ADDR 0x10
#define AC101_ADDR 0x1A
#define ES8388_CONTROL1 0x00
#define ES8388_CONTROL2 0x01
#define ES8388_CHIPPOWER 0x02
#define ES8388_ADCPOWER 0x03
#define ES8388_DACPOWER 0x04
#define ES8388_MASTERMODE 0x08
#define ES8388_ADCCONTROL1 0x09
#define ES8388_ADCCONTROL2 0x0A
#define ES8388_ADCCONTROL3 0x0B
#define ES8388_ADCCONTROL4 0x0C
#define ES8388_ADCCONTROL5 0x0D
#define ES8388_ADCCONTROL8 0x10
#define ES8388_ADCCONTROL9 0x11
#define ES8388_ADCCONTROL10 0x12
#define ES8388_ADCCONTROL11 0x13
#define ES8388_ADCCONTROL12 0x14
#define ES8388_ADCCONTROL13 0x15
#define ES8388_ADCCONTROL14 0x16
#define ES8388_DACCONTROL1 0x17
#define ES8388_DACCONTROL2 0x18
#define ES8388_DACCONTROL3 0x19
#define ES8388_DACCONTROL3_DAC_MUTE 0x04
#define ES8388_DACCONTROL4 0x1A
#define ES8388_DACCONTROL5 0x1B
#define ES8388_DACCONTROL16 0x26
#define ES8388_DACCONTROL17 0x27
#define ES8388_DACCONTROL18 0x28
#define ES8388_DACCONTROL19 0x29
#define ES8388_DACCONTROL20 0x2A
#define ES8388_DACCONTROL21 0x2B
#define ES8388_DACCONTROL23 0x2D
#define ES8388_DACCONTROL24 0x2E
#define ES8388_DACCONTROL25 0x2F
#define ES8388_DACCONTROL26 0x30
#define ES8388_DACCONTROL27 0x31
#define ES8388_DACPOWER_DISABLE_OUTPUTS 0xC0
#define ES8388_DACPOWER_ENABLE_ALL_OUTPUTS 0x3C
#define ES8388_DACPOWER_ENABLE_OUT1 0x0C
#define ES8388_DACPOWER_ENABLE_OUT2 0x30
#define ES8388_ADCPOWER_POWER_DOWN 0xFF
#define ES8388_ADCPOWER_ENABLE_ADC 0x09

#define ES8388_ADC_INPUT_LINE1 0x00
#define ES8388_ADC_INPUT_MIC1 0x05
#define ES8388_ADC_INPUT_MIC2 0x06
#define ES8388_ADC_INPUT_LINE2 0x50
#define ES8388_ADC_INPUT_DIFFERENCE 0xF0
#define ES8388_MIXER_DAC_0DB 0x90
#define ES8388_MIXER_DAC_AND_LINE_ENABLE 0x40
#define ES8388_DAC_LRCK_SHARED 0x80
#define ES8388_DAC_LRCK_SHARED_WITH_BYPASS 0xC0
#define ES8388_MIXSEL_LINE1 0x00
#define ES8388_MIXSEL_LINE2 0x09
#define ES8388_MIXSEL_ADC_P 0x1B
#define ES8388_MIXSEL_ADC_N 0x24
#define ES8388_OUTPUT_VOLUME_0DB 0x1E
#define ES8388_OUTPUT_VOLUME_PLUS_1_5DB 0x1F
#define ES8388_OUTPUT_VOLUME_PLUS_3DB 0x20
#define ES8388_OUTPUT_VOLUME_PLUS_4_5DB 0x21
#define ES8388_OUTPUT_VOLUME_MUTE 0x00

static const char *TAG = "intercom_esp32";

typedef enum {
    TALK_MODE_MUTED,
    TALK_MODE_PTT,
    TALK_MODE_OPEN,
} talk_mode_t;

typedef struct {
    uint16_t values[MAX_CHANNELS];
    size_t count;
} channel_list_t;

typedef struct {
    uint16_t values[MAX_DIRECT_USERS];
    size_t count;
} user_list_t;

typedef struct {
    char id[MAX_BUTTON_ID];
    char label[MAX_BUTTON_LABEL];
    bool latching;
    bool active;
    bool duck;
    channel_list_t tx_channels;
    user_list_t tx_users;
} button_route_t;

typedef struct {
    channel_list_t listen;
    channel_list_t tx;
    ic_codec_t codec;
    talk_mode_t talk_mode;
    bool regular_talk_active;
    button_route_t buttons[MAX_BUTTONS];
    size_t button_count;
} runtime_config_t;

typedef struct {
    ic_target_kind_t kind;
    uint16_t id;
} tx_target_t;

typedef struct {
    int gpio;
    const char *id;
    const char *label;
    bool stable_pressed;
    bool last_raw_pressed;
    int64_t last_change_us;
} physical_button_t;

typedef struct {
    char id[MAX_BUTTON_ID];
    char label[MAX_BUTTON_LABEL];
    bool enabled;
    bool configured;
    bool active;
} ui_button_state_t;

typedef struct {
    uint64_t id;
    uint16_t sender;
    char message[MAX_ALERT_MESSAGE];
    uint64_t created_at_ms;
    bool present;
} ui_alert_state_t;

typedef struct {
    bool wifi_connected;
    bool control_connected;
    bool config_received;
    char blocking_status[MAX_UI_STATUS];
    uint16_t user_id;
    char unit_name[MAX_UNIT_NAME];
    ui_button_state_t buttons[MAX_BUTTONS];
    ui_alert_state_t active_alert;
    bool has_last_direct_caller;
    uint16_t last_direct_caller;
    uint16_t active_direct_call_count;
    bool reply_held;
    uint16_t reply_target;
    char transient_status[MAX_UI_STATUS];
    int64_t transient_until_us;
} ui_state_t;

typedef struct {
    double sum;
    double sum_squares;
    int peak_abs;
} capture_channel_accumulator_t;

typedef struct {
    capture_channel_accumulator_t left;
    capture_channel_accumulator_t right;
    capture_channel_accumulator_t selected;
    uint32_t samples;
    uint32_t raw_clipped_samples;
    uint32_t software_clipped_samples;
    uint16_t tx_target_count;
    uint32_t tx_packets_sent;
    uint32_t tx_send_failures;
    int64_t started_us;
} capture_health_accumulator_t;

typedef struct {
    float left_rms;
    float left_peak;
    float left_dc_offset;
    float right_rms;
    float right_peak;
    float right_dc_offset;
    float selected_rms;
    float selected_peak;
    float selected_dc_offset;
    uint32_t raw_clipped_samples;
    uint32_t software_clipped_samples;
    uint16_t tx_target_count;
    uint32_t tx_packets_sent;
    uint32_t tx_send_failures;
    bool ready;
} capture_health_report_t;

typedef enum {
    AUDIO_CUE_NONE,
    AUDIO_CUE_CONNECTED,
    AUDIO_CUE_DISCONNECTED,
    AUDIO_CUE_RECONNECTING,
} audio_cue_kind_t;

typedef struct {
    audio_cue_kind_t kind;
    audio_cue_kind_t pending_kind;
    uint32_t sample_index;
    uint32_t sample_rate;
    uint32_t pending_sample_rate;
    uint32_t release_index;
    uint32_t release_samples;
    uint16_t gain_percent;
    uint16_t pending_gain_percent;
} audio_cue_state_t;

typedef struct {
    uint16_t len;
    uint8_t bytes[IC_MAX_PACKET_BYTES];
} audio_tx_packet_t;

typedef struct {
    uint32_t active_preroll_frames;
    uint32_t idle_frames;
} playback_codec_mute_gate_t;

typedef enum {
    AUDIO_DIAGNOSTIC_NORMAL,
    AUDIO_DIAGNOSTIC_OUTPUT_TEST,
    AUDIO_DIAGNOSTIC_CAPTURE_TEST,
    AUDIO_DIAGNOSTIC_LOCAL_LOOPBACK,
} audio_diagnostic_mode_t;

typedef struct {
    bool msb_format;
    bool data_width_32;
    bool slot_width_32;
    bool mclk_enabled;
    bool pa_active_high;
    bool swapped_ws_dout_pins;
} i2s_runtime_options_t;

typedef enum {
    ESP32_ADC_DIFFERENCE,
    ESP32_ADC_MIC1,
    ESP32_ADC_MIC2,
    ESP32_ADC_LINE1,
    ESP32_ADC_LINE2,
} esp32_adc_input_t;

typedef enum {
    ESP32_CAPTURE_LEFT,
    ESP32_CAPTURE_RIGHT,
    ESP32_CAPTURE_AVERAGE,
} esp32_capture_channel_t;

typedef enum {
    ESP32_SIDETONE_OFF,
    ESP32_SIDETONE_FIRMWARE,
    ESP32_SIDETONE_CODEC_BYPASS,
} esp32_sidetone_mode_t;

typedef enum {
    ESP32_OUTPUT_ROUTE_BOTH,
    ESP32_OUTPUT_ROUTE_OUT1,
    ESP32_OUTPUT_ROUTE_OUT2,
} esp32_output_route_t;

typedef struct {
    bool server_control_enabled;
    esp32_adc_input_t adc_input;
    uint8_t mic_pga_gain_db;
    esp32_capture_channel_t capture_channel;
    bool high_pass_enabled;
    uint16_t mic_software_gain_percent;
    uint16_t speaker_software_gain_percent;
    uint16_t notification_gain_percent;
    bool alc_enabled;
    bool noise_gate_enabled;
    esp32_sidetone_mode_t sidetone_mode;
    uint16_t sidetone_firmware_gain_percent;
    uint16_t sidetone_codec_bypass_gain_percent;
    uint16_t sidetone_mic_bypass_gain_percent;
} esp32_audio_config_t;

static EventGroupHandle_t s_wifi_events;
static SemaphoreHandle_t s_config_lock;
static QueueHandle_t s_codec_mute_queue;
static QueueHandle_t s_audio_tx_queue;
static QueueHandle_t s_audio_config_queue;
static SemaphoreHandle_t s_ws_send_lock;
static runtime_config_t s_config;
static ic_pcm_frame_ring_t s_playback_ring;
static ic_pcm_frame_ring_t s_sidetone_ring;
static portMUX_TYPE s_capture_health_lock = portMUX_INITIALIZER_UNLOCKED;
static portMUX_TYPE s_audio_config_lock = portMUX_INITIALIZER_UNLOCKED;
static portMUX_TYPE s_audio_cue_lock = portMUX_INITIALIZER_UNLOCKED;
static portMUX_TYPE s_codec_mute_lock = portMUX_INITIALIZER_UNLOCKED;
static portMUX_TYPE s_control_state_lock = portMUX_INITIALIZER_UNLOCKED;
static portMUX_TYPE s_ui_lock = portMUX_INITIALIZER_UNLOCKED;
static capture_health_report_t s_capture_health_report;
static audio_cue_state_t s_audio_cue;
static esp32_audio_config_t s_audio_config;
static float s_high_pass_previous_input;
static float s_high_pass_previous_output;
static int16_t s_playback_mono_frame[IC_MAX_SAMPLES_PER_FRAME];
static int16_t s_playback_sidetone_frame[IC_MAX_SAMPLES_PER_FRAME];
static int16_t s_playback_stereo_frame[IC_MAX_SAMPLES_PER_FRAME * 2];
static int16_t s_capture_stereo_frame[IC_MAX_SAMPLES_PER_FRAME * 2];
static int16_t s_capture_mono_frame[IC_MAX_SAMPLES_PER_FRAME];
static int16_t s_capture_packet_frame[IC_MAX_SAMPLES_PER_FRAME];
static uint8_t s_capture_payload[IC_MAX_BYTES_PER_FRAME];
static tx_target_t s_capture_targets[MAX_CHANNELS + MAX_DIRECT_USERS];
static int16_t s_capture_resample_history[6];
static bool s_capture_resample_history_ready = false;
static uint8_t s_udp_rx_packet[IC_MAX_PACKET_BYTES];
static int16_t s_udp_playback_frame[IC_MAX_SAMPLES_PER_FRAME];
static int16_t s_udp_packet_frame[IC_MAX_SAMPLES_PER_FRAME];
static audio_tx_packet_t s_audio_tx_encode_packet;
static audio_tx_packet_t s_audio_tx_send_packet;
static char s_control_rx_buffer[CONTROL_RX_MAX_BYTES + 1];
static size_t s_control_rx_expected_len;
static size_t s_control_rx_received_len;
static esp_websocket_client_handle_t s_ws_client;
static int s_audio_sock = -1;
static uint16_t s_audio_seq;
static uint16_t s_registration_seq;
static uint16_t s_runtime_user_id;
static char s_client_uid[37];
static bool s_codec_output_mute_requested;
static bool s_control_connected;
static bool s_control_hello_pending;
static bool s_control_startup_config_pending;
static volatile uint32_t s_wifi_connect_count;
static volatile uint32_t s_wifi_disconnect_count;
static volatile uint32_t s_control_connect_count;
static volatile uint32_t s_control_disconnect_count;
static volatile uint32_t s_udp_rx_packets;
static volatile uint32_t s_udp_decode_errors;
static volatile uint32_t s_udp_codec_drops;
static volatile uint32_t s_udp_sequence_gaps;
static volatile uint32_t s_udp_payload_decode_errors;
static volatile uint32_t s_udp_tx_send_failures;
static volatile uint32_t s_audio_tx_queue_drops;
#if CONFIG_INTERCOM_OPUS
static volatile uint32_t s_opus_encode_failures;
static volatile uint32_t s_opus_decode_failures;
#endif
static bool s_udp_have_last_seq;
static uint16_t s_udp_last_seq;
static TaskHandle_t s_udp_task_handle;
static TaskHandle_t s_registration_task_handle;
static TaskHandle_t s_playback_task_handle;
static TaskHandle_t s_capture_task_handle;
static TaskHandle_t s_button_task_handle;
#if CONFIG_INTERCOM_DISPLAY_ST7789
static TaskHandle_t s_display_task_handle;
static bool s_display_initialized;
static bool s_display_framebuffer_in_psram;
static size_t s_display_framebuffer_bytes;
#endif
#if CONFIG_INTERCOM_OPUS
static OpusEncoder *s_opus_encoder;
static OpusDecoder *s_opus_decoder;
#endif
static int16_t s_audio_cue_sine_lut[AUDIO_CUE_SINE_LUT_SIZE];
static bool s_audio_cue_sine_lut_ready;
static i2c_master_bus_handle_t s_i2c_bus;
static i2c_master_dev_handle_t s_i2c_codec_dev;
static i2s_runtime_options_t s_i2s_options;
static bool s_audio_hw_ready;
static int64_t s_audio_hw_last_write_start_us;
static int64_t s_audio_hw_last_write_warn_us;
static uint32_t s_audio_hw_write_gap_warnings;
static uint32_t s_audio_hw_write_slow_warnings;
static uint32_t s_audio_hw_write_short_warnings;
static ui_state_t s_ui_state;
static volatile uint32_t s_ui_state_version;
static uint16_t s_reply_active_target;

#if CONFIG_INTERCOM_DISPLAY_ST7789
static esp_lcd_panel_handle_t s_display_panel;
static uint16_t *s_display_framebuffer;
#endif

static physical_button_t s_dedicated_buttons[] = {
    {CONFIG_INTERCOM_BUTTON1_GPIO, CONFIG_INTERCOM_BUTTON1_ID, CONFIG_INTERCOM_BUTTON1_LABEL, false, false, 0},
    {CONFIG_INTERCOM_BUTTON2_GPIO, CONFIG_INTERCOM_BUTTON2_ID, CONFIG_INTERCOM_BUTTON2_LABEL, false, false, 0},
    {CONFIG_INTERCOM_BUTTON3_GPIO, CONFIG_INTERCOM_BUTTON3_ID, CONFIG_INTERCOM_BUTTON3_LABEL, false, false, 0},
    {CONFIG_INTERCOM_BUTTON4_GPIO, CONFIG_INTERCOM_BUTTON4_ID, CONFIG_INTERCOM_BUTTON4_LABEL, false, false, 0},
};
static physical_button_t s_reply_button = {
    CONFIG_INTERCOM_REPLY_BUTTON_GPIO,
    "reply",
    "Reply",
    false,
    false,
    0,
};

static cJSON *codec_config_json(void);
static esp_err_t audio_hw_init(void);
static esp_err_t audio_hw_probe_codec(void);
static esp_err_t audio_hw_apply_audio_config(const esp32_audio_config_t *config);
static esp_err_t audio_hw_apply_output_registers(const char *label,
                                                 uint8_t dac_power,
                                                 uint8_t out1_volume,
                                                 uint8_t out2_volume,
                                                 bool log_readback);
static esp_err_t audio_hw_apply_output_route(esp32_output_route_t route, bool log_readback);
static esp_err_t audio_hw_set_pa_gpio_level(bool high);
static esp_err_t audio_hw_write_es8388_reg(int reg, int value, const char *name);
static esp_err_t audio_hw_read_es8388_reg_value(int reg, int *value, const char *name);
static esp_err_t audio_hw_update_es8388_reg_bits(int reg, int clear_mask, int set_bits, const char *name);
static esp_err_t audio_hw_write(const int16_t *stereo_frame, size_t bytes);
static esp_err_t audio_hw_read(int16_t *stereo_frame, size_t bytes);
static esp_err_t es8388_apply_audio_config(const esp32_audio_config_t *config);
static void request_audio_config_apply(const esp32_audio_config_t *config);
static esp_err_t es8388_set_playback_mute(bool mute);
static esp_err_t audio_i2s_set_codec(ic_codec_t codec);
static void es8388_dump_registers(const char *reason, const esp32_audio_config_t *config, bool warn_on_mismatch);
static ic_codec_t default_codec(void);
static bool codec_supported(ic_codec_t codec);
static uint32_t samples_for_ms_u32(uint32_t sample_rate, uint32_t ms);
static float smoothstep_unit(float x);

static audio_diagnostic_mode_t audio_diagnostic_mode(void)
{
#if CONFIG_INTERCOM_AUDIO_DIAGNOSTIC_OUTPUT_TEST
    return AUDIO_DIAGNOSTIC_OUTPUT_TEST;
#elif CONFIG_INTERCOM_AUDIO_DIAGNOSTIC_CAPTURE_TEST
    return AUDIO_DIAGNOSTIC_CAPTURE_TEST;
#elif CONFIG_INTERCOM_AUDIO_DIAGNOSTIC_LOCAL_LOOPBACK
    return AUDIO_DIAGNOSTIC_LOCAL_LOOPBACK;
#else
    return AUDIO_DIAGNOSTIC_NORMAL;
#endif
}

static const char *audio_diagnostic_mode_name(audio_diagnostic_mode_t mode)
{
    switch (mode) {
    case AUDIO_DIAGNOSTIC_OUTPUT_TEST:
        return "output-test";
    case AUDIO_DIAGNOSTIC_CAPTURE_TEST:
        return "capture-test";
    case AUDIO_DIAGNOSTIC_LOCAL_LOOPBACK:
        return "local-loopback";
    case AUDIO_DIAGNOSTIC_NORMAL:
    default:
        return "normal";
    }
}

static i2s_runtime_options_t default_i2s_options(void)
{
    return (i2s_runtime_options_t){
        .msb_format =
#if CONFIG_INTERCOM_I2S_FORMAT_MSB
            true,
#else
            false,
#endif
        .data_width_32 = false,
        .slot_width_32 =
#if CONFIG_INTERCOM_I2S_SLOT_WIDTH_32
            true,
#else
            false,
#endif
        .mclk_enabled = CONFIG_INTERCOM_I2S_MCLK_GPIO >= 0,
        .pa_active_high =
#if CONFIG_INTERCOM_PA_ACTIVE_LOW
            false,
#else
            true,
#endif
        .swapped_ws_dout_pins =
#if CONFIG_INTERCOM_I2S_SWAP_WS_DOUT_PINS
            true,
#else
            false,
#endif
    };
}

static const char *i2s_format_name_for(const i2s_runtime_options_t *options)
{
    return options && options->msb_format ? "msb" : "philips";
}

static const char *i2s_slot_width_name_for(const i2s_runtime_options_t *options)
{
    return options && options->slot_width_32 ? "32" : "16";
}

static const char *i2s_data_width_name_for(const i2s_runtime_options_t *options)
{
    return options && options->data_width_32 ? "32" : "16";
}

static const char *i2s_pin_profile_name_for(const i2s_runtime_options_t *options)
{
    return options && options->swapped_ws_dout_pins ? "ws26-dout25" : "configured";
}

static const char *i2s_format_name(void)
{
    return i2s_format_name_for(&s_i2s_options);
}

static const char *i2s_slot_width_name(void)
{
    return i2s_slot_width_name_for(&s_i2s_options);
}

static const char *i2s_data_width_name(void)
{
    return i2s_data_width_name_for(&s_i2s_options);
}

static const char *i2s_pin_profile_name(void)
{
    return i2s_pin_profile_name_for(&s_i2s_options);
}

static i2s_comm_format_t i2s_legacy_comm_format_for(const i2s_runtime_options_t *options)
{
    return options && options->msb_format ? I2S_COMM_FORMAT_STAND_MSB : I2S_COMM_FORMAT_STAND_I2S;
}

static const char *output_route_name(esp32_output_route_t route)
{
    switch (route) {
    case ESP32_OUTPUT_ROUTE_OUT1:
        return "out1";
    case ESP32_OUTPUT_ROUTE_OUT2:
        return "out2";
    case ESP32_OUTPUT_ROUTE_BOTH:
    default:
        return "out1+out2";
    }
}

static int16_t clamp_i16(int value)
{
    if (value > INT16_MAX) {
        return INT16_MAX;
    }
    if (value < INT16_MIN) {
        return INT16_MIN;
    }
    return (int16_t)value;
}

static int abs_i16_value(int16_t value)
{
    return value == INT16_MIN ? 32768 : abs((int)value);
}

static esp32_adc_input_t default_adc_input(void)
{
#if CONFIG_INTERCOM_ES8388_ADC_INPUT_MIC1
    return ESP32_ADC_MIC1;
#elif CONFIG_INTERCOM_ES8388_ADC_INPUT_MIC2
    return ESP32_ADC_MIC2;
#elif CONFIG_INTERCOM_ES8388_ADC_INPUT_LINE1
    return ESP32_ADC_LINE1;
#elif CONFIG_INTERCOM_ES8388_ADC_INPUT_LINE2
    return ESP32_ADC_LINE2;
#else
    return ESP32_ADC_DIFFERENCE;
#endif
}

static esp32_capture_channel_t default_capture_channel(void)
{
#if CONFIG_INTERCOM_CAPTURE_CHANNEL_RIGHT
    return ESP32_CAPTURE_RIGHT;
#elif CONFIG_INTERCOM_CAPTURE_CHANNEL_AVERAGE
    return ESP32_CAPTURE_AVERAGE;
#else
    return ESP32_CAPTURE_LEFT;
#endif
}

static uint8_t default_mic_pga_gain_db(void)
{
#if CONFIG_INTERCOM_ES8388_MIC_PGA_GAIN_0DB
    return 0;
#elif CONFIG_INTERCOM_ES8388_MIC_PGA_GAIN_3DB
    return 3;
#elif CONFIG_INTERCOM_ES8388_MIC_PGA_GAIN_6DB
    return 6;
#elif CONFIG_INTERCOM_ES8388_MIC_PGA_GAIN_12DB
    return 12;
#elif CONFIG_INTERCOM_ES8388_MIC_PGA_GAIN_15DB
    return 15;
#elif CONFIG_INTERCOM_ES8388_MIC_PGA_GAIN_18DB
    return 18;
#elif CONFIG_INTERCOM_ES8388_MIC_PGA_GAIN_21DB
    return 21;
#elif CONFIG_INTERCOM_ES8388_MIC_PGA_GAIN_24DB
    return 24;
#else
    return 9;
#endif
}

static esp32_sidetone_mode_t default_sidetone_mode(void)
{
#if CONFIG_INTERCOM_SIDETONE_FIRMWARE
    return ESP32_SIDETONE_FIRMWARE;
#else
    return ESP32_SIDETONE_OFF;
#endif
}

static esp32_audio_config_t default_audio_config(void)
{
    return (esp32_audio_config_t){
        .server_control_enabled = false,
        .adc_input = default_adc_input(),
        .mic_pga_gain_db = default_mic_pga_gain_db(),
        .capture_channel = default_capture_channel(),
#if CONFIG_INTERCOM_CAPTURE_HIGH_PASS
        .high_pass_enabled = true,
#else
        .high_pass_enabled = false,
#endif
#if CONFIG_INTERCOM_ES8388_ALC
        .alc_enabled = true,
#else
        .alc_enabled = false,
#endif
#if CONFIG_INTERCOM_ES8388_NOISE_GATE
        .noise_gate_enabled = true,
#else
        .noise_gate_enabled = false,
#endif
        .mic_software_gain_percent = CONFIG_INTERCOM_MIC_GAIN_PERCENT,
        .speaker_software_gain_percent = CONFIG_INTERCOM_SPEAKER_GAIN_PERCENT,
        .notification_gain_percent = CONFIG_INTERCOM_NOTIFICATION_GAIN_PERCENT,
        .sidetone_mode = default_sidetone_mode(),
        .sidetone_codec_bypass_gain_percent = CONFIG_INTERCOM_SIDETONE_CODEC_BYPASS_GAIN_PERCENT,
        .sidetone_mic_bypass_gain_percent = CONFIG_INTERCOM_SIDETONE_MIC_BYPASS_GAIN_PERCENT,
#if CONFIG_INTERCOM_SIDETONE_FIRMWARE
        .sidetone_firmware_gain_percent = CONFIG_INTERCOM_SIDETONE_GAIN_PERCENT,
#else
        .sidetone_firmware_gain_percent = 0,
#endif
    };
}

static void audio_config_init(void)
{
    taskENTER_CRITICAL(&s_audio_config_lock);
    s_audio_config = default_audio_config();
    taskEXIT_CRITICAL(&s_audio_config_lock);
}

static esp32_audio_config_t audio_config_snapshot(void)
{
    esp32_audio_config_t config;
    taskENTER_CRITICAL(&s_audio_config_lock);
    config = s_audio_config;
    taskEXIT_CRITICAL(&s_audio_config_lock);
    return config;
}

static void audio_config_store(const esp32_audio_config_t *config)
{
    taskENTER_CRITICAL(&s_audio_config_lock);
    s_audio_config = *config;
    s_high_pass_previous_input = 0.0f;
    s_high_pass_previous_output = 0.0f;
    taskEXIT_CRITICAL(&s_audio_config_lock);
}

static bool audio_config_equal(const esp32_audio_config_t *a, const esp32_audio_config_t *b)
{
    return a && b && a->server_control_enabled == b->server_control_enabled && a->adc_input == b->adc_input &&
           a->mic_pga_gain_db == b->mic_pga_gain_db && a->capture_channel == b->capture_channel &&
           a->high_pass_enabled == b->high_pass_enabled &&
           a->mic_software_gain_percent == b->mic_software_gain_percent &&
           a->speaker_software_gain_percent == b->speaker_software_gain_percent &&
           a->notification_gain_percent == b->notification_gain_percent && a->alc_enabled == b->alc_enabled &&
           a->noise_gate_enabled == b->noise_gate_enabled && a->sidetone_mode == b->sidetone_mode &&
           a->sidetone_firmware_gain_percent == b->sidetone_firmware_gain_percent &&
           a->sidetone_codec_bypass_gain_percent == b->sidetone_codec_bypass_gain_percent &&
           a->sidetone_mic_bypass_gain_percent == b->sidetone_mic_bypass_gain_percent;
}

static ic_codec_t runtime_codec_snapshot(void)
{
    ic_codec_t codec = default_codec();
    if (s_config_lock) {
        xSemaphoreTake(s_config_lock, portMAX_DELAY);
        codec = s_config.codec;
        xSemaphoreGive(s_config_lock);
    }
    return codec_supported(codec) ? codec : IC_CODEC_PCM16;
}

static const char *adc_input_name(esp32_adc_input_t input)
{
    switch (input) {
    case ESP32_ADC_MIC1:
        return "mic1";
    case ESP32_ADC_MIC2:
        return "mic2";
    case ESP32_ADC_LINE1:
        return "line1";
    case ESP32_ADC_LINE2:
        return "line2";
    case ESP32_ADC_DIFFERENCE:
    default:
        return "difference";
    }
}

static uint8_t es8388_adc_input_value(esp32_adc_input_t input)
{
    switch (input) {
    case ESP32_ADC_MIC1:
        return ES8388_ADC_INPUT_MIC1;
    case ESP32_ADC_MIC2:
        return ES8388_ADC_INPUT_MIC2;
    case ESP32_ADC_LINE1:
        return ES8388_ADC_INPUT_LINE1;
    case ESP32_ADC_LINE2:
        return ES8388_ADC_INPUT_LINE2;
    case ESP32_ADC_DIFFERENCE:
    default:
        return ES8388_ADC_INPUT_DIFFERENCE;
    }
}

static const char *capture_channel_name(esp32_capture_channel_t channel)
{
    switch (channel) {
    case ESP32_CAPTURE_RIGHT:
        return "right";
    case ESP32_CAPTURE_AVERAGE:
        return "average";
    case ESP32_CAPTURE_LEFT:
    default:
        return "left";
    }
}

static const char *sidetone_mode_name(esp32_sidetone_mode_t mode)
{
    switch (mode) {
    case ESP32_SIDETONE_FIRMWARE:
        return "firmware";
    case ESP32_SIDETONE_CODEC_BYPASS:
        return "codec_bypass";
    case ESP32_SIDETONE_OFF:
    default:
        return "off";
    }
}

static int capture_select_sample(const esp32_audio_config_t *config, int16_t left, int16_t right)
{
    switch (config->capture_channel) {
    case ESP32_CAPTURE_RIGHT:
        return right;
    case ESP32_CAPTURE_AVERAGE:
        return ((int)left + (int)right) / 2;
    case ESP32_CAPTURE_LEFT:
    default:
        return left;
    }
}

static int capture_high_pass_sample(const esp32_audio_config_t *config, int sample)
{
    if (!config->high_pass_enabled) {
        return sample;
    }
    float input = (float)sample;
    float output = input - s_high_pass_previous_input + CAPTURE_HIGH_PASS_ALPHA * s_high_pass_previous_output;
    s_high_pass_previous_input = input;
    s_high_pass_previous_output = output;
    return (int)lrintf(output);
}

static int16_t read_le_i16(const uint8_t *bytes)
{
    return (int16_t)((uint16_t)bytes[0] | ((uint16_t)bytes[1] << 8));
}

static int task_core(int configured)
{
    return configured < 0 ? tskNO_AFFINITY : configured;
}

static void channel_list_add(channel_list_t *list, uint16_t value)
{
    if (!list || value == 0) {
        return;
    }
    for (size_t i = 0; i < list->count; i++) {
        if (list->values[i] == value) {
            return;
        }
    }
    if (list->count < MAX_CHANNELS) {
        list->values[list->count++] = value;
    }
}

static void user_list_add(user_list_t *list, uint16_t value)
{
    if (!list || value == 0) {
        return;
    }
    for (size_t i = 0; i < list->count; i++) {
        if (list->values[i] == value) {
            return;
        }
    }
    if (list->count < MAX_DIRECT_USERS) {
        list->values[list->count++] = value;
    }
}

static void parse_channel_csv(const char *csv, channel_list_t *out)
{
    memset(out, 0, sizeof(*out));
    if (!csv || !*csv) {
        return;
    }

    char buf[96];
    strlcpy(buf, csv, sizeof(buf));
    char *save = NULL;
    for (char *part = strtok_r(buf, ",", &save); part; part = strtok_r(NULL, ",", &save)) {
        long value = strtol(part, NULL, 10);
        if (value > 0 && value <= UINT16_MAX) {
            channel_list_add(out, (uint16_t)value);
        }
    }
}

static bool string_eq(const char *a, const char *b)
{
    return a && b && strcmp(a, b) == 0;
}

static talk_mode_t parse_talk_mode(const char *mode)
{
    if (string_eq(mode, "muted")) {
        return TALK_MODE_MUTED;
    }
    if (string_eq(mode, "open")) {
        return TALK_MODE_OPEN;
    }
    return TALK_MODE_PTT;
}

static const char *talk_mode_wire(talk_mode_t mode)
{
    switch (mode) {
    case TALK_MODE_MUTED:
        return "muted";
    case TALK_MODE_OPEN:
        return "open";
    case TALK_MODE_PTT:
    default:
        return "ptt";
    }
}

static ic_codec_t default_codec(void)
{
#if CONFIG_INTERCOM_INITIAL_CODEC_OPUS
    return IC_CODEC_OPUS;
#elif CONFIG_INTERCOM_INITIAL_CODEC_PCM48
    return IC_CODEC_PCM48;
#elif CONFIG_INTERCOM_INITIAL_CODEC_PCM24
    return IC_CODEC_PCM24;
#else
    return IC_CODEC_PCM16;
#endif
}

static bool codec_supported(ic_codec_t codec)
{
    return codec == IC_CODEC_PCM16 || codec == IC_CODEC_PCM24 || codec == IC_CODEC_PCM48
#if CONFIG_INTERCOM_OPUS
           || codec == IC_CODEC_OPUS
#endif
        ;
}

static gpio_num_t audio_i2s_ws_gpio_for(const i2s_runtime_options_t *options)
{
    return (gpio_num_t)(options && options->swapped_ws_dout_pins ? CONFIG_INTERCOM_I2S_DOUT_GPIO
                                                                 : CONFIG_INTERCOM_I2S_WS_GPIO);
}

static gpio_num_t audio_i2s_dout_gpio_for(const i2s_runtime_options_t *options)
{
    return (gpio_num_t)(options && options->swapped_ws_dout_pins ? CONFIG_INTERCOM_I2S_WS_GPIO
                                                                 : CONFIG_INTERCOM_I2S_DOUT_GPIO);
}

static i2s_bits_per_chan_t audio_i2s_bits_per_chan_for(const i2s_runtime_options_t *options)
{
    return options && (options->slot_width_32 || options->data_width_32) ? I2S_BITS_PER_CHAN_32BIT
                                                                         : I2S_BITS_PER_CHAN_16BIT;
}

static void audio_i2s_log_channel_info(const char *label)
{
    ESP_LOGI(TAG,
             "legacy I2S %s: sample_rate=%u Hz mclk=%u Hz bclk=%u Hz format=%s data_width=%s slot_width=%s pins=%s",
             label,
             ESP32_AUDIO_HW_SAMPLE_RATE,
             s_i2s_options.mclk_enabled ? ESP32_AUDIO_HW_SAMPLE_RATE * 256U : 0U,
             ESP32_AUDIO_HW_SAMPLE_RATE * ESP32_AUDIO_HW_CHANNELS *
                 (s_i2s_options.slot_width_32 ? 32U : 16U),
             i2s_format_name(),
             i2s_data_width_name(),
             i2s_slot_width_name(),
             i2s_pin_profile_name());
}

static const char *codec_wire(ic_codec_t codec)
{
    switch (codec) {
    case IC_CODEC_OPUS:
        return "opus";
    case IC_CODEC_PCM24:
        return "pcm24";
    case IC_CODEC_PCM48:
        return "pcm48";
    case IC_CODEC_PCM16:
    default:
        return "pcm16";
    }
}

static bool parse_codec_value(const char *value, ic_codec_t *out)
{
    if (string_eq(value, "pcm16") || string_eq(value, "pcm")) {
        *out = IC_CODEC_PCM16;
        return true;
    }
    if (string_eq(value, "pcm24") || string_eq(value, "pcm-24")) {
        *out = IC_CODEC_PCM24;
        return true;
    }
    if (string_eq(value, "pcm48") || string_eq(value, "pcm-48")) {
        *out = IC_CODEC_PCM48;
        return true;
    }
    if (string_eq(value, "opus")) {
        *out = IC_CODEC_OPUS;
        return true;
    }
    return false;
}

static size_t codec_samples_per_frame(ic_codec_t codec)
{
    switch (codec) {
    case IC_CODEC_OPUS:
        return IC_OPUS_SAMPLES_PER_FRAME;
    case IC_CODEC_PCM24:
        return IC_PCM24_SAMPLES_PER_FRAME;
    case IC_CODEC_PCM48:
        return IC_PCM48_SAMPLES_PER_FRAME;
    case IC_CODEC_PCM16:
    default:
        return IC_PCM16_SAMPLES_PER_FRAME;
    }
}

#if CONFIG_INTERCOM_OPUS
static esp_err_t opus_codec_init(void)
{
    int err = OPUS_OK;
    s_opus_encoder = opus_encoder_create(IC_PCM24_SAMPLE_RATE, 1, OPUS_APPLICATION_VOIP, &err);
    if (!s_opus_encoder || err != OPUS_OK) {
        ESP_LOGE(TAG, "failed to create Opus encoder: %d", err);
        return ESP_FAIL;
    }
    s_opus_decoder = opus_decoder_create(IC_PCM24_SAMPLE_RATE, 1, &err);
    if (!s_opus_decoder || err != OPUS_OK) {
        ESP_LOGE(TAG, "failed to create Opus decoder: %d", err);
        return ESP_FAIL;
    }
    opus_encoder_ctl(s_opus_encoder, OPUS_SET_BITRATE(CONFIG_INTERCOM_OPUS_BITRATE_BPS));
    opus_encoder_ctl(s_opus_encoder, OPUS_SET_COMPLEXITY(CONFIG_INTERCOM_OPUS_COMPLEXITY));
    opus_encoder_ctl(s_opus_encoder, OPUS_SET_SIGNAL(OPUS_SIGNAL_VOICE));
    opus_encoder_ctl(s_opus_encoder, OPUS_SET_BANDWIDTH(OPUS_BANDWIDTH_SUPERWIDEBAND));
    opus_encoder_ctl(s_opus_encoder, OPUS_SET_PACKET_LOSS_PERC(5));
    ESP_LOGI(TAG,
             "Opus codec initialized: rate=%u bitrate=%u complexity=%u",
             (unsigned)IC_PCM24_SAMPLE_RATE,
             (unsigned)CONFIG_INTERCOM_OPUS_BITRATE_BPS,
             (unsigned)CONFIG_INTERCOM_OPUS_COMPLEXITY);
    return ESP_OK;
}
#endif

static int16_t clamp_i32_to_i16(int32_t value)
{
    if (value > INT16_MAX) {
        return INT16_MAX;
    }
    if (value < INT16_MIN) {
        return INT16_MIN;
    }
    return (int16_t)value;
}

static int16_t lerp_i16(int16_t a, int16_t b, int numerator, int denominator)
{
    int32_t mixed = ((int32_t)a * (denominator - numerator)) + ((int32_t)b * numerator);
    if (mixed >= 0) {
        mixed += denominator / 2;
    } else {
        mixed -= denominator / 2;
    }
    return clamp_i32_to_i16(mixed / denominator);
}

static int16_t history_sample_or_current(const int16_t *in, int index)
{
    if (index >= 0) {
        return in[index];
    }
    int history_index = (int)(sizeof(s_capture_resample_history) / sizeof(s_capture_resample_history[0])) + index;
    if (history_index < 0) {
        history_index = 0;
    }
    return s_capture_resample_history[history_index];
}

static void capture_resample_history_prepare(const int16_t *in)
{
    if (s_capture_resample_history_ready) {
        return;
    }
    for (size_t i = 0; i < sizeof(s_capture_resample_history) / sizeof(s_capture_resample_history[0]); i++) {
        s_capture_resample_history[i] = in ? in[0] : 0;
    }
    s_capture_resample_history_ready = true;
}

static void capture_resample_history_update(const int16_t *in)
{
    size_t history_len = sizeof(s_capture_resample_history) / sizeof(s_capture_resample_history[0]);
    for (size_t i = 0; i < history_len; i++) {
        s_capture_resample_history[i] = in[ESP32_AUDIO_HW_SAMPLES_PER_FRAME - history_len + i];
    }
    s_capture_resample_history_ready = true;
}

static int16_t fir_7tap_48k_to_16k_causal(const int16_t *in, size_t center)
{
    static const int taps[7] = {1, 3, 6, 8, 6, 3, 1};
    int32_t sum = 0;
    for (int i = 0; i < 7; i++) {
        sum += (int32_t)history_sample_or_current(in, (int)center + i - 6) * taps[i];
    }
    return clamp_i32_to_i16(sum / 28);
}

static int16_t fir_5tap_48k_to_24k_causal(const int16_t *in, size_t center)
{
    static const int taps[5] = {1, 4, 6, 4, 1};
    int32_t sum = 0;
    for (int i = 0; i < 5; i++) {
        sum += (int32_t)history_sample_or_current(in, (int)center + i - 4) * taps[i];
    }
    return clamp_i32_to_i16(sum / 16);
}

static void resample_network_to_hw(const int16_t *in, size_t in_samples, int16_t *out)
{
    if (!in || !out || in_samples == 0) {
        if (out) {
            memset(out, 0, ESP32_AUDIO_HW_SAMPLES_PER_FRAME * sizeof(int16_t));
        }
        return;
    }
    if (in_samples == ESP32_AUDIO_HW_SAMPLES_PER_FRAME) {
        memcpy(out, in, ESP32_AUDIO_HW_SAMPLES_PER_FRAME * sizeof(int16_t));
        return;
    }
    if (in_samples == IC_PCM24_SAMPLES_PER_FRAME) {
        for (size_t i = 0; i < in_samples; i++) {
            out[i * 2] = in[i];
            out[i * 2 + 1] = lerp_i16(in[i], in[(i + 1 < in_samples) ? i + 1 : i], 1, 2);
        }
        return;
    }
    if (in_samples == IC_PCM16_SAMPLES_PER_FRAME) {
        for (size_t i = 0; i < in_samples; i++) {
            int16_t next = in[(i + 1 < in_samples) ? i + 1 : i];
            out[i * 3] = in[i];
            out[i * 3 + 1] = lerp_i16(in[i], next, 1, 3);
            out[i * 3 + 2] = lerp_i16(in[i], next, 2, 3);
        }
        return;
    }
    for (size_t i = 0; i < ESP32_AUDIO_HW_SAMPLES_PER_FRAME; i++) {
        size_t source = (i * in_samples) / ESP32_AUDIO_HW_SAMPLES_PER_FRAME;
        if (source >= in_samples) {
            source = in_samples - 1;
        }
        out[i] = in[source];
    }
}

static void resample_hw_to_network(const int16_t *in, int16_t *out, size_t out_samples)
{
    if (!in || !out || out_samples == 0) {
        return;
    }
    capture_resample_history_prepare(in);
    if (out_samples == ESP32_AUDIO_HW_SAMPLES_PER_FRAME) {
        memcpy(out, in, ESP32_AUDIO_HW_SAMPLES_PER_FRAME * sizeof(int16_t));
        capture_resample_history_update(in);
        return;
    }
    if (out_samples == IC_PCM24_SAMPLES_PER_FRAME) {
        for (size_t i = 0; i < out_samples; i++) {
            out[i] = fir_5tap_48k_to_24k_causal(in, i * 2 + 1);
        }
        capture_resample_history_update(in);
        return;
    }
    if (out_samples == IC_PCM16_SAMPLES_PER_FRAME) {
        for (size_t i = 0; i < out_samples; i++) {
            out[i] = fir_7tap_48k_to_16k_causal(in, i * 3 + 2);
        }
        capture_resample_history_update(in);
        return;
    }
    for (size_t i = 0; i < out_samples; i++) {
        size_t start = (i * ESP32_AUDIO_HW_SAMPLES_PER_FRAME) / out_samples;
        size_t end = ((i + 1) * ESP32_AUDIO_HW_SAMPLES_PER_FRAME) / out_samples;
        if (end <= start) {
            end = start + 1;
        }
        int sum = 0;
        size_t count = 0;
        for (size_t j = start; j < end && j < ESP32_AUDIO_HW_SAMPLES_PER_FRAME; j++) {
            sum += in[j];
            count++;
        }
        out[i] = count ? (int16_t)(sum / (int)count) : 0;
    }
    capture_resample_history_update(in);
}

static uint32_t audio_cue_switch_ramp_samples(uint32_t sample_rate)
{
    uint32_t samples = sample_rate * AUDIO_CUE_SWITCH_RAMP_MS / 1000;
    return samples < 16 ? 16 : samples;
}

static void audio_cue_lut_init(void)
{
    for (size_t i = 0; i < AUDIO_CUE_SINE_LUT_SIZE; i++) {
        float phase = 2.0f * AUDIO_CUE_PI * (float)i / (float)AUDIO_CUE_SINE_LUT_SIZE;
        s_audio_cue_sine_lut[i] = (int16_t)lrintf(sinf(phase) * 32767.0f);
    }
    s_audio_cue_sine_lut_ready = true;
}

static void audio_cue_activate_locked(audio_cue_kind_t kind, uint32_t sample_rate, uint16_t gain_percent)
{
    s_audio_cue.kind = kind;
    s_audio_cue.pending_kind = AUDIO_CUE_NONE;
    s_audio_cue.sample_index = 0;
    s_audio_cue.sample_rate = sample_rate;
    s_audio_cue.pending_sample_rate = 0;
    s_audio_cue.release_index = 0;
    s_audio_cue.release_samples = 0;
    s_audio_cue.gain_percent = gain_percent;
    s_audio_cue.pending_gain_percent = 0;
}

static void audio_cue_replace(audio_cue_kind_t kind)
{
    uint32_t sample_rate = ESP32_AUDIO_HW_SAMPLE_RATE;
    uint16_t gain_percent = audio_config_snapshot().notification_gain_percent;
    taskENTER_CRITICAL(&s_audio_cue_lock);
    audio_cue_activate_locked(kind, sample_rate, gain_percent);
    taskEXIT_CRITICAL(&s_audio_cue_lock);
}

static void audio_cue_start(audio_cue_kind_t kind)
{
    uint32_t sample_rate = ESP32_AUDIO_HW_SAMPLE_RATE;
    uint16_t gain_percent = audio_config_snapshot().notification_gain_percent;
    taskENTER_CRITICAL(&s_audio_cue_lock);
    if (s_audio_cue.kind == AUDIO_CUE_NONE) {
        audio_cue_activate_locked(kind, sample_rate, gain_percent);
    } else if (s_audio_cue.kind != kind || s_audio_cue.pending_kind != AUDIO_CUE_NONE) {
        s_audio_cue.pending_kind = kind;
        s_audio_cue.pending_sample_rate = sample_rate;
        s_audio_cue.pending_gain_percent = gain_percent;
        if (s_audio_cue.release_samples == 0) {
            s_audio_cue.release_index = 0;
            s_audio_cue.release_samples = audio_cue_switch_ramp_samples(s_audio_cue.sample_rate);
        }
    }
    taskEXIT_CRITICAL(&s_audio_cue_lock);
}

static float cue_tone_envelope(uint32_t sample, uint32_t total)
{
    if (total == 0) {
        return 0.0f;
    }
    uint32_t attack = total / 5;
    uint32_t release = total / 3;
    if (attack < 16) {
        attack = 16;
    }
    if (release < 16) {
        release = 16;
    }
    if (attack + release > total) {
        attack = total / 3;
        release = total / 3;
    }
    float envelope = 1.0f;
    if (attack > 0 && sample < attack) {
        envelope *= smoothstep_unit((float)sample / (float)attack);
    }
    if (release > 0 && sample + release >= total) {
        uint32_t remaining = total > sample ? total - sample : 0;
        envelope *= smoothstep_unit((float)remaining / (float)release);
    }
    float progress = (float)sample / (float)total;
    return envelope * (1.0f - (0.55f * progress));
}

static float cue_release_multiplier(uint32_t index, uint32_t samples)
{
    if (samples == 0) {
        return 1.0f;
    }
    return 1.0f - smoothstep_unit((float)index / (float)samples);
}

static void audio_cue_params(audio_cue_kind_t kind,
                             uint32_t sample_rate,
                             uint32_t sample_index,
                             uint32_t *total,
                             uint32_t *freq,
                             uint32_t *local,
                             uint32_t *tone_total,
                             float *amplitude_scale)
{
    *total = 0;
    *freq = 0;
    *local = 0;
    *tone_total = 0;
    *amplitude_scale = 1.0f;

    switch (kind) {
    case AUDIO_CUE_CONNECTED: {
        uint32_t tone_a = sample_rate * 90 / 1000;
        uint32_t gap = sample_rate * 45 / 1000;
        uint32_t tone_b = sample_rate * 120 / 1000;
        *total = tone_a + gap + tone_b;
        *amplitude_scale = 0.58f;
        if (sample_index < tone_a) {
            *freq = 523;
            *local = sample_index;
            *tone_total = tone_a;
        } else if (sample_index < tone_a + gap) {
            *freq = 0;
        } else {
            *freq = 659;
            *local = sample_index - tone_a - gap;
            *tone_total = tone_b;
        }
        break;
    }
    case AUDIO_CUE_DISCONNECTED: {
        uint32_t tone_a = sample_rate * 190 / 1000;
        *total = tone_a;
        *amplitude_scale = 0.42f;
        if (sample_index < tone_a) {
            *freq = 330;
            *local = sample_index;
            *tone_total = tone_a;
        }
        break;
    }
    case AUDIO_CUE_RECONNECTING: {
        uint32_t tone_a = sample_rate * 170 / 1000;
        *total = tone_a;
        *amplitude_scale = 0.22f;
        if (sample_index < tone_a) {
            *freq = 392;
            *local = sample_index;
            *tone_total = tone_a;
        }
        break;
    }
    case AUDIO_CUE_NONE:
    default:
        break;
    }
}

static int16_t audio_cue_next_sample(void)
{
    audio_cue_state_t cue;
    taskENTER_CRITICAL(&s_audio_cue_lock);
    cue = s_audio_cue;
    taskEXIT_CRITICAL(&s_audio_cue_lock);

    if (cue.kind == AUDIO_CUE_NONE || cue.sample_rate == 0) {
        return 0;
    }

    uint32_t total;
    uint32_t freq;
    uint32_t local;
    uint32_t tone_total;
    float amplitude_scale;
    audio_cue_params(cue.kind, cue.sample_rate, cue.sample_index, &total, &freq, &local, &tone_total, &amplitude_scale);

    int16_t sample = 0;
    if (cue.sample_index < total && freq > 0 && tone_total > 0) {
        float amplitude = AUDIO_CUE_AMPLITUDE * amplitude_scale * (float)cue.gain_percent / 100.0f;
        uint32_t phase_index =
            (uint32_t)(((uint64_t)local * (uint64_t)freq * AUDIO_CUE_SINE_LUT_SIZE / cue.sample_rate) &
                       (AUDIO_CUE_SINE_LUT_SIZE - 1));
        float sine = s_audio_cue_sine_lut_ready ? (float)s_audio_cue_sine_lut[phase_index] / 32767.0f : 0.0f;
        float shaped = sine * amplitude * cue_tone_envelope(local, tone_total);
        if (cue.release_samples > 0) {
            shaped *= cue_release_multiplier(cue.release_index, cue.release_samples);
        }
        sample = (int16_t)shaped;
    }

    taskENTER_CRITICAL(&s_audio_cue_lock);
    if (s_audio_cue.kind == cue.kind && s_audio_cue.sample_index == cue.sample_index) {
        if (s_audio_cue.release_samples > 0) {
            s_audio_cue.sample_index++;
            s_audio_cue.release_index++;
            if (s_audio_cue.release_index >= s_audio_cue.release_samples) {
                if (s_audio_cue.pending_kind != AUDIO_CUE_NONE) {
                    audio_cue_activate_locked(s_audio_cue.pending_kind,
                                              s_audio_cue.pending_sample_rate,
                                              s_audio_cue.pending_gain_percent);
                } else {
                    s_audio_cue.kind = AUDIO_CUE_NONE;
                    s_audio_cue.sample_index = 0;
                    s_audio_cue.release_index = 0;
                    s_audio_cue.release_samples = 0;
                }
            }
        } else {
            s_audio_cue.sample_index++;
            if (s_audio_cue.sample_index >= total) {
                if (s_audio_cue.pending_kind != AUDIO_CUE_NONE) {
                    audio_cue_activate_locked(s_audio_cue.pending_kind,
                                              s_audio_cue.pending_sample_rate,
                                              s_audio_cue.pending_gain_percent);
                } else {
                    s_audio_cue.kind = AUDIO_CUE_NONE;
                    s_audio_cue.sample_index = 0;
                }
            }
        }
    }
    taskEXIT_CRITICAL(&s_audio_cue_lock);
    return sample;
}

static uint32_t samples_for_ms_u32(uint32_t sample_rate, uint32_t ms)
{
    uint32_t samples = sample_rate * ms / 1000;
    return samples == 0 ? 1 : samples;
}

static float smoothstep_unit(float x)
{
    if (x <= 0.0f) {
        return 0.0f;
    }
    if (x >= 1.0f) {
        return 1.0f;
    }
    return x * x * (3.0f - 2.0f * x);
}

static void fill_playback_fade_to_silence(int16_t *frame, size_t samples, int16_t last_sample)
{
    if (!frame || samples == 0) {
        return;
    }
    for (size_t i = 0; i < samples; i++) {
        int remaining = (int)(samples - i - 1);
        frame[i] = (int16_t)((int)last_sample * remaining / (int)samples);
    }
}

static void apply_playback_fade_in(int16_t *frame, size_t samples)
{
    if (!frame || samples == 0) {
        return;
    }
    for (size_t i = 0; i < samples; i++) {
        frame[i] = (int16_t)((int)frame[i] * (int)i / (int)samples);
    }
}

static int16_t playback_idle_floor_sample(void)
{
    if (!CONFIG_INTERCOM_PLAYBACK_IDLE_FLOOR_ENABLED || CONFIG_INTERCOM_PLAYBACK_IDLE_FLOOR_AMPLITUDE <= 0) {
        return 0;
    }
    return (int16_t)CONFIG_INTERCOM_PLAYBACK_IDLE_FLOOR_AMPLITUDE;
}

static void request_codec_output_mute(bool mute)
{
    bool changed = false;
    taskENTER_CRITICAL(&s_codec_mute_lock);
    if (s_codec_output_mute_requested != mute) {
        s_codec_output_mute_requested = mute;
        changed = true;
    }
    taskEXIT_CRITICAL(&s_codec_mute_lock);

    if (changed && s_codec_mute_queue) {
        (void)xQueueOverwrite(s_codec_mute_queue, &mute);
    }
}

static void codec_output_mute_state_set(bool mute)
{
    taskENTER_CRITICAL(&s_codec_mute_lock);
    s_codec_output_mute_requested = mute;
    taskEXIT_CRITICAL(&s_codec_mute_lock);
}

static void playback_codec_mute_gate_update(playback_codec_mute_gate_t *gate, bool frame_active)
{
    if (frame_active) {
        gate->idle_frames = 0;
        if (gate->active_preroll_frames < PLAYBACK_CODEC_UNMUTE_PREROLL_FRAMES) {
            gate->active_preroll_frames++;
            request_codec_output_mute(true);
        } else {
            request_codec_output_mute(false);
        }
        return;
    }

    gate->active_preroll_frames = 0;
    if (gate->idle_frames < PLAYBACK_CODEC_IDLE_MUTE_FRAMES) {
        gate->idle_frames++;
    }
    if (gate->idle_frames >= PLAYBACK_CODEC_IDLE_MUTE_FRAMES) {
        request_codec_output_mute(true);
    }
}

static bool physical_button_enabled(const physical_button_t *button)
{
    return button && button->gpio >= 0 && button->id && button->id[0] != '\0';
}

static bool gpio_supports_internal_pullup(int gpio)
{
    return gpio >= 0 && !(gpio >= 34 && gpio <= 39);
}

static void ui_mark_changed_locked(void)
{
    s_ui_state_version++;
}

static void ui_seed_buttons_locked(void)
{
    for (size_t i = 0; i < MAX_BUTTONS; i++) {
        physical_button_t *button = &s_dedicated_buttons[i];
        memset(&s_ui_state.buttons[i], 0, sizeof(s_ui_state.buttons[i]));
        strlcpy(s_ui_state.buttons[i].id, button->id ? button->id : "", sizeof(s_ui_state.buttons[i].id));
        strlcpy(s_ui_state.buttons[i].label,
                button->label && button->label[0] ? button->label : s_ui_state.buttons[i].id,
                sizeof(s_ui_state.buttons[i].label));
        s_ui_state.buttons[i].enabled = physical_button_enabled(button);
    }
}

static void ui_state_init(void)
{
    taskENTER_CRITICAL(&s_ui_lock);
    memset(&s_ui_state, 0, sizeof(s_ui_state));
    s_ui_state.user_id = CONFIG_INTERCOM_USER_ID;
    strlcpy(s_ui_state.blocking_status, "Starting", sizeof(s_ui_state.blocking_status));
    ui_seed_buttons_locked();
    ui_mark_changed_locked();
    taskEXIT_CRITICAL(&s_ui_lock);
}

static ui_state_t ui_state_snapshot(void)
{
    ui_state_t snapshot;
    taskENTER_CRITICAL(&s_ui_lock);
    snapshot = s_ui_state;
    taskEXIT_CRITICAL(&s_ui_lock);
    return snapshot;
}

static void ui_set_wifi_connected(bool connected)
{
    taskENTER_CRITICAL(&s_ui_lock);
    if (s_ui_state.wifi_connected != connected) {
        s_ui_state.wifi_connected = connected;
        if (!connected) {
            s_ui_state.control_connected = false;
            s_ui_state.config_received = false;
        }
        ui_mark_changed_locked();
    }
    taskEXIT_CRITICAL(&s_ui_lock);
}

static void ui_set_control_connected(bool connected)
{
    taskENTER_CRITICAL(&s_ui_lock);
    if (s_ui_state.control_connected != connected) {
        s_ui_state.control_connected = connected;
        if (!connected) {
            s_ui_state.config_received = false;
        }
        ui_mark_changed_locked();
    }
    taskEXIT_CRITICAL(&s_ui_lock);
}

static void ui_set_blocking_status(const char *status)
{
    taskENTER_CRITICAL(&s_ui_lock);
    strlcpy(s_ui_state.blocking_status, status && status[0] ? status : "", sizeof(s_ui_state.blocking_status));
    ui_mark_changed_locked();
    taskEXIT_CRITICAL(&s_ui_lock);
}

static void ui_set_transient_status(const char *status)
{
    taskENTER_CRITICAL(&s_ui_lock);
    strlcpy(s_ui_state.transient_status, status && status[0] ? status : "", sizeof(s_ui_state.transient_status));
    s_ui_state.transient_until_us = status && status[0] ? esp_timer_get_time() + DISPLAY_TRANSIENT_US : 0;
    ui_mark_changed_locked();
    taskEXIT_CRITICAL(&s_ui_lock);
}

static void ui_set_reply_state(bool held, uint16_t target)
{
    taskENTER_CRITICAL(&s_ui_lock);
    s_ui_state.reply_held = held;
    s_ui_state.reply_target = target;
    if (!held) {
        s_ui_state.reply_target = 0;
    }
    ui_mark_changed_locked();
    taskEXIT_CRITICAL(&s_ui_lock);
}

static void ui_set_button_active(const char *id, bool active)
{
    taskENTER_CRITICAL(&s_ui_lock);
    for (size_t i = 0; i < MAX_BUTTONS; i++) {
        if (string_eq(s_ui_state.buttons[i].id, id)) {
            s_ui_state.buttons[i].active = active;
            ui_mark_changed_locked();
            break;
        }
    }
    taskEXIT_CRITICAL(&s_ui_lock);
}

static void control_connected_set(bool connected)
{
    taskENTER_CRITICAL(&s_control_state_lock);
    s_control_connected = connected;
    if (connected) {
        s_control_hello_pending = true;
        s_control_startup_config_pending = false;
    } else {
        s_control_hello_pending = false;
        s_control_startup_config_pending = false;
    }
    taskEXIT_CRITICAL(&s_control_state_lock);
    ui_set_control_connected(connected);
}

static bool control_connected_snapshot(void)
{
    bool connected;
    taskENTER_CRITICAL(&s_control_state_lock);
    connected = s_control_connected;
    taskEXIT_CRITICAL(&s_control_state_lock);
    return connected;
}

static void watchdog_init_if_enabled(void)
{
#if CONFIG_INTERCOM_TASK_WATCHDOG
    esp_task_wdt_config_t config = {
        .timeout_ms = CONFIG_INTERCOM_TASK_WATCHDOG_TIMEOUT_SECONDS * 1000,
        .idle_core_mask = 0,
        .trigger_panic = true,
    };
    esp_err_t err = esp_task_wdt_init(&config);
    if (err == ESP_ERR_INVALID_STATE) {
        ESP_LOGI(TAG, "task watchdog already initialized");
    } else if (err != ESP_OK) {
        ESP_LOGW(TAG, "failed to initialize task watchdog: %s", esp_err_to_name(err));
    } else {
        ESP_LOGI(TAG,
                 "task watchdog initialized: timeout=%u seconds",
                 (unsigned)CONFIG_INTERCOM_TASK_WATCHDOG_TIMEOUT_SECONDS);
    }
#endif
}

static void watchdog_register_current_task(const char *name)
{
#if CONFIG_INTERCOM_TASK_WATCHDOG
    esp_err_t err = esp_task_wdt_add(NULL);
    if (err == ESP_ERR_INVALID_STATE || err == ESP_ERR_INVALID_ARG) {
        ESP_LOGW(TAG, "task watchdog unavailable for %s: %s", name, esp_err_to_name(err));
    } else if (err != ESP_OK) {
        ESP_LOGW(TAG, "failed to register %s with task watchdog: %s", name, esp_err_to_name(err));
    }
#else
    (void)name;
#endif
}

static void watchdog_reset_current_task(void)
{
#if CONFIG_INTERCOM_TASK_WATCHDOG
    (void)esp_task_wdt_reset();
#endif
}

static bool control_take_hello_pending(void)
{
    bool pending = false;
    taskENTER_CRITICAL(&s_control_state_lock);
    if (s_control_connected && s_control_hello_pending) {
        s_control_hello_pending = false;
        pending = true;
    }
    taskEXIT_CRITICAL(&s_control_state_lock);
    return pending;
}

static void control_request_startup_config(void)
{
    taskENTER_CRITICAL(&s_control_state_lock);
    if (s_control_connected) {
        s_control_startup_config_pending = true;
    }
    taskEXIT_CRITICAL(&s_control_state_lock);
}

static bool control_take_startup_config_pending(void)
{
    bool pending = false;
    taskENTER_CRITICAL(&s_control_state_lock);
    if (s_control_connected && s_control_startup_config_pending) {
        s_control_startup_config_pending = false;
        pending = true;
    }
    taskEXIT_CRITICAL(&s_control_state_lock);
    return pending;
}

static uint16_t current_user_id(void)
{
    uint16_t user_id;
    taskENTER_CRITICAL(&s_control_state_lock);
    user_id = s_runtime_user_id;
    taskEXIT_CRITICAL(&s_control_state_lock);
    return user_id ? user_id : CONFIG_INTERCOM_USER_ID;
}

static void set_current_user_id(uint16_t user_id)
{
    if (user_id == 0) {
        return;
    }
    taskENTER_CRITICAL(&s_control_state_lock);
    s_runtime_user_id = user_id;
    taskEXIT_CRITICAL(&s_control_state_lock);
}

static void format_generated_client_uid(char *out, size_t out_len)
{
    uint32_t a = esp_random();
    uint32_t b = esp_random();
    uint32_t c = esp_random();
    uint32_t d = esp_random();
    snprintf(out,
             out_len,
             "%08" PRIx32 "-%04" PRIx32 "-4%03" PRIx32 "-%04" PRIx32 "-%08" PRIx32 "%04" PRIx32,
             a,
             b & 0xffff,
             c & 0x0fff,
             ((c >> 16) & 0x3fff) | 0x8000,
             d,
             (b >> 16) & 0xffff);
}

static void client_identity_init(void)
{
    const char *configured = CONFIG_INTERCOM_CLIENT_UID;
    if (configured && configured[0] != '\0') {
        strlcpy(s_client_uid, configured, sizeof(s_client_uid));
        ESP_LOGI(TAG, "using configured client UID %s", s_client_uid);
        return;
    }

    nvs_handle_t nvs = 0;
    esp_err_t err = nvs_open("intercom", NVS_READWRITE, &nvs);
    if (err != ESP_OK) {
        format_generated_client_uid(s_client_uid, sizeof(s_client_uid));
        ESP_LOGW(TAG, "could not open NVS for client UID (%s); using volatile UID %s", esp_err_to_name(err), s_client_uid);
        return;
    }

    size_t len = sizeof(s_client_uid);
    err = nvs_get_str(nvs, "client_uid", s_client_uid, &len);
    if (err == ESP_OK && s_client_uid[0] != '\0') {
        ESP_LOGI(TAG, "loaded client UID %s", s_client_uid);
        nvs_close(nvs);
        return;
    }

    format_generated_client_uid(s_client_uid, sizeof(s_client_uid));
    err = nvs_set_str(nvs, "client_uid", s_client_uid);
    if (err == ESP_OK) {
        err = nvs_commit(nvs);
    }
    if (err != ESP_OK) {
        ESP_LOGW(TAG, "could not persist client UID (%s); using %s", esp_err_to_name(err), s_client_uid);
    } else {
        ESP_LOGI(TAG, "generated and stored client UID %s", s_client_uid);
    }
    nvs_close(nvs);
}

static bool websocket_ready(void)
{
    return control_connected_snapshot() && s_ws_client && esp_websocket_client_is_connected(s_ws_client);
}

static esp_err_t send_json(cJSON *json)
{
    if (!json || !websocket_ready()) {
        return ESP_FAIL;
    }
    char *text = cJSON_PrintUnformatted(json);
    if (!text) {
        return ESP_ERR_NO_MEM;
    }

    if (!s_ws_send_lock || xSemaphoreTake(s_ws_send_lock, pdMS_TO_TICKS(CONTROL_SEND_MUTEX_TIMEOUT_MS)) != pdTRUE) {
        cJSON_free(text);
        return ESP_ERR_TIMEOUT;
    }
    if (!websocket_ready()) {
        xSemaphoreGive(s_ws_send_lock);
        cJSON_free(text);
        return ESP_FAIL;
    }
    int rc = esp_websocket_client_send_text(s_ws_client, text, strlen(text), pdMS_TO_TICKS(CONTROL_SEND_TIMEOUT_MS));
    xSemaphoreGive(s_ws_send_lock);
    cJSON_free(text);
    if (rc < 0) {
        return ESP_FAIL;
    }
    return ESP_OK;
}

static void log_control_send_result(const char *label, esp_err_t err, bool warn)
{
    if (err == ESP_OK) {
        return;
    }
    if (warn) {
        ESP_LOGW(TAG, "failed to send control %s: %s", label, esp_err_to_name(err));
    } else {
        ESP_LOGD(TAG, "skipped control %s send: %s", label, esp_err_to_name(err));
    }
}

static cJSON *channel_array_json(const channel_list_t *channels)
{
    cJSON *array = cJSON_CreateArray();
    if (!array) {
        return NULL;
    }
    for (size_t i = 0; channels && i < channels->count; i++) {
        cJSON_AddItemToArray(array, cJSON_CreateNumber(channels->values[i]));
    }
    return array;
}

static void send_hello(void)
{
    uint16_t user_id = current_user_id();
    cJSON *root = cJSON_CreateObject();
    cJSON_AddStringToObject(root, "type", "hello");
    cJSON_AddNumberToObject(root, "user_id", user_id);
    cJSON_AddNumberToObject(root, "requested_user_id", CONFIG_INTERCOM_USER_ID);
    cJSON_AddStringToObject(root, "client_uid", s_client_uid);
    cJSON_AddStringToObject(root, "role", "client");

    cJSON *codecs = cJSON_CreateArray();
    cJSON_AddItemToArray(codecs, cJSON_CreateString("pcm16"));
    cJSON_AddItemToArray(codecs, cJSON_CreateString("pcm24"));
    cJSON_AddItemToArray(codecs, cJSON_CreateString("pcm48"));
#if CONFIG_INTERCOM_OPUS
    cJSON_AddItemToArray(codecs, cJSON_CreateString("opus"));
#endif
    cJSON_AddItemToObject(root, "codecs", codecs);
    cJSON *codec_config = codec_config_json();
    if (codec_config) {
        cJSON_AddItemToObject(root, "codec_config", codec_config);
    }

    cJSON *buttons = cJSON_CreateArray();
    for (size_t i = 0; i < sizeof(s_dedicated_buttons) / sizeof(s_dedicated_buttons[0]); i++) {
        if (!physical_button_enabled(&s_dedicated_buttons[i])) {
            continue;
        }
        cJSON *button = cJSON_CreateObject();
        cJSON_AddStringToObject(button, "id", s_dedicated_buttons[i].id);
        cJSON_AddStringToObject(button, "label", s_dedicated_buttons[i].label);
        cJSON_AddItemToArray(buttons, button);
    }
    cJSON_AddItemToObject(root, "buttons", buttons);

    ESP_LOGI(TAG, "sending hello for user %u uid=%s", (unsigned)user_id, s_client_uid);
    log_control_send_result("hello", send_json(root), true);
    cJSON_Delete(root);
}

static void send_startup_config(void)
{
    cJSON *root = cJSON_CreateObject();
    cJSON_AddStringToObject(root, "type", "config");
    cJSON_AddNumberToObject(root, "user_id", current_user_id());
    cJSON_AddStringToObject(root, "role", "client");
    cJSON_AddStringToObject(root, "codec", codec_wire(runtime_codec_snapshot()));
    cJSON_AddStringToObject(root, "talk_mode", "ptt");

    xSemaphoreTake(s_config_lock, portMAX_DELAY);
    cJSON_AddItemToObject(root, "listen", channel_array_json(&s_config.listen));
    cJSON_AddItemToObject(root, "tx", channel_array_json(&s_config.tx));
    xSemaphoreGive(s_config_lock);
    cJSON_AddItemToObject(root, "vol", cJSON_CreateObject());

    ESP_LOGI(TAG, "server has no preconfig; seeding startup listen/TX defaults");
    log_control_send_result("startup config", send_json(root), true);
    cJSON_Delete(root);
}

static void send_talk_control(bool active)
{
    cJSON *root = cJSON_CreateObject();
    cJSON_AddStringToObject(root, "type", "talk");
    cJSON_AddNumberToObject(root, "user_id", current_user_id());
    cJSON_AddBoolToObject(root, "active", active);
    log_control_send_result("talk", send_json(root), true);
    cJSON_Delete(root);
}

static void send_button_control(const char *button_id, bool pressed)
{
    cJSON *root = cJSON_CreateObject();
    cJSON_AddStringToObject(root, "type", "button");
    cJSON_AddNumberToObject(root, "user_id", current_user_id());
    cJSON_AddStringToObject(root, "button_id", button_id);
    cJSON_AddBoolToObject(root, "pressed", pressed);
    log_control_send_result("button", send_json(root), true);
    cJSON_Delete(root);
}

static void send_ack_alert_control(uint64_t alert_id)
{
    cJSON *root = cJSON_CreateObject();
    cJSON_AddStringToObject(root, "type", "ack_alert");
    cJSON_AddNumberToObject(root, "user_id", current_user_id());
    cJSON_AddNumberToObject(root, "alert_id", (double)alert_id);
    log_control_send_result("ack alert", send_json(root), true);
    cJSON_Delete(root);
}

static void send_direct_call_control(uint16_t target_user_id, bool active)
{
    cJSON *root = cJSON_CreateObject();
    cJSON_AddStringToObject(root, "type", "direct_call");
    cJSON_AddNumberToObject(root, "user_id", current_user_id());
    cJSON_AddNumberToObject(root, "target_user_id", target_user_id);
    cJSON_AddBoolToObject(root, "active", active);
    cJSON_AddBoolToObject(root, "duck", false);
    log_control_send_result("direct call", send_json(root), true);
    cJSON_Delete(root);
}

static void clear_local_transmit_state(bool send_releases, const char *reason)
{
    bool release_regular = false;
    char release_button_ids[MAX_BUTTONS][MAX_BUTTON_ID];
    size_t release_button_count = 0;
    uint16_t release_reply_target = 0;

    xSemaphoreTake(s_config_lock, portMAX_DELAY);
    if (s_config.regular_talk_active) {
        release_regular = true;
        s_config.regular_talk_active = false;
    }
    for (size_t i = 0; i < s_config.button_count && release_button_count < MAX_BUTTONS; i++) {
        if (!s_config.buttons[i].active) {
            continue;
        }
        strlcpy(release_button_ids[release_button_count],
                s_config.buttons[i].id,
                sizeof(release_button_ids[release_button_count]));
        release_button_count++;
        s_config.buttons[i].active = false;
    }
    xSemaphoreGive(s_config_lock);

    release_reply_target = s_reply_active_target;
    s_reply_active_target = 0;
    ui_set_reply_state(false, 0);

    for (size_t i = 0; i < MAX_BUTTONS; i++) {
        ui_set_button_active(s_dedicated_buttons[i].id, false);
    }

    if (!release_regular && release_button_count == 0 && release_reply_target == 0) {
        return;
    }

    ESP_LOGW(TAG,
             "clearing local transmit state: reason=%s regular=%s buttons=%u reply_target=%u",
             reason ? reason : "unknown",
             release_regular ? "true" : "false",
             (unsigned)release_button_count,
             (unsigned)release_reply_target);

    if (!send_releases || !websocket_ready()) {
        return;
    }
    if (release_regular) {
        send_talk_control(false);
    }
    for (size_t i = 0; i < release_button_count; i++) {
        send_button_control(release_button_ids[i], false);
    }
    if (release_reply_target != 0) {
        send_direct_call_control(release_reply_target, false);
    }
}

static void send_ping(void)
{
    cJSON *root = cJSON_CreateObject();
    cJSON_AddStringToObject(root, "type", "ping");
    cJSON_AddNumberToObject(root, "user_id", current_user_id());
    log_control_send_result("ping", send_json(root), false);
    cJSON_Delete(root);
}

static void add_capture_channel_json(cJSON *root, const char *key, float rms, float peak, float dc_offset)
{
    cJSON *channel = cJSON_CreateObject();
    cJSON_AddNumberToObject(channel, "rms", rms);
    cJSON_AddNumberToObject(channel, "peak", peak);
    cJSON_AddNumberToObject(channel, "dc_offset", dc_offset);
    cJSON_AddItemToObject(root, key, channel);
}

static cJSON *codec_config_json(void)
{
    esp32_audio_config_t audio = audio_config_snapshot();
    ic_codec_t codec = runtime_codec_snapshot();
    cJSON *config = cJSON_CreateObject();
    if (!config) {
        return NULL;
    }
    cJSON_AddStringToObject(config, "chip", "es8388");
    cJSON_AddStringToObject(config, "active_codec", codec_wire(codec));
    cJSON_AddBoolToObject(config, "server_control_enabled", audio.server_control_enabled);
    cJSON_AddStringToObject(config, "adc_input", adc_input_name(audio.adc_input));
    cJSON_AddNumberToObject(config, "mic_pga_gain_db", audio.mic_pga_gain_db);
    cJSON_AddStringToObject(config, "capture_channel", capture_channel_name(audio.capture_channel));
    cJSON_AddNumberToObject(config, "mic_software_gain_percent", audio.mic_software_gain_percent);
    cJSON_AddNumberToObject(config, "speaker_software_gain_percent", audio.speaker_software_gain_percent);
    cJSON_AddNumberToObject(config, "notification_gain_percent", audio.notification_gain_percent);
    cJSON_AddBoolToObject(config, "high_pass_enabled", audio.high_pass_enabled);
    cJSON_AddBoolToObject(config, "alc_enabled", false);
    cJSON_AddBoolToObject(config, "noise_gate_enabled", false);
    cJSON_AddStringToObject(config, "audio_backend", "legacy_i2s_es8388");
    cJSON_AddNumberToObject(config, "hardware_sample_rate_hz", ESP32_AUDIO_HW_SAMPLE_RATE);
    cJSON_AddNumberToObject(config, "hardware_channels", ESP32_AUDIO_HW_CHANNELS);
    cJSON_AddNumberToObject(config, "hardware_bits_per_sample", ESP32_AUDIO_HW_BITS_PER_SAMPLE);
    cJSON_AddNumberToObject(config, "i2s_sample_rate_hz", ESP32_AUDIO_HW_SAMPLE_RATE);
    cJSON_AddStringToObject(config, "i2s_format", i2s_format_name());
    cJSON_AddStringToObject(config, "i2s_slot_width", i2s_slot_width_name());

    cJSON *sidetone = cJSON_CreateObject();
    if (sidetone) {
        cJSON_AddStringToObject(sidetone, "mode", sidetone_mode_name(audio.sidetone_mode));
        cJSON_AddNumberToObject(sidetone, "firmware_gain_percent", audio.sidetone_firmware_gain_percent);
        cJSON_AddNumberToObject(sidetone, "codec_bypass_gain_percent", audio.sidetone_codec_bypass_gain_percent);
        cJSON_AddNumberToObject(sidetone, "mic_bypass_gain_percent", audio.sidetone_mic_bypass_gain_percent);
        cJSON_AddBoolToObject(sidetone, "codec_bypass_available", false);
        cJSON_AddStringToObject(sidetone, "active_bypass_source", "none");
        cJSON_AddBoolToObject(sidetone, "codec_bypass_preserves_dac", true);
        cJSON_AddItemToObject(config, "sidetone", sidetone);
    }
    return config;
}

static uint32_t task_stack_high_water(TaskHandle_t handle)
{
    return handle ? (uint32_t)uxTaskGetStackHighWaterMark(handle) : 0;
}

static cJSON *wifi_health_json(void)
{
    cJSON *wifi = cJSON_CreateObject();
    if (!wifi) {
        return NULL;
    }
    wifi_ap_record_t ap = {0};
    if (esp_wifi_sta_get_ap_info(&ap) == ESP_OK) {
        cJSON_AddNumberToObject(wifi, "rssi_dbm", ap.rssi);
    }
    cJSON_AddNumberToObject(wifi, "connect_count", s_wifi_connect_count);
    cJSON_AddNumberToObject(wifi, "disconnect_count", s_wifi_disconnect_count);
    cJSON_AddNumberToObject(wifi, "control_connect_count", s_control_connect_count);
    cJSON_AddNumberToObject(wifi, "control_disconnect_count", s_control_disconnect_count);
    return wifi;
}

static cJSON *transport_health_json(void)
{
    cJSON *transport = cJSON_CreateObject();
    if (!transport) {
        return NULL;
    }
    cJSON_AddNumberToObject(transport, "udp_rx_packets", s_udp_rx_packets);
    cJSON_AddNumberToObject(transport, "udp_decode_errors", s_udp_decode_errors);
    cJSON_AddNumberToObject(transport, "udp_codec_drops", s_udp_codec_drops);
    cJSON_AddNumberToObject(transport, "udp_sequence_gaps", s_udp_sequence_gaps);
    cJSON_AddNumberToObject(transport, "udp_payload_decode_errors", s_udp_payload_decode_errors);
    cJSON_AddNumberToObject(transport, "udp_tx_send_failures", s_udp_tx_send_failures);
    cJSON_AddNumberToObject(transport, "audio_tx_queue_drops", s_audio_tx_queue_drops);
#if CONFIG_INTERCOM_OPUS
    cJSON_AddNumberToObject(transport, "opus_encode_failures", s_opus_encode_failures);
    cJSON_AddNumberToObject(transport, "opus_decode_failures", s_opus_decode_failures);
#else
    cJSON_AddNumberToObject(transport, "opus_encode_failures", 0);
    cJSON_AddNumberToObject(transport, "opus_decode_failures", 0);
#endif
    return transport;
}

static cJSON *memory_health_json(void)
{
    cJSON *memory = cJSON_CreateObject();
    if (!memory) {
        return NULL;
    }
    cJSON_AddNumberToObject(memory, "free_heap_bytes", esp_get_free_heap_size());
    cJSON_AddNumberToObject(memory, "min_free_heap_bytes", esp_get_minimum_free_heap_size());
    cJSON_AddNumberToObject(memory, "internal_free_heap_bytes", heap_caps_get_free_size(MALLOC_CAP_INTERNAL));
    cJSON_AddNumberToObject(memory,
                            "internal_largest_free_block_bytes",
                            heap_caps_get_largest_free_block(MALLOC_CAP_INTERNAL));
    cJSON_AddNumberToObject(memory, "spiram_free_heap_bytes", heap_caps_get_free_size(MALLOC_CAP_SPIRAM));
    cJSON_AddNumberToObject(memory,
                            "spiram_largest_free_block_bytes",
                            heap_caps_get_largest_free_block(MALLOC_CAP_SPIRAM));
    return memory;
}

static cJSON *task_stack_health_json(void)
{
    cJSON *stack = cJSON_CreateObject();
    if (!stack) {
        return NULL;
    }
    cJSON_AddNumberToObject(stack, "udp", task_stack_high_water(s_udp_task_handle));
    cJSON_AddNumberToObject(stack, "registration", task_stack_high_water(s_registration_task_handle));
    cJSON_AddNumberToObject(stack, "playback", task_stack_high_water(s_playback_task_handle));
    cJSON_AddNumberToObject(stack, "capture", task_stack_high_water(s_capture_task_handle));
    cJSON_AddNumberToObject(stack, "buttons", task_stack_high_water(s_button_task_handle));
#if CONFIG_INTERCOM_DISPLAY_ST7789
    cJSON_AddNumberToObject(stack, "display", task_stack_high_water(s_display_task_handle));
#endif
    return stack;
}

static cJSON *display_health_json(void)
{
    cJSON *display = cJSON_CreateObject();
    if (!display) {
        return NULL;
    }
#if CONFIG_INTERCOM_DISPLAY_ST7789
    cJSON_AddBoolToObject(display, "enabled", true);
    cJSON_AddBoolToObject(display, "initialized", s_display_initialized);
    cJSON_AddBoolToObject(display, "framebuffer_in_psram", s_display_framebuffer_in_psram);
    cJSON_AddNumberToObject(display, "framebuffer_bytes", s_display_framebuffer_bytes);
#else
    cJSON_AddBoolToObject(display, "enabled", false);
    cJSON_AddBoolToObject(display, "initialized", false);
    cJSON_AddBoolToObject(display, "framebuffer_in_psram", false);
    cJSON_AddNumberToObject(display, "framebuffer_bytes", 0);
#endif
    return display;
}

static cJSON *battery_health_json(void)
{
    cJSON *battery = cJSON_CreateObject();
    if (!battery) {
        return NULL;
    }
    cJSON_AddStringToObject(battery, "status", "unknown");
    cJSON_AddBoolToObject(battery, "present", false);
    return battery;
}

static void send_capture_health(void)
{
    capture_health_report_t report;
    taskENTER_CRITICAL(&s_capture_health_lock);
    report = s_capture_health_report;
    s_capture_health_report.ready = false;
    taskEXIT_CRITICAL(&s_capture_health_lock);

    if (!report.ready || !websocket_ready()) {
        return;
    }

    cJSON *root = cJSON_CreateObject();
    cJSON_AddStringToObject(root, "type", "capture_health");
    cJSON_AddNumberToObject(root, "user_id", current_user_id());

    cJSON *health = cJSON_CreateObject();
    cJSON_AddNumberToObject(health, "uptime_ms", (double)(esp_timer_get_time() / 1000));
    esp32_audio_config_t audio = audio_config_snapshot();
    cJSON *codec_config = codec_config_json();
    if (codec_config) {
        cJSON_AddItemToObject(health, "codec_config", codec_config);
    }
    cJSON_AddStringToObject(health, "adc_input", adc_input_name(audio.adc_input));
    cJSON_AddNumberToObject(health, "mic_pga_gain_db", audio.mic_pga_gain_db);
    cJSON_AddStringToObject(health, "capture_channel", capture_channel_name(audio.capture_channel));
    cJSON_AddNumberToObject(health, "software_gain_percent", audio.mic_software_gain_percent);
    cJSON_AddBoolToObject(health, "high_pass_enabled", audio.high_pass_enabled);
    cJSON_AddBoolToObject(health, "alc_enabled", false);
    cJSON_AddBoolToObject(health, "noise_gate_enabled", false);
    add_capture_channel_json(health, "left", report.left_rms, report.left_peak, report.left_dc_offset);
    add_capture_channel_json(health, "right", report.right_rms, report.right_peak, report.right_dc_offset);
    add_capture_channel_json(health,
                             "selected",
                             report.selected_rms,
                             report.selected_peak,
                             report.selected_dc_offset);
    size_t playback_queue_depth = 0;
    uint32_t playback_underflows = 0;
    uint32_t playback_overflows = 0;
    ic_pcm_frame_ring_stats(&s_playback_ring, &playback_queue_depth, &playback_underflows, &playback_overflows);
    cJSON_AddNumberToObject(health, "playback_queue_depth", playback_queue_depth);
    cJSON_AddNumberToObject(health, "playback_underflows", playback_underflows);
    cJSON_AddNumberToObject(health, "playback_overflows", playback_overflows);
    cJSON_AddNumberToObject(health, "playback_i2s_gap_warnings", s_audio_hw_write_gap_warnings);
    cJSON_AddNumberToObject(health, "playback_i2s_slow_warnings", s_audio_hw_write_slow_warnings);
    cJSON_AddNumberToObject(health, "playback_i2s_short_warnings", s_audio_hw_write_short_warnings);
    cJSON_AddNumberToObject(health, "free_heap_bytes", esp_get_free_heap_size());
    cJSON_AddNumberToObject(health, "min_free_heap_bytes", esp_get_minimum_free_heap_size());
    cJSON *wifi = wifi_health_json();
    if (wifi) {
        cJSON_AddItemToObject(health, "wifi", wifi);
    }
    cJSON *transport = transport_health_json();
    if (transport) {
        cJSON_AddItemToObject(health, "transport", transport);
    }
    cJSON *memory = memory_health_json();
    if (memory) {
        cJSON_AddItemToObject(health, "memory", memory);
    }
    cJSON *stack = task_stack_health_json();
    if (stack) {
        cJSON_AddItemToObject(health, "task_stack_high_water_bytes", stack);
    }
    cJSON *display = display_health_json();
    if (display) {
        cJSON_AddItemToObject(health, "display", display);
    }
    cJSON *battery = battery_health_json();
    if (battery) {
        cJSON_AddItemToObject(health, "battery", battery);
    }
    cJSON_AddNumberToObject(health, "raw_clipped_samples", report.raw_clipped_samples);
    cJSON_AddNumberToObject(health, "software_clipped_samples", report.software_clipped_samples);
    cJSON_AddNumberToObject(health, "tx_target_count", report.tx_target_count);
    cJSON_AddNumberToObject(health, "tx_packets_sent", report.tx_packets_sent);
    cJSON_AddNumberToObject(health, "tx_send_failures", report.tx_send_failures);
    cJSON_AddItemToObject(root, "health", health);

    log_control_send_result("capture health", send_json(root), false);
    cJSON_Delete(root);
}

static void parse_json_channel_array(cJSON *array, channel_list_t *out)
{
    memset(out, 0, sizeof(*out));
    cJSON *item = NULL;
    cJSON_ArrayForEach(item, array) {
        if (cJSON_IsNumber(item) && item->valuedouble > 0 && item->valuedouble <= UINT16_MAX) {
            channel_list_add(out, (uint16_t)item->valuedouble);
        }
    }
}

static void parse_json_user_array(cJSON *array, user_list_t *out)
{
    memset(out, 0, sizeof(*out));
    cJSON *item = NULL;
    cJSON_ArrayForEach(item, array) {
        if (cJSON_IsNumber(item) && item->valuedouble > 0 && item->valuedouble <= UINT16_MAX) {
            user_list_add(out, (uint16_t)item->valuedouble);
        }
    }
}

static bool active_buttons_contains(cJSON *active_buttons, const char *id)
{
    cJSON *item = NULL;
    cJSON_ArrayForEach(item, active_buttons) {
        if (cJSON_IsString(item) && string_eq(cJSON_GetStringValue(item), id)) {
            return true;
        }
    }
    return false;
}

static bool parse_adc_input_value(const char *value, esp32_adc_input_t *out)
{
    if (string_eq(value, "mic1")) {
        *out = ESP32_ADC_MIC1;
    } else if (string_eq(value, "mic2")) {
        *out = ESP32_ADC_MIC2;
    } else if (string_eq(value, "line1")) {
        *out = ESP32_ADC_LINE1;
    } else if (string_eq(value, "line2")) {
        *out = ESP32_ADC_LINE2;
    } else if (string_eq(value, "difference")) {
        *out = ESP32_ADC_DIFFERENCE;
    } else {
        return false;
    }
    return true;
}

static bool parse_capture_channel_value(const char *value, esp32_capture_channel_t *out)
{
    if (string_eq(value, "right")) {
        *out = ESP32_CAPTURE_RIGHT;
    } else if (string_eq(value, "average")) {
        *out = ESP32_CAPTURE_AVERAGE;
    } else if (string_eq(value, "left")) {
        *out = ESP32_CAPTURE_LEFT;
    } else {
        return false;
    }
    return true;
}

static bool parse_sidetone_mode_value(const char *value, esp32_sidetone_mode_t *out)
{
    if (string_eq(value, "firmware")) {
        *out = ESP32_SIDETONE_FIRMWARE;
    } else if (string_eq(value, "codec_bypass")) {
        *out = ESP32_SIDETONE_CODEC_BYPASS;
    } else if (string_eq(value, "off")) {
        *out = ESP32_SIDETONE_OFF;
    } else {
        return false;
    }
    return true;
}

static uint16_t json_u16_or(cJSON *object, const char *key, uint16_t fallback, uint16_t max)
{
    cJSON *item = cJSON_GetObjectItem(object, key);
    if (!cJSON_IsNumber(item)) {
        return fallback;
    }
    int value = (int)item->valuedouble;
    if (value < 0) {
        return 0;
    }
    if (value > max) {
        return max;
    }
    return (uint16_t)value;
}

static bool parse_esp32_audio_config(cJSON *object, esp32_audio_config_t *out)
{
    if (!cJSON_IsObject(object) || !out) {
        return false;
    }

    esp32_audio_config_t next = default_audio_config();
    cJSON *enabled = cJSON_GetObjectItem(object, "enabled");
    if (!cJSON_IsBool(enabled) || !cJSON_IsTrue(enabled)) {
        *out = next;
        return true;
    }
    next.server_control_enabled = true;

    cJSON *adc_input = cJSON_GetObjectItem(object, "adc_input");
    if (cJSON_IsString(adc_input)) {
        parse_adc_input_value(cJSON_GetStringValue(adc_input), &next.adc_input);
    }

    next.mic_pga_gain_db = (uint8_t)((json_u16_or(object, "mic_pga_gain_db", next.mic_pga_gain_db, 24) / 3) * 3);

    cJSON *capture_channel = cJSON_GetObjectItem(object, "capture_channel");
    if (cJSON_IsString(capture_channel)) {
        parse_capture_channel_value(cJSON_GetStringValue(capture_channel), &next.capture_channel);
    }

    next.mic_software_gain_percent =
        json_u16_or(object, "mic_software_gain_percent", next.mic_software_gain_percent, 400);
    next.speaker_software_gain_percent =
        json_u16_or(object, "speaker_software_gain_percent", next.speaker_software_gain_percent, 400);
    next.notification_gain_percent =
        json_u16_or(object, "notification_gain_percent", next.notification_gain_percent, 200);
    cJSON *high_pass = cJSON_GetObjectItem(object, "high_pass_enabled");
    if (cJSON_IsBool(high_pass)) {
        next.high_pass_enabled = cJSON_IsTrue(high_pass);
    }
    cJSON *alc = cJSON_GetObjectItem(object, "alc_enabled");
    if (cJSON_IsBool(alc)) {
        next.alc_enabled = cJSON_IsTrue(alc);
    }
    cJSON *noise_gate = cJSON_GetObjectItem(object, "noise_gate_enabled");
    if (cJSON_IsBool(noise_gate)) {
        next.noise_gate_enabled = cJSON_IsTrue(noise_gate);
    }

    cJSON *sidetone = cJSON_GetObjectItem(object, "sidetone");
    if (cJSON_IsObject(sidetone)) {
        cJSON *mode = cJSON_GetObjectItem(sidetone, "mode");
        if (cJSON_IsString(mode)) {
            parse_sidetone_mode_value(cJSON_GetStringValue(mode), &next.sidetone_mode);
        }
        next.sidetone_firmware_gain_percent =
            json_u16_or(sidetone, "firmware_gain_percent", next.sidetone_firmware_gain_percent, 200);
        next.sidetone_codec_bypass_gain_percent =
            json_u16_or(sidetone, "codec_bypass_gain_percent", next.sidetone_codec_bypass_gain_percent, 200);
        next.sidetone_mic_bypass_gain_percent =
            json_u16_or(sidetone, "mic_bypass_gain_percent", next.sidetone_mic_bypass_gain_percent, 400);
    }
    if (next.sidetone_mode == ESP32_SIDETONE_CODEC_BYPASS) {
        next.sidetone_mode = ESP32_SIDETONE_OFF;
    }

    *out = next;
    return true;
}

static uint64_t json_u64_value(cJSON *object, const char *key, uint64_t fallback)
{
    cJSON *item = cJSON_GetObjectItem(object, key);
    if (!cJSON_IsNumber(item) || item->valuedouble < 0) {
        return fallback;
    }
    return (uint64_t)item->valuedouble;
}

static void ui_apply_config_update(cJSON *root, const runtime_config_t *config)
{
    if (!root || !config) {
        return;
    }

    taskENTER_CRITICAL(&s_ui_lock);
    s_ui_state.config_received = true;
    s_ui_state.blocking_status[0] = '\0';

    cJSON *user_id = cJSON_GetObjectItem(root, "user_id");
    if (cJSON_IsNumber(user_id) && user_id->valuedouble > 0 && user_id->valuedouble <= UINT16_MAX) {
        s_ui_state.user_id = (uint16_t)user_id->valuedouble;
    }

    cJSON *name = cJSON_GetObjectItem(root, "name");
    strlcpy(s_ui_state.unit_name,
            cJSON_IsString(name) ? cJSON_GetStringValue(name) : "",
            sizeof(s_ui_state.unit_name));

    ui_seed_buttons_locked();
    for (size_t i = 0; i < MAX_BUTTONS; i++) {
        for (size_t j = 0; j < config->button_count; j++) {
            const button_route_t *route = &config->buttons[j];
            if (!string_eq(s_ui_state.buttons[i].id, route->id)) {
                continue;
            }
            s_ui_state.buttons[i].configured = true;
            s_ui_state.buttons[i].active = route->active;
            strlcpy(s_ui_state.buttons[i].label,
                    route->label[0] ? route->label : route->id,
                    sizeof(s_ui_state.buttons[i].label));
            break;
        }
    }

    memset(&s_ui_state.active_alert, 0, sizeof(s_ui_state.active_alert));
    cJSON *active_alerts = cJSON_GetObjectItem(root, "active_alerts");
    cJSON *alert = NULL;
    cJSON_ArrayForEach(alert, active_alerts) {
        cJSON *sender = cJSON_GetObjectItem(alert, "sender");
        if (!cJSON_IsNumber(sender) || sender->valuedouble <= 0 || sender->valuedouble > UINT16_MAX) {
            continue;
        }
        uint64_t created_at_ms = json_u64_value(alert, "created_at_ms", 0);
        if (s_ui_state.active_alert.present && created_at_ms < s_ui_state.active_alert.created_at_ms) {
            continue;
        }
        s_ui_state.active_alert.present = true;
        s_ui_state.active_alert.id = json_u64_value(alert, "id", 0);
        s_ui_state.active_alert.sender = (uint16_t)sender->valuedouble;
        s_ui_state.active_alert.created_at_ms = created_at_ms;
        cJSON *message = cJSON_GetObjectItem(alert, "message");
        strlcpy(s_ui_state.active_alert.message,
                cJSON_IsString(message) ? cJSON_GetStringValue(message) : "",
                sizeof(s_ui_state.active_alert.message));
    }

    cJSON *last_direct_caller = cJSON_GetObjectItem(root, "last_direct_caller");
    s_ui_state.has_last_direct_caller =
        cJSON_IsNumber(last_direct_caller) && last_direct_caller->valuedouble > 0 &&
        last_direct_caller->valuedouble <= UINT16_MAX;
    s_ui_state.last_direct_caller =
        s_ui_state.has_last_direct_caller ? (uint16_t)last_direct_caller->valuedouble : 0;

    s_ui_state.active_direct_call_count = 0;
    cJSON *active_direct_calls = cJSON_GetObjectItem(root, "active_direct_calls");
    cJSON *call = NULL;
    cJSON_ArrayForEach(call, active_direct_calls) {
        if (cJSON_IsTrue(cJSON_GetObjectItem(call, "active")) ||
            !cJSON_HasObjectItem(call, "active")) {
            if (s_ui_state.active_direct_call_count < UINT16_MAX) {
                s_ui_state.active_direct_call_count++;
            }
        }
    }

    ui_mark_changed_locked();
    taskEXIT_CRITICAL(&s_ui_lock);
}

static void apply_config_update(cJSON *root)
{
    cJSON *user_id = cJSON_GetObjectItem(root, "user_id");
    if (!cJSON_IsNumber(user_id) || (uint16_t)user_id->valuedouble != current_user_id()) {
        return;
    }

    runtime_config_t next;
    xSemaphoreTake(s_config_lock, portMAX_DELAY);
    next = s_config;
    xSemaphoreGive(s_config_lock);

    parse_json_channel_array(cJSON_GetObjectItem(root, "listen"), &next.listen);
    parse_json_channel_array(cJSON_GetObjectItem(root, "tx"), &next.tx);

    cJSON *codec = cJSON_GetObjectItem(root, "codec");
    if (cJSON_IsString(codec)) {
        ic_codec_t parsed_codec = next.codec;
        if (parse_codec_value(cJSON_GetStringValue(codec), &parsed_codec) && codec_supported(parsed_codec)) {
            next.codec = parsed_codec;
        } else {
            ESP_LOGW(TAG, "ignoring unsupported codec in config_update: %s", cJSON_GetStringValue(codec));
        }
    }

    cJSON *mode = cJSON_GetObjectItem(root, "talk_mode");
    if (cJSON_IsString(mode)) {
        next.talk_mode = parse_talk_mode(cJSON_GetStringValue(mode));
    }
    cJSON *regular = cJSON_GetObjectItem(root, "regular_talk_active");
    if (cJSON_IsBool(regular)) {
        next.regular_talk_active = cJSON_IsTrue(regular);
    }

    next.button_count = 0;
    cJSON *buttons = cJSON_GetObjectItem(root, "buttons");
    cJSON *active_buttons = cJSON_GetObjectItem(root, "active_buttons");
    cJSON *button = NULL;
    cJSON_ArrayForEach(button, buttons) {
        if (next.button_count >= MAX_BUTTONS) {
            break;
        }
        cJSON *id = cJSON_GetObjectItem(button, "id");
        if (!cJSON_IsString(id)) {
            continue;
        }

        button_route_t *route = &next.buttons[next.button_count++];
        memset(route, 0, sizeof(*route));
        strlcpy(route->id, cJSON_GetStringValue(id), sizeof(route->id));
        cJSON *label = cJSON_GetObjectItem(button, "label");
        strlcpy(route->label, cJSON_IsString(label) ? cJSON_GetStringValue(label) : route->id, sizeof(route->label));
        cJSON *button_mode = cJSON_GetObjectItem(button, "mode");
        route->latching = cJSON_IsString(button_mode) && string_eq(cJSON_GetStringValue(button_mode), "latching");
        route->active = active_buttons_contains(active_buttons, route->id);

        cJSON *actions = cJSON_GetObjectItem(button, "actions");
        cJSON *action = NULL;
        cJSON_ArrayForEach(action, actions) {
            cJSON *type = cJSON_GetObjectItem(action, "type");
            if (!cJSON_IsString(type) || !string_eq(cJSON_GetStringValue(type), "transmit")) {
                continue;
            }
            parse_json_channel_array(cJSON_GetObjectItem(action, "channels"), &route->tx_channels);
            parse_json_user_array(cJSON_GetObjectItem(action, "users"), &route->tx_users);
            route->duck = cJSON_IsTrue(cJSON_GetObjectItem(action, "duck"));
            break;
        }
    }

    esp32_audio_config_t audio_next;
    bool has_audio_config = parse_esp32_audio_config(cJSON_GetObjectItem(root, "esp32_audio"), &audio_next);
    if (has_audio_config) {
        esp32_audio_config_t audio_current = audio_config_snapshot();
        if (!audio_config_equal(&audio_current, &audio_next)) {
            audio_config_store(&audio_next);
            request_audio_config_apply(&audio_next);
        }
    }

    xSemaphoreTake(s_config_lock, portMAX_DELAY);
    ic_codec_t previous_codec = s_config.codec;
    s_config = next;
    xSemaphoreGive(s_config_lock);
    ui_apply_config_update(root, &next);

    if (previous_codec != next.codec) {
        esp_err_t err = audio_i2s_set_codec(next.codec);
        if (err != ESP_OK) {
            ESP_LOGW(TAG, "failed to switch I2S codec to %s: %s", codec_wire(next.codec), esp_err_to_name(err));
        }
    }

    ESP_LOGI(TAG,
             "config update: listen=%u tx=%u codec=%s mode=%s buttons=%u",
             (unsigned)next.listen.count,
             (unsigned)next.tx.count,
             codec_wire(next.codec),
             talk_mode_wire(next.talk_mode),
             (unsigned)next.button_count);
}

static void handle_control_json(const char *text, size_t len)
{
    cJSON *root = cJSON_ParseWithLength(text, len);
    if (!root) {
        ESP_LOGW(TAG, "invalid control JSON (%u bytes)", (unsigned)len);
        return;
    }

    cJSON *type = cJSON_GetObjectItem(root, "type");
    if (!cJSON_IsString(type)) {
        cJSON_Delete(root);
        return;
    }

    const char *type_text = cJSON_GetStringValue(type);
    if (string_eq(type_text, "hello")) {
        cJSON *assigned_user_id = cJSON_GetObjectItem(root, "user_id");
        if (cJSON_IsNumber(assigned_user_id)) {
            set_current_user_id((uint16_t)assigned_user_id->valuedouble);
        }
        cJSON *returned_uid = cJSON_GetObjectItem(root, "client_uid");
        if (cJSON_IsString(returned_uid) && cJSON_GetStringValue(returned_uid)[0] != '\0') {
            strlcpy(s_client_uid, cJSON_GetStringValue(returned_uid), sizeof(s_client_uid));
        }
        cJSON *enrollment = cJSON_GetObjectItem(root, "enrollment");
        if (cJSON_IsString(enrollment) && !string_eq(cJSON_GetStringValue(enrollment), "enrolled")) {
            char status[MAX_UI_STATUS];
            snprintf(status, sizeof(status), "Enrollment %s", cJSON_GetStringValue(enrollment));
            ui_set_blocking_status(status);
            ESP_LOGW(TAG,
                     "server enrollment status is %s; waiting for admin approval and not seeding startup config",
                     cJSON_GetStringValue(enrollment));
            cJSON_Delete(root);
            return;
        }
        ui_set_blocking_status("Waiting for config");
        if (!cJSON_IsTrue(cJSON_GetObjectItem(root, "preconfigured"))) {
            control_request_startup_config();
        }
    } else if (string_eq(type_text, "config_update")) {
        apply_config_update(root);
    } else if (string_eq(type_text, "error")) {
        cJSON *message = cJSON_GetObjectItem(root, "message");
        ui_set_blocking_status(cJSON_IsString(message) ? cJSON_GetStringValue(message) : "Server error");
        ESP_LOGW(TAG, "server control error: %s", cJSON_IsString(message) ? cJSON_GetStringValue(message) : "unknown");
    }

    cJSON_Delete(root);
}

static void reset_control_rx_buffer(void)
{
    s_control_rx_expected_len = 0;
    s_control_rx_received_len = 0;
}

static void handle_control_ws_data(const esp_websocket_event_data_t *data)
{
    if (!data || !data->data_ptr || data->data_len <= 0) {
        return;
    }
    if (data->op_code != 0x1 && data->op_code != 0x0) {
        return;
    }

    size_t payload_len = data->payload_len > 0 ? (size_t)data->payload_len : (size_t)data->data_len;
    size_t offset = data->payload_offset >= 0 ? (size_t)data->payload_offset : 0;
    size_t data_len = (size_t)data->data_len;

    if (payload_len > CONTROL_RX_MAX_BYTES || offset + data_len > CONTROL_RX_MAX_BYTES ||
        offset + data_len > payload_len) {
        ESP_LOGW(TAG,
                 "dropping oversized control websocket message: payload=%u offset=%u chunk=%u max=%u",
                 (unsigned)payload_len,
                 (unsigned)offset,
                 (unsigned)data_len,
                 (unsigned)CONTROL_RX_MAX_BYTES);
        reset_control_rx_buffer();
        return;
    }

    if (offset == 0) {
        s_control_rx_expected_len = payload_len;
        s_control_rx_received_len = 0;
    } else if (s_control_rx_expected_len != payload_len || offset != s_control_rx_received_len) {
        ESP_LOGW(TAG,
                 "dropping out-of-order control websocket fragment: payload=%u offset=%u received=%u",
                 (unsigned)payload_len,
                 (unsigned)offset,
                 (unsigned)s_control_rx_received_len);
        reset_control_rx_buffer();
        return;
    }

    memcpy(s_control_rx_buffer + offset, data->data_ptr, data_len);
    s_control_rx_received_len = offset + data_len;

    if (s_control_rx_received_len < s_control_rx_expected_len) {
        ESP_LOGD(TAG,
                 "buffered control websocket fragment: %u/%u bytes",
                 (unsigned)s_control_rx_received_len,
                 (unsigned)s_control_rx_expected_len);
        return;
    }

    s_control_rx_buffer[s_control_rx_expected_len] = '\0';
    handle_control_json(s_control_rx_buffer, s_control_rx_expected_len);
    reset_control_rx_buffer();
}

static void websocket_event_handler(void *handler_args,
                                    esp_event_base_t base,
                                    int32_t event_id,
                                    void *event_data)
{
    (void)handler_args;
    (void)base;
    esp_websocket_event_data_t *data = (esp_websocket_event_data_t *)event_data;

    switch (event_id) {
    case WEBSOCKET_EVENT_CONNECTED:
        ESP_LOGI(TAG, "control connected");
        s_control_connect_count++;
        control_connected_set(true);
        audio_cue_replace(AUDIO_CUE_CONNECTED);
        break;
    case WEBSOCKET_EVENT_DISCONNECTED:
        ESP_LOGW(TAG, "control disconnected");
        s_control_disconnect_count++;
        clear_local_transmit_state(false, "control disconnected");
        control_connected_set(false);
        audio_cue_replace(AUDIO_CUE_DISCONNECTED);
        reset_control_rx_buffer();
        break;
    case WEBSOCKET_EVENT_DATA:
        handle_control_ws_data(data);
        break;
    case WEBSOCKET_EVENT_ERROR:
        ESP_LOGW(TAG, "control websocket error");
        s_control_disconnect_count++;
        clear_local_transmit_state(false, "control websocket error");
        control_connected_set(false);
        break;
    default:
        break;
    }
}

static void wifi_event_handler(void *arg, esp_event_base_t event_base, int32_t event_id, void *event_data)
{
    (void)arg;
    if (event_base == WIFI_EVENT && event_id == WIFI_EVENT_STA_START) {
        ui_set_wifi_connected(false);
        esp_wifi_connect();
    } else if (event_base == WIFI_EVENT && event_id == WIFI_EVENT_STA_DISCONNECTED) {
        xEventGroupClearBits(s_wifi_events, WIFI_CONNECTED_BIT);
        s_wifi_disconnect_count++;
        clear_local_transmit_state(true, "Wi-Fi disconnected");
        ui_set_wifi_connected(false);
        ESP_LOGW(TAG, "Wi-Fi disconnected; reconnecting");
        esp_wifi_connect();
    } else if (event_base == IP_EVENT && event_id == IP_EVENT_STA_GOT_IP) {
        ip_event_got_ip_t *event = (ip_event_got_ip_t *)event_data;
        s_wifi_connect_count++;
        ESP_LOGI(TAG, "Wi-Fi connected: " IPSTR, IP2STR(&event->ip_info.ip));
        esp_wifi_set_ps(WIFI_PS_NONE);
        ui_set_wifi_connected(true);
        xEventGroupSetBits(s_wifi_events, WIFI_CONNECTED_BIT);
    }
}

static void wifi_init(void)
{
    s_wifi_events = xEventGroupCreate();
    ESP_ERROR_CHECK(esp_netif_init());
    ESP_ERROR_CHECK(esp_event_loop_create_default());
    esp_netif_create_default_wifi_sta();

    wifi_init_config_t cfg = WIFI_INIT_CONFIG_DEFAULT();
    ESP_ERROR_CHECK(esp_wifi_init(&cfg));
    ESP_ERROR_CHECK(esp_event_handler_instance_register(WIFI_EVENT, ESP_EVENT_ANY_ID, wifi_event_handler, NULL, NULL));
    ESP_ERROR_CHECK(esp_event_handler_instance_register(IP_EVENT, IP_EVENT_STA_GOT_IP, wifi_event_handler, NULL, NULL));

    wifi_config_t wifi_config = {0};
    strlcpy((char *)wifi_config.sta.ssid, CONFIG_INTERCOM_WIFI_SSID, sizeof(wifi_config.sta.ssid));
    strlcpy((char *)wifi_config.sta.password, CONFIG_INTERCOM_WIFI_PASSWORD, sizeof(wifi_config.sta.password));
    wifi_config.sta.threshold.authmode = WIFI_AUTH_WPA2_PSK;

    ESP_ERROR_CHECK(esp_wifi_set_mode(WIFI_MODE_STA));
    ESP_ERROR_CHECK(esp_wifi_set_config(WIFI_IF_STA, &wifi_config));
    ESP_ERROR_CHECK(esp_wifi_start());
}

static esp_err_t resolve_server(struct sockaddr_storage *out, socklen_t *out_len, uint16_t port)
{
    char port_text[8];
    snprintf(port_text, sizeof(port_text), "%u", port);
    struct addrinfo hints = {
        .ai_family = AF_INET,
        .ai_socktype = SOCK_DGRAM,
    };
    struct addrinfo *res = NULL;
    int rc = getaddrinfo(CONFIG_INTERCOM_SERVER_HOST, port_text, &hints, &res);
    if (rc != 0 || !res) {
        return ESP_FAIL;
    }
    memcpy(out, res->ai_addr, res->ai_addrlen);
    *out_len = res->ai_addrlen;
    freeaddrinfo(res);
    return ESP_OK;
}

static void websocket_start(void)
{
    char uri[160];
    snprintf(uri, sizeof(uri), "ws://%s:%d", CONFIG_INTERCOM_SERVER_HOST, CONFIG_INTERCOM_CONTROL_PORT);
    esp_websocket_client_config_t config = {
        .uri = uri,
        .reconnect_timeout_ms = 2000,
        .network_timeout_ms = 5000,
        .task_core_id_set = true,
        .task_core_id = task_core(CONFIG_INTERCOM_NET_TASK_CORE),
        .task_prio = 5,
        .task_name = "ic_ws",
        .task_stack = 6144,
        .buffer_size = 2048,
        .keep_alive_enable = true,
        .keep_alive_idle = 10,
        .keep_alive_interval = 5,
        .keep_alive_count = 3,
    };
    control_connected_set(false);
    s_ws_client = esp_websocket_client_init(&config);
    ESP_ERROR_CHECK(esp_websocket_register_events(s_ws_client, WEBSOCKET_EVENT_ANY, websocket_event_handler, NULL));
    ESP_ERROR_CHECK(esp_websocket_client_start(s_ws_client));
}

static esp_err_t i2c_init(void)
{
    i2c_master_bus_config_t bus_config = {
        .i2c_port = I2C_NUM_0,
        .sda_io_num = CONFIG_INTERCOM_I2C_SDA_GPIO,
        .scl_io_num = CONFIG_INTERCOM_I2C_SCL_GPIO,
        .clk_source = I2C_CLK_SRC_DEFAULT,
        .glitch_ignore_cnt = 7,
        .flags = {
            .enable_internal_pullup = true,
        },
    };
    return i2c_new_master_bus(&bus_config, &s_i2c_bus);
}

static bool audio_hw_i2c_probe_addr(uint8_t address)
{
    if (!s_i2c_bus) {
        return false;
    }
    return i2c_master_probe(s_i2c_bus, address, 60) == ESP_OK;
}

static esp_err_t audio_hw_probe_codec(void)
{
    bool found_any = false;
    char devices[128] = {0};
    size_t used = 0;
    for (uint8_t address = 0x08; address <= 0x77; address++) {
        if (!audio_hw_i2c_probe_addr(address)) {
            continue;
        }
        found_any = true;
        if (used + 6 < sizeof(devices)) {
            int written = snprintf(devices + used, sizeof(devices) - used, "%s0x%02x", used ? "," : "", address);
            if (written > 0) {
                used += (size_t)written < sizeof(devices) - used ? (size_t)written : sizeof(devices) - used - 1;
            }
        }
    }

    ESP_LOGW(TAG, "I2C probe devices: %s", found_any ? devices : "none");
    bool es8388_present = audio_hw_i2c_probe_addr(ES8388_ADDR);
    bool ac101_present = audio_hw_i2c_probe_addr(AC101_ADDR);
    if (es8388_present) {
        ESP_LOGI(TAG, "codec probe: device answered at 0x%02x, expected ES8388 address", ES8388_ADDR);
    }
    if (ac101_present) {
        ESP_LOGE(TAG,
                 "codec probe: device answered at 0x%02x, common AC101 address; this firmware currently supports "
                 "ES8388 only",
                 AC101_ADDR);
    }
    if (!es8388_present) {
        ESP_LOGE(TAG,
                 "codec probe: no device answered at ES8388 address 0x%02x; check whether this is an AC101 board, "
                 "a different board revision, or an I2C wiring/pin issue",
                 ES8388_ADDR);
        return ESP_ERR_NOT_FOUND;
    }
    if (ac101_present) {
        ESP_LOGW(TAG,
                 "codec probe: both ES8388 and AC101 addresses responded; continuing with ES8388 init but board "
                 "identity should be verified visually");
    }
    return ESP_OK;
}

static esp_err_t audio_hw_apply_audio_config(const esp32_audio_config_t *config)
{
    if (!s_i2c_codec_dev || !config) {
        return ESP_ERR_INVALID_STATE;
    }
    int output_volume = config->speaker_software_gain_percent;
    if (output_volume > 100) {
        output_volume = 100;
    }
    uint8_t pga = config->mic_pga_gain_db / 3;
    if (pga > 0x0f) {
        pga = 0x0f;
    }
    uint8_t pga_reg = (uint8_t)((pga << 4) | pga);
    uint8_t out_volume = output_volume == 0 ? ES8388_OUTPUT_VOLUME_MUTE : ES8388_OUTPUT_VOLUME_0DB;

    ESP_RETURN_ON_ERROR(audio_hw_write_es8388_reg(ES8388_ADCCONTROL1, pga_reg, "ADCCONTROL1 mic PGA"),
                        TAG,
                        "set ES8388 mic PGA");
    ESP_RETURN_ON_ERROR(audio_hw_write_es8388_reg(ES8388_ADCCONTROL2,
                                                  es8388_adc_input_value(config->adc_input),
                                                  "ADCCONTROL2 ADC input"),
                        TAG,
                        "set ES8388 ADC input");
    ESP_RETURN_ON_ERROR(audio_hw_write_es8388_reg(ES8388_DACCONTROL24, out_volume, "DACCONTROL24 LOUT1"),
                        TAG,
                        "set LOUT1 volume");
    ESP_RETURN_ON_ERROR(audio_hw_write_es8388_reg(ES8388_DACCONTROL25, out_volume, "DACCONTROL25 ROUT1"),
                        TAG,
                        "set ROUT1 volume");
    ESP_RETURN_ON_ERROR(audio_hw_write_es8388_reg(ES8388_DACCONTROL26, out_volume, "DACCONTROL26 LOUT2"),
                        TAG,
                        "set LOUT2 volume");
    ESP_RETURN_ON_ERROR(audio_hw_write_es8388_reg(ES8388_DACCONTROL27, out_volume, "DACCONTROL27 ROUT2"),
                        TAG,
                        "set ROUT2 volume");
    if (config->sidetone_mode == ESP32_SIDETONE_CODEC_BYPASS) {
        ESP_LOGW(TAG, "codec-bypass sidetone is disabled in legacy bring-up; use firmware sidetone instead");
    }
    if (config->alc_enabled || config->noise_gate_enabled) {
        ESP_LOGW(TAG, "legacy ES8388 bring-up ignores ALC/noise-gate register overrides for now");
    }
    ESP_LOGI(TAG,
             "applied legacy ES8388 audio config: output_volume=%d driver_volume=0x%02x input_gain=%u dB "
             "mic_pga_reg=0x%02x mic_software_gain=%u%% "
             "capture_channel=%s high_pass=%s sidetone=%s",
             output_volume,
             out_volume,
             config->mic_pga_gain_db,
             pga_reg,
             config->mic_software_gain_percent,
             capture_channel_name(config->capture_channel),
             config->high_pass_enabled ? "on" : "off",
             sidetone_mode_name(config->sidetone_mode));
    return ESP_OK;
}

static esp_err_t audio_hw_write_es8388_reg(int reg, int value, const char *name)
{
    if (!s_i2c_codec_dev) {
        return ESP_ERR_INVALID_STATE;
    }
    uint8_t data[2] = {(uint8_t)(reg & 0xff), (uint8_t)(value & 0xff)};
    esp_err_t ret = i2c_master_transmit(s_i2c_codec_dev, data, sizeof(data), 100);
    if (ret != ESP_OK) {
        ESP_LOGW(TAG,
                 "failed to write %s reg 0x%02x = 0x%02x: %s",
                 name,
                 reg,
                 value,
                 esp_err_to_name(ret));
        return ret;
    }
    return ESP_OK;
}

static esp_err_t audio_hw_read_es8388_reg_value(int reg, int *value, const char *name)
{
    if (!s_i2c_codec_dev || !value) {
        return ESP_ERR_INVALID_STATE;
    }
    uint8_t reg_addr = (uint8_t)(reg & 0xff);
    uint8_t data = 0;
    esp_err_t ret = i2c_master_transmit_receive(s_i2c_codec_dev, &reg_addr, 1, &data, 1, 100);
    if (ret != ESP_OK) {
        ESP_LOGW(TAG, "failed to read %s reg 0x%02x: %s", name, reg, esp_err_to_name(ret));
        return ret;
    }
    *value = data;
    return ESP_OK;
}

static esp_err_t audio_hw_update_es8388_reg_bits(int reg, int clear_mask, int set_bits, const char *name)
{
    int value = 0;
    ESP_RETURN_ON_ERROR(audio_hw_read_es8388_reg_value(reg, &value, name), TAG, "read ES8388 register");
    value = (value & ~clear_mask) | set_bits;
    return audio_hw_write_es8388_reg(reg, value, name);
}

static void audio_hw_log_es8388_reg(int reg, const char *name)
{
    int value = 0;
    esp_err_t ret = audio_hw_read_es8388_reg_value(reg, &value, name);
    if (ret != ESP_OK) {
        return;
    }
    ESP_LOGI(TAG, "  %s reg 0x%02x = 0x%02x", name, reg, value & 0xff);
}

static esp_err_t audio_hw_set_pa_gpio_level(bool high)
{
#if CONFIG_INTERCOM_PA_ENABLE_GPIO >= 0
    if (!GPIO_IS_VALID_OUTPUT_GPIO(CONFIG_INTERCOM_PA_ENABLE_GPIO)) {
        ESP_LOGW(TAG, "PA GPIO %d cannot be driven as an output", CONFIG_INTERCOM_PA_ENABLE_GPIO);
        return ESP_ERR_INVALID_ARG;
    }
    ESP_RETURN_ON_ERROR(gpio_set_direction((gpio_num_t)CONFIG_INTERCOM_PA_ENABLE_GPIO, GPIO_MODE_OUTPUT),
                        TAG,
                        "configure diagnostic PA GPIO");
    ESP_RETURN_ON_ERROR(gpio_set_level((gpio_num_t)CONFIG_INTERCOM_PA_ENABLE_GPIO, high ? 1 : 0),
                        TAG,
                        "set diagnostic PA GPIO");
    ESP_LOGW(TAG, "diagnostic PA GPIO %d forced %s", CONFIG_INTERCOM_PA_ENABLE_GPIO, high ? "high" : "low");
    return ESP_OK;
#else
    (void)high;
    ESP_LOGW(TAG, "diagnostic PA GPIO disabled");
    return ESP_OK;
#endif
}

static esp_err_t audio_hw_apply_output_registers(const char *label,
                                                 uint8_t dac_power,
                                                 uint8_t out1_volume,
                                                 uint8_t out2_volume,
                                                 bool log_readback)
{
    if (!s_i2c_codec_dev) {
        return ESP_ERR_INVALID_STATE;
    }

    ESP_LOGI(TAG,
             "applying ES8388 output profile: %s DACPOWER=0x%02x OUT1=0x%02x OUT2=0x%02x",
             label ? label : "custom",
             dac_power,
             out1_volume,
             out2_volume);
    ESP_RETURN_ON_ERROR(audio_hw_write_es8388_reg(ES8388_DACPOWER, dac_power, "DACPOWER"),
                        TAG,
                        "set ES8388 output power route");
    ESP_RETURN_ON_ERROR(audio_hw_write_es8388_reg(ES8388_DACCONTROL24, out1_volume, "DACCONTROL24 LOUT1"),
                        TAG,
                        "set LOUT1 route volume");
    ESP_RETURN_ON_ERROR(audio_hw_write_es8388_reg(ES8388_DACCONTROL25, out1_volume, "DACCONTROL25 ROUT1"),
                        TAG,
                        "set ROUT1 route volume");
    ESP_RETURN_ON_ERROR(audio_hw_write_es8388_reg(ES8388_DACCONTROL26, out2_volume, "DACCONTROL26 LOUT2"),
                        TAG,
                        "set LOUT2 route volume");
    ESP_RETURN_ON_ERROR(audio_hw_write_es8388_reg(ES8388_DACCONTROL27, out2_volume, "DACCONTROL27 ROUT2"),
                        TAG,
                        "set ROUT2 route volume");

    if (log_readback) {
        ESP_LOGI(TAG, "ES8388 output profile readback for %s:", label ? label : "custom");
        audio_hw_log_es8388_reg(ES8388_DACPOWER, "DACPOWER");
        audio_hw_log_es8388_reg(ES8388_DACCONTROL24, "DACCONTROL24 LOUT1");
        audio_hw_log_es8388_reg(ES8388_DACCONTROL25, "DACCONTROL25 ROUT1");
        audio_hw_log_es8388_reg(ES8388_DACCONTROL26, "DACCONTROL26 LOUT2");
        audio_hw_log_es8388_reg(ES8388_DACCONTROL27, "DACCONTROL27 ROUT2");
        audio_hw_log_es8388_reg(ES8388_DACCONTROL3, "DACCONTROL3 mute");
    }
    return ESP_OK;
}

static esp_err_t audio_hw_apply_output_route(esp32_output_route_t route, bool log_readback)
{
    uint8_t dac_power = ES8388_DACPOWER_ENABLE_ALL_OUTPUTS;
    uint8_t out1_volume = ES8388_OUTPUT_VOLUME_0DB;
    uint8_t out2_volume = ES8388_OUTPUT_VOLUME_0DB;
    switch (route) {
    case ESP32_OUTPUT_ROUTE_OUT1:
        dac_power = ES8388_DACPOWER_ENABLE_OUT1;
        out2_volume = ES8388_OUTPUT_VOLUME_MUTE;
        break;
    case ESP32_OUTPUT_ROUTE_OUT2:
        dac_power = ES8388_DACPOWER_ENABLE_OUT2;
        out1_volume = ES8388_OUTPUT_VOLUME_MUTE;
        break;
    case ESP32_OUTPUT_ROUTE_BOTH:
    default:
        break;
    }

    return audio_hw_apply_output_registers(output_route_name(route),
                                           dac_power,
                                           out1_volume,
                                           out2_volume,
                                           log_readback);
}

static esp_err_t audio_hw_init(void)
{
    if (!s_i2c_bus) {
        return ESP_ERR_INVALID_STATE;
    }

    if (!s_i2c_codec_dev) {
        i2c_device_config_t dev_cfg = {
            .dev_addr_length = I2C_ADDR_BIT_LEN_7,
            .device_address = ES8388_ADDR,
            .scl_speed_hz = 100000,
        };
        ESP_RETURN_ON_ERROR(i2c_master_bus_add_device(s_i2c_bus, &dev_cfg, &s_i2c_codec_dev),
                            TAG,
                            "add ES8388 I2C device");
    }

    ESP_LOGI(TAG, "initializing ES8388 codec with legacy register sequence at I2C address 0x%02x", ES8388_ADDR);
    s_audio_hw_ready = false;
    int fmt = s_i2s_options.msb_format ? 1 : 0;

    ESP_RETURN_ON_ERROR(audio_hw_write_es8388_reg(ES8388_DACCONTROL3,
                                                  ES8388_DACCONTROL3_DAC_MUTE,
                                                  "DACCONTROL3 mute"),
                        TAG,
                        "mute ES8388 during init");
    codec_output_mute_state_set(true);
    ESP_RETURN_ON_ERROR(audio_hw_write_es8388_reg(ES8388_CONTROL1, 0x12, "CONTROL1"), TAG, "ES8388 CONTROL1");
    ESP_RETURN_ON_ERROR(audio_hw_write_es8388_reg(ES8388_CONTROL2, 0x50, "CONTROL2"), TAG, "ES8388 CONTROL2");
    ESP_RETURN_ON_ERROR(audio_hw_write_es8388_reg(ES8388_CHIPPOWER, 0x00, "CHIPPOWER"), TAG, "ES8388 CHIPPOWER");
    ESP_RETURN_ON_ERROR(audio_hw_write_es8388_reg(ES8388_MASTERMODE, 0x00, "MASTERMODE"), TAG, "ES8388 slave mode");
    ESP_RETURN_ON_ERROR(audio_hw_write_es8388_reg(ES8388_DACPOWER,
                                                  ES8388_DACPOWER_DISABLE_OUTPUTS,
                                                  "DACPOWER"),
                        TAG,
                        "disable ES8388 outputs during init");

    ESP_RETURN_ON_ERROR(audio_hw_write_es8388_reg(ES8388_DACCONTROL1, 0x18 | (fmt << 1), "DACCONTROL1"),
                        TAG,
                        "set ES8388 DAC format");
    ESP_RETURN_ON_ERROR(audio_hw_write_es8388_reg(ES8388_DACCONTROL2, 0x02, "DACCONTROL2"), TAG, "ES8388 DAC ratio");
    ESP_RETURN_ON_ERROR(audio_hw_write_es8388_reg(ES8388_DACCONTROL4, 0x00, "DACCONTROL4 left DAC volume"),
                        TAG,
                        "set left DAC volume");
    ESP_RETURN_ON_ERROR(audio_hw_write_es8388_reg(ES8388_DACCONTROL5, 0x00, "DACCONTROL5 right DAC volume"),
                        TAG,
                        "set right DAC volume");
    ESP_RETURN_ON_ERROR(audio_hw_write_es8388_reg(ES8388_DACCONTROL16,
                                                  ES8388_MIXSEL_LINE2,
                                                  "DACCONTROL16 mixer select"),
                        TAG,
                        "set Audio Kit mixer select");
    ESP_RETURN_ON_ERROR(audio_hw_write_es8388_reg(ES8388_DACCONTROL17, ES8388_MIXER_DAC_0DB, "DACCONTROL17"),
                        TAG,
                        "set left DAC mixer");
    ESP_RETURN_ON_ERROR(audio_hw_write_es8388_reg(ES8388_DACCONTROL18, 0x28, "DACCONTROL18"), TAG, "ES8388 mixer");
    ESP_RETURN_ON_ERROR(audio_hw_write_es8388_reg(ES8388_DACCONTROL19, 0x28, "DACCONTROL19"), TAG, "ES8388 mixer");
    ESP_RETURN_ON_ERROR(audio_hw_write_es8388_reg(ES8388_DACCONTROL20, ES8388_MIXER_DAC_0DB, "DACCONTROL20"),
                        TAG,
                        "set right DAC mixer");
    ESP_RETURN_ON_ERROR(audio_hw_write_es8388_reg(ES8388_DACCONTROL21, ES8388_DAC_LRCK_SHARED, "DACCONTROL21"),
                        TAG,
                        "set DAC LRCK");
    ESP_RETURN_ON_ERROR(audio_hw_write_es8388_reg(ES8388_DACCONTROL23, 0x00, "DACCONTROL23"), TAG, "ES8388 VROI");

    ESP_RETURN_ON_ERROR(audio_hw_write_es8388_reg(ES8388_ADCPOWER,
                                                  ES8388_ADCPOWER_POWER_DOWN,
                                                  "ADCPOWER"),
                        TAG,
                        "power down ES8388 ADC during init");
    ESP_RETURN_ON_ERROR(audio_hw_write_es8388_reg(ES8388_ADCCONTROL3, 0x02, "ADCCONTROL3"),
                        TAG,
                        "set ES8388 ADC mono/stereo");
    ESP_RETURN_ON_ERROR(audio_hw_write_es8388_reg(ES8388_ADCCONTROL4, 0x0c | fmt, "ADCCONTROL4"),
                        TAG,
                        "set ES8388 ADC format");
    ESP_RETURN_ON_ERROR(audio_hw_write_es8388_reg(ES8388_ADCCONTROL5, 0x02, "ADCCONTROL5"),
                        TAG,
                        "set ES8388 ADC ratio");
    ESP_RETURN_ON_ERROR(audio_hw_write_es8388_reg(ES8388_ADCCONTROL8, 0x00, "ADCCONTROL8 left ADC volume"),
                        TAG,
                        "set left ADC volume");
    ESP_RETURN_ON_ERROR(audio_hw_write_es8388_reg(ES8388_ADCCONTROL9, 0x00, "ADCCONTROL9 right ADC volume"),
                        TAG,
                        "set right ADC volume");
    ESP_RETURN_ON_ERROR(audio_hw_write_es8388_reg(ES8388_ADCCONTROL10, 0xe2, "ADCCONTROL10"),
                        TAG,
                        "ES8388 ADC ALC");
    ESP_RETURN_ON_ERROR(audio_hw_write_es8388_reg(ES8388_ADCCONTROL11, 0xa0, "ADCCONTROL11"),
                        TAG,
                        "ES8388 ADC ALC");
    ESP_RETURN_ON_ERROR(audio_hw_write_es8388_reg(ES8388_ADCCONTROL12, 0x12, "ADCCONTROL12"),
                        TAG,
                        "ES8388 ADC ALC");
    ESP_RETURN_ON_ERROR(audio_hw_write_es8388_reg(ES8388_ADCCONTROL13, 0x06, "ADCCONTROL13"),
                        TAG,
                        "ES8388 ADC ALC");
    ESP_RETURN_ON_ERROR(audio_hw_write_es8388_reg(ES8388_ADCCONTROL14, 0xc3, "ADCCONTROL14"),
                        TAG,
                        "ES8388 ADC noise gate");

    esp32_audio_config_t audio = audio_config_snapshot();
    ESP_RETURN_ON_ERROR(audio_hw_apply_output_route(ESP32_OUTPUT_ROUTE_BOTH, false),
                        TAG,
                        "enable ES8388 outputs");
    ESP_RETURN_ON_ERROR(audio_hw_apply_audio_config(&audio), TAG, "apply ES8388 config");
    ESP_RETURN_ON_ERROR(audio_hw_write_es8388_reg(ES8388_ADCPOWER,
                                                  ES8388_ADCPOWER_ENABLE_ADC,
                                                  "ADCPOWER"),
                        TAG,
                        "enable ES8388 ADC");
    ESP_RETURN_ON_ERROR(audio_hw_set_pa_gpio_level(s_i2s_options.pa_active_high), TAG, "enable board output gate");

    s_audio_hw_ready = true;
    ESP_LOGI(TAG,
             "ES8388 opened through legacy I2S/register path at %u Hz, %u-bit, stereo, %s I2S",
             ESP32_AUDIO_HW_SAMPLE_RATE,
             ESP32_AUDIO_HW_BITS_PER_SAMPLE,
             i2s_format_name());
    es8388_dump_registers("after legacy init", &audio, false);
    return ESP_OK;
}

static esp_err_t audio_hw_write(const int16_t *stereo_frame, size_t bytes)
{
    if (!s_audio_hw_ready || !stereo_frame) {
        return ESP_ERR_INVALID_STATE;
    }
    int64_t start_us = esp_timer_get_time();
    int64_t previous_start_us = s_audio_hw_last_write_start_us;
    s_audio_hw_last_write_start_us = start_us;
    size_t bytes_written = 0;
    esp_err_t err = i2s_write(I2S_NUM_0, stereo_frame, bytes, &bytes_written, portMAX_DELAY);
    int64_t elapsed_us = esp_timer_get_time() - start_us;
    int64_t gap_us = previous_start_us > 0 ? start_us - previous_start_us : 0;
    bool gap_warning = gap_us > PLAYBACK_I2S_GAP_WARN_US;
    bool slow_warning = elapsed_us > PLAYBACK_I2S_SLOW_WRITE_WARN_US;
    bool short_warning = err != ESP_OK || bytes_written != bytes;
    if (gap_warning) {
        s_audio_hw_write_gap_warnings++;
    }
    if (slow_warning) {
        s_audio_hw_write_slow_warnings++;
    }
    if (short_warning) {
        s_audio_hw_write_short_warnings++;
    }
    if ((gap_warning || slow_warning || short_warning) &&
        start_us - s_audio_hw_last_write_warn_us >= PLAYBACK_I2S_WARN_THROTTLE_US) {
        ESP_LOGW(TAG,
                 "I2S playback timing warning: gap=%" PRId64 "us write=%" PRId64
                 "us bytes=%u/%u err=%s counts gap=%" PRIu32 " slow=%" PRIu32 " short=%" PRIu32,
                 gap_us,
                 elapsed_us,
                 (unsigned)bytes_written,
                 (unsigned)bytes,
                 esp_err_to_name(err),
                 s_audio_hw_write_gap_warnings,
                 s_audio_hw_write_slow_warnings,
                 s_audio_hw_write_short_warnings);
        s_audio_hw_last_write_warn_us = start_us;
    }
    if (err != ESP_OK) {
        return err;
    }
    return bytes_written == bytes ? ESP_OK : ESP_FAIL;
}

static esp_err_t audio_hw_read(int16_t *stereo_frame, size_t bytes)
{
    if (!s_audio_hw_ready || !stereo_frame) {
        return ESP_ERR_INVALID_STATE;
    }
    size_t bytes_read = 0;
    esp_err_t err = i2s_read(I2S_NUM_0, stereo_frame, bytes, &bytes_read, portMAX_DELAY);
    if (err != ESP_OK) {
        return err;
    }
    return bytes_read == bytes ? ESP_OK : ESP_FAIL;
}

static esp_err_t es8388_set_playback_mute(bool mute)
{
    if (!s_i2c_codec_dev) {
        return ESP_ERR_INVALID_STATE;
    }
    return audio_hw_update_es8388_reg_bits(ES8388_DACCONTROL3,
                                           ES8388_DACCONTROL3_DAC_MUTE,
                                           mute ? ES8388_DACCONTROL3_DAC_MUTE : 0,
                                           "DACCONTROL3 mute");
}


static void es8388_dump_registers(const char *reason, const esp32_audio_config_t *config, bool warn_on_mismatch)
{
    (void)config;
    (void)warn_on_mismatch;
    if (!s_i2c_codec_dev) {
        ESP_LOGW(TAG, "ES8388 register dump skipped before codec init: %s", reason ? reason : "unknown");
        return;
    }
    ESP_LOGI(TAG, "ES8388 register dump: %s", reason ? reason : "unknown");
    const uint8_t regs[] = {
        ES8388_CONTROL1,      ES8388_CONTROL2,      ES8388_CHIPPOWER,     ES8388_ADCPOWER,
        ES8388_DACPOWER,     ES8388_MASTERMODE,    ES8388_ADCCONTROL1,   ES8388_ADCCONTROL2,
        ES8388_ADCCONTROL3,  ES8388_ADCCONTROL4,   ES8388_ADCCONTROL5,   ES8388_ADCCONTROL8,
        ES8388_ADCCONTROL9,  ES8388_ADCCONTROL10,  ES8388_ADCCONTROL11,  ES8388_ADCCONTROL12,
        ES8388_ADCCONTROL13, ES8388_ADCCONTROL14,  ES8388_DACCONTROL1,   ES8388_DACCONTROL2,
        ES8388_DACCONTROL3,  ES8388_DACCONTROL4,   ES8388_DACCONTROL5,   ES8388_DACCONTROL16,
        ES8388_DACCONTROL17, ES8388_DACCONTROL18,  ES8388_DACCONTROL19,  ES8388_DACCONTROL20,
        ES8388_DACCONTROL21, ES8388_DACCONTROL23,  ES8388_DACCONTROL24,  ES8388_DACCONTROL25,
        ES8388_DACCONTROL26, ES8388_DACCONTROL27,
    };
    for (size_t i = 0; i < sizeof(regs) / sizeof(regs[0]); i++) {
        audio_hw_log_es8388_reg(regs[i], "ES8388");
    }
}

static esp_err_t es8388_apply_audio_config(const esp32_audio_config_t *config)
{
    return audio_hw_apply_audio_config(config);
}

static void request_audio_config_apply(const esp32_audio_config_t *config)
{
    if (!config || !s_audio_config_queue) {
        return;
    }
    if (xQueueOverwrite(s_audio_config_queue, config) != pdTRUE) {
        ESP_LOGW(TAG, "failed to queue audio config apply");
    }
}

static esp_err_t audio_i2s_set_codec(ic_codec_t codec)
{
    ESP_LOGI(TAG,
             "network codec=%s selected; ES8388 hardware remains fixed at %u Hz stereo",
             codec_wire(codec),
             ESP32_AUDIO_HW_SAMPLE_RATE);
    ic_pcm_frame_ring_clear(&s_playback_ring);
    ic_pcm_frame_ring_clear(&s_sidetone_ring);
    return ESP_OK;
}

static esp_err_t audio_i2s_init_with_options(const i2s_runtime_options_t *options)
{
    s_i2s_options = options ? *options : default_i2s_options();
    i2s_config_t i2s_config = {
        .mode = I2S_MODE_MASTER | I2S_MODE_TX | I2S_MODE_RX,
        .sample_rate = ESP32_AUDIO_HW_SAMPLE_RATE,
        .bits_per_sample = I2S_BITS_PER_SAMPLE_16BIT,
        .channel_format = I2S_CHANNEL_FMT_RIGHT_LEFT,
        .communication_format = i2s_legacy_comm_format_for(&s_i2s_options),
        .intr_alloc_flags = 0,
        .dma_desc_num = 8,
        .dma_frame_num = ESP32_AUDIO_HW_SAMPLES_PER_FRAME,
        .use_apll = false,
        .tx_desc_auto_clear = false,
        .fixed_mclk = s_i2s_options.mclk_enabled ? (int)(ESP32_AUDIO_HW_SAMPLE_RATE * 256U) : 0,
        .mclk_multiple = I2S_MCLK_MULTIPLE_256,
        .bits_per_chan = audio_i2s_bits_per_chan_for(&s_i2s_options),
    };
    i2s_pin_config_t pin_config = {
        .mck_io_num = s_i2s_options.mclk_enabled ? CONFIG_INTERCOM_I2S_MCLK_GPIO : I2S_PIN_NO_CHANGE,
        .bck_io_num = CONFIG_INTERCOM_I2S_BCLK_GPIO,
        .ws_io_num = (int)audio_i2s_ws_gpio_for(&s_i2s_options),
        .data_out_num = (int)audio_i2s_dout_gpio_for(&s_i2s_options),
        .data_in_num = CONFIG_INTERCOM_I2S_DIN_GPIO,
    };

    ESP_LOGI(TAG,
             "initializing legacy I2S hardware sample_rate=%u"
             " Hz format=%s data_width=%s slot_width=%s mclk=%s pa=%s pins=%s bclk=%d ws=%d dout=%d din=%d mclk_gpio=%d",
             ESP32_AUDIO_HW_SAMPLE_RATE,
             i2s_format_name(),
             i2s_data_width_name(),
             i2s_slot_width_name(),
             s_i2s_options.mclk_enabled ? "on" : "off",
             s_i2s_options.pa_active_high ? "active-high" : "active-low",
             i2s_pin_profile_name(),
             CONFIG_INTERCOM_I2S_BCLK_GPIO,
             (int)audio_i2s_ws_gpio_for(&s_i2s_options),
             (int)audio_i2s_dout_gpio_for(&s_i2s_options),
             CONFIG_INTERCOM_I2S_DIN_GPIO,
             s_i2s_options.mclk_enabled ? CONFIG_INTERCOM_I2S_MCLK_GPIO : -1);
    ESP_ERROR_CHECK_WITHOUT_ABORT(i2s_driver_uninstall(I2S_NUM_0));
    ESP_RETURN_ON_ERROR(i2s_driver_install(I2S_NUM_0, &i2s_config, 0, NULL), TAG, "legacy i2s install");
    ESP_RETURN_ON_ERROR(i2s_set_pin(I2S_NUM_0, &pin_config), TAG, "legacy i2s set pins");
    ESP_RETURN_ON_ERROR(i2s_set_clk(I2S_NUM_0,
                                    ESP32_AUDIO_HW_SAMPLE_RATE,
                                    I2S_BITS_PER_SAMPLE_16BIT,
                                    I2S_CHANNEL_STEREO),
                        TAG,
                        "legacy i2s set clock");
    ESP_RETURN_ON_ERROR(i2s_zero_dma_buffer(I2S_NUM_0), TAG, "legacy i2s clear dma");
    audio_i2s_log_channel_info("tx/rx");
    return ESP_OK;
}

static esp_err_t audio_i2s_init(void)
{
    i2s_runtime_options_t options = default_i2s_options();
    return audio_i2s_init_with_options(&options);
}

static void buttons_init(void)
{
    uint64_t pullup_mask = 0;
    uint64_t floating_mask = 0;
    int ptt_gpio = CONFIG_INTERCOM_PTT_GPIO;
    if (ptt_gpio >= 0) {
        if (gpio_supports_internal_pullup(ptt_gpio)) {
            pullup_mask |= 1ULL << (unsigned)ptt_gpio;
        } else {
            floating_mask |= 1ULL << (unsigned)ptt_gpio;
        }
    }
    if (s_reply_button.gpio >= 0) {
        if (gpio_supports_internal_pullup(s_reply_button.gpio)) {
            pullup_mask |= 1ULL << (unsigned)s_reply_button.gpio;
        } else {
            floating_mask |= 1ULL << (unsigned)s_reply_button.gpio;
        }
    }
    for (size_t i = 0; i < sizeof(s_dedicated_buttons) / sizeof(s_dedicated_buttons[0]); i++) {
        if (physical_button_enabled(&s_dedicated_buttons[i])) {
            int button_gpio = s_dedicated_buttons[i].gpio;
            if (gpio_supports_internal_pullup(button_gpio)) {
                pullup_mask |= 1ULL << (unsigned)button_gpio;
            } else {
                floating_mask |= 1ULL << (unsigned)button_gpio;
            }
        }
    }
    if (!pullup_mask && !floating_mask) {
        return;
    }
    if (pullup_mask) {
        gpio_config_t cfg = {
            .pin_bit_mask = pullup_mask,
            .mode = GPIO_MODE_INPUT,
            .pull_up_en = GPIO_PULLUP_ENABLE,
            .pull_down_en = GPIO_PULLDOWN_DISABLE,
            .intr_type = GPIO_INTR_DISABLE,
        };
        ESP_ERROR_CHECK(gpio_config(&cfg));
    }
    if (floating_mask) {
        ESP_LOGW(TAG, "button GPIO mask 0x%" PRIx64 " has no internal pull-up; use an external pull-up", floating_mask);
        gpio_config_t cfg = {
            .pin_bit_mask = floating_mask,
            .mode = GPIO_MODE_INPUT,
            .pull_up_en = GPIO_PULLUP_DISABLE,
            .pull_down_en = GPIO_PULLDOWN_DISABLE,
            .intr_type = GPIO_INTR_DISABLE,
        };
        ESP_ERROR_CHECK(gpio_config(&cfg));
    }
}

static void target_add(tx_target_t *targets, size_t *count, size_t max, ic_target_kind_t kind, uint16_t id)
{
    if (id == 0) {
        return;
    }
    for (size_t i = 0; i < *count; i++) {
        if (targets[i].kind == kind && targets[i].id == id) {
            return;
        }
    }
    if (*count < max) {
        targets[*count] = (tx_target_t){.kind = kind, .id = id};
        (*count)++;
    }
}

static size_t build_tx_targets(tx_target_t *targets, size_t max)
{
    size_t count = 0;
    xSemaphoreTake(s_config_lock, portMAX_DELAY);
    bool regular = s_config.talk_mode == TALK_MODE_OPEN ||
                   (s_config.talk_mode == TALK_MODE_PTT && s_config.regular_talk_active);
    if (regular) {
        for (size_t i = 0; i < s_config.tx.count; i++) {
            target_add(targets, &count, max, IC_TARGET_CHANNEL, s_config.tx.values[i]);
        }
    }
    for (size_t i = 0; i < s_config.button_count; i++) {
        const button_route_t *button = &s_config.buttons[i];
        if (!button->active) {
            continue;
        }
        for (size_t j = 0; j < button->tx_channels.count; j++) {
            target_add(targets, &count, max, IC_TARGET_CHANNEL, button->tx_channels.values[j]);
        }
        for (size_t j = 0; j < button->tx_users.count; j++) {
            target_add(targets, &count, max, IC_TARGET_DIRECT, button->tx_users.values[j]);
        }
    }
    xSemaphoreGive(s_config_lock);
    return count;
}

static bool enqueue_audio_payload(ic_target_kind_t kind,
                                  uint16_t target_id,
                                  ic_codec_t codec,
                                  const uint8_t *payload,
                                  uint16_t payload_len)
{
    if (!s_audio_tx_queue || s_audio_sock < 0) {
        return false;
    }
    audio_tx_packet_t *tx = &s_audio_tx_encode_packet;
    size_t written = 0;
    ic_audio_packet_t packet = {
        .user_id = current_user_id(),
        .target_kind = kind,
        .target_id = target_id,
        .codec = codec,
        .seq = s_audio_seq++,
        .timestamp_ms = (uint32_t)(esp_timer_get_time() / 1000),
        .payload = payload,
        .payload_len = payload_len,
    };
    if (!ic_encode_audio_packet(&packet, tx->bytes, sizeof(tx->bytes), &written)) {
        return false;
    }
    tx->len = (uint16_t)written;
    if (xQueueSend(s_audio_tx_queue, tx, 0) != pdTRUE) {
        s_audio_tx_queue_drops++;
        return false;
    }
    return true;
}

static bool build_encoded_audio_payload(ic_codec_t codec,
                                        const int16_t *pcm,
                                        size_t samples,
                                        uint8_t *out,
                                        size_t out_len,
                                        uint16_t *payload_len)
{
    if (!pcm || !out || !payload_len) {
        return false;
    }
#if CONFIG_INTERCOM_OPUS
    if (codec == IC_CODEC_OPUS) {
        if (!s_opus_encoder || samples != IC_OPUS_SAMPLES_PER_FRAME) {
            return false;
        }
        int encoded = opus_encode(s_opus_encoder, pcm, (int)samples, out, (opus_int32)out_len);
        if (encoded < 0) {
            s_opus_encode_failures++;
            ESP_LOGW(TAG, "Opus encode failed: %d", encoded);
            return false;
        }
        *payload_len = (uint16_t)encoded;
        return true;
    }
#else
    if (codec == IC_CODEC_OPUS) {
        return false;
    }
#endif
    size_t bytes = samples * sizeof(int16_t);
    if (bytes > out_len || bytes > UINT16_MAX) {
        return false;
    }
    for (size_t i = 0; i < samples; i++) {
        out[i * 2] = (uint8_t)pcm[i];
        out[i * 2 + 1] = (uint8_t)((uint16_t)pcm[i] >> 8);
    }
    *payload_len = (uint16_t)bytes;
    return true;
}

static bool decode_packet_audio_payload(const ic_audio_packet_t *packet, ic_codec_t codec, int16_t *out)
{
    if (!packet || !out || packet->codec != codec) {
        return false;
    }
#if CONFIG_INTERCOM_OPUS
    if (codec == IC_CODEC_OPUS) {
        if (!s_opus_decoder || packet->payload_len == 0 || packet->payload_len > IC_OPUS_MAX_BYTES_PER_FRAME) {
            s_opus_decode_failures++;
            return false;
        }
        int decoded = opus_decode(s_opus_decoder,
                                  packet->payload,
                                  packet->payload_len,
                                  out,
                                  IC_OPUS_SAMPLES_PER_FRAME,
                                  0);
        if (decoded != IC_OPUS_SAMPLES_PER_FRAME) {
            s_opus_decode_failures++;
            ESP_LOGW(TAG, "Opus decode failed or returned unexpected frame: %d", decoded);
            return false;
        }
        return true;
    }
#else
    if (codec == IC_CODEC_OPUS) {
        return false;
    }
#endif
    size_t samples = codec_samples_per_frame(codec);
    size_t mono_payload_len = samples * sizeof(int16_t);
    bool stereo_payload = packet->payload_len == mono_payload_len * 2;
    if (packet->payload_len != mono_payload_len && !stereo_payload) {
        return false;
    }
    for (size_t i = 0; i < samples; i++) {
        if (stereo_payload) {
            int16_t left = read_le_i16(&packet->payload[i * 4]);
            int16_t right = read_le_i16(&packet->payload[i * 4 + 2]);
            out[i] = (int16_t)(((int)left + (int)right) / 2);
        } else {
            out[i] = read_le_i16(&packet->payload[i * 2]);
        }
    }
    return true;
}

static void drain_audio_tx_queue(int sock)
{
    if (!s_audio_tx_queue || sock < 0) {
        return;
    }
    while (xQueueReceive(s_audio_tx_queue, &s_audio_tx_send_packet, 0) == pdTRUE) {
        if (send(sock,
                 s_audio_tx_send_packet.bytes,
                 s_audio_tx_send_packet.len,
                 0) != (int)s_audio_tx_send_packet.len) {
            s_udp_tx_send_failures++;
            ESP_LOGW(TAG, "UDP audio send failed: errno=%d", errno);
        }
    }
}

static void send_registration(void)
{
    int sock = s_audio_sock;
    if (sock < 0) {
        return;
    }
    uint8_t encoded[IC_HEADER_LEN];
    size_t written = 0;
    ic_codec_t codec = runtime_codec_snapshot();
    ic_audio_packet_t packet = {
        .user_id = current_user_id(),
        .target_kind = IC_TARGET_MIXED,
        .target_id = 0,
        .codec = codec,
        .seq = s_registration_seq++,
        .timestamp_ms = 0,
        .payload = NULL,
        .payload_len = 0,
    };
    if (ic_encode_audio_packet(&packet, encoded, sizeof(encoded), &written)) {
        (void)send(sock, encoded, written, 0);
    }
}

static void udp_task(void *arg)
{
    (void)arg;
    watchdog_register_current_task("ic_udp");
    ESP_LOGI(TAG, "task ic_udp running on core %d", xPortGetCoreID());
    struct sockaddr_storage server_addr;
    socklen_t server_len = 0;

    for (;;) {
        watchdog_reset_current_task();
        EventBits_t bits =
            xEventGroupWaitBits(s_wifi_events, WIFI_CONNECTED_BIT, false, true, pdMS_TO_TICKS(1000));
        if ((bits & WIFI_CONNECTED_BIT) == 0) {
            continue;
        }
        if (resolve_server(&server_addr, &server_len, CONFIG_INTERCOM_AUDIO_PORT) != ESP_OK) {
            ESP_LOGW(TAG, "failed to resolve audio server %s", CONFIG_INTERCOM_SERVER_HOST);
            vTaskDelay(pdMS_TO_TICKS(1000));
            continue;
        }

        int sock = socket(server_addr.ss_family, SOCK_DGRAM, IPPROTO_IP);
        if (sock < 0) {
            ESP_LOGW(TAG, "socket failed: errno=%d", errno);
            vTaskDelay(pdMS_TO_TICKS(1000));
            continue;
        }
        if (connect(sock, (struct sockaddr *)&server_addr, server_len) != 0) {
            ESP_LOGW(TAG, "UDP connect failed: errno=%d", errno);
            close(sock);
            vTaskDelay(pdMS_TO_TICKS(1000));
            continue;
        }
        struct timeval timeout = {
            .tv_sec = 0,
            .tv_usec = IC_FRAME_MS * 1000,
        };
        (void)setsockopt(sock, SOL_SOCKET, SO_RCVTIMEO, &timeout, sizeof(timeout));
        (void)setsockopt(sock, SOL_SOCKET, SO_SNDTIMEO, &timeout, sizeof(timeout));

        s_audio_sock = sock;
        s_udp_have_last_seq = false;
        ESP_LOGI(TAG, "UDP audio connected to %s:%d", CONFIG_INTERCOM_SERVER_HOST, CONFIG_INTERCOM_AUDIO_PORT);
        send_registration();

        for (;;) {
            watchdog_reset_current_task();
            drain_audio_tx_queue(sock);
            int len = recv(sock, s_udp_rx_packet, sizeof(s_udp_rx_packet), 0);
            if (len <= 0) {
                if (errno == EAGAIN || errno == EWOULDBLOCK) {
                    continue;
                }
                ESP_LOGW(TAG, "UDP receive failed: errno=%d", errno);
                break;
            }
            ic_audio_packet_t packet;
            if (!ic_decode_audio_packet(s_udp_rx_packet, len, &packet)) {
                s_udp_decode_errors++;
                continue;
            }
            s_udp_rx_packets++;
            ic_codec_t codec = runtime_codec_snapshot();
            size_t packet_samples_per_frame = codec_samples_per_frame(codec);
            if (packet.target_kind != IC_TARGET_MIXED || packet.codec != codec) {
                s_udp_codec_drops++;
                continue;
            }
            if (s_udp_have_last_seq) {
                uint16_t expected = (uint16_t)(s_udp_last_seq + 1);
                if (packet.seq != expected) {
                    uint16_t gap = (uint16_t)(packet.seq - expected);
                    s_udp_sequence_gaps += gap == 0 ? 1 : gap;
                }
            }
            s_udp_have_last_seq = true;
            s_udp_last_seq = packet.seq;
            memset(s_udp_packet_frame, 0, sizeof(s_udp_packet_frame));
            if (!decode_packet_audio_payload(&packet, codec, s_udp_packet_frame)) {
                s_udp_payload_decode_errors++;
                continue;
            }
            resample_network_to_hw(s_udp_packet_frame, packet_samples_per_frame, s_udp_playback_frame);
            ic_pcm_frame_ring_push(&s_playback_ring, s_udp_playback_frame);
        }

        s_audio_sock = -1;
        close(sock);
        vTaskDelay(pdMS_TO_TICKS(1000));
    }
}

static void registration_task(void *arg)
{
    (void)arg;
    watchdog_register_current_task("ic_register");
    ESP_LOGI(TAG, "task ic_register running on core %d", xPortGetCoreID());
    int64_t last_reconnect_cue_us = 0;
    int64_t last_ping_us = 0;
    int64_t last_capture_health_us = 0;
    for (;;) {
        send_registration();
        int64_t now_us = esp_timer_get_time();
        if (control_connected_snapshot()) {
            if (control_take_hello_pending()) {
                send_hello();
                last_ping_us = now_us;
                last_capture_health_us = now_us;
            }
            if (control_take_startup_config_pending()) {
                send_startup_config();
            }
            if (websocket_ready() && (last_ping_us == 0 || now_us - last_ping_us >= CONTROL_PING_INTERVAL_US)) {
                send_ping();
                last_ping_us = now_us;
            }
            if (websocket_ready() &&
                (last_capture_health_us == 0 ||
                 now_us - last_capture_health_us >= CONTROL_CAPTURE_HEALTH_SEND_INTERVAL_US)) {
                send_capture_health();
                last_capture_health_us = now_us;
            }
            last_reconnect_cue_us = 0;
        } else {
            last_ping_us = 0;
            last_capture_health_us = 0;
            if (last_reconnect_cue_us == 0) {
                last_reconnect_cue_us = now_us;
            } else if (now_us - last_reconnect_cue_us >= AUDIO_CUE_RECONNECT_INTERVAL_US) {
                audio_cue_start(AUDIO_CUE_RECONNECTING);
                last_reconnect_cue_us = now_us;
            }
        }
        watchdog_reset_current_task();
        vTaskDelay(pdMS_TO_TICKS(1000));
    }
}

static void codec_mute_task(void *arg)
{
    (void)arg;
    ESP_LOGI(TAG, "task ic_codec_mute running on core %d", xPortGetCoreID());
    bool mute = true;
    for (;;) {
        if (xQueueReceive(s_codec_mute_queue, &mute, portMAX_DELAY) != pdTRUE) {
            continue;
        }
        esp_err_t err = es8388_set_playback_mute(mute);
        if (err != ESP_OK) {
            ESP_LOGW(TAG, "failed to %s ES8388 playback output", mute ? "mute" : "unmute");
        }
    }
}

static void audio_config_apply_task(void *arg)
{
    (void)arg;
    ESP_LOGI(TAG, "task ic_audio_cfg running on core %d", xPortGetCoreID());
    esp32_audio_config_t audio;
    for (;;) {
        if (xQueueReceive(s_audio_config_queue, &audio, portMAX_DELAY) != pdTRUE) {
            continue;
        }
        esp_err_t err = es8388_apply_audio_config(&audio);
        if (err != ESP_OK) {
            ESP_LOGW(TAG, "failed to apply ESP32 audio config: %s", esp_err_to_name(err));
            continue;
        }
        es8388_dump_registers("after server esp32_audio config", &audio, true);
        ESP_LOGI(TAG,
                 "audio config: server=%s input=%s pga=%u dB channel=%s hp=%s mic=%u%% speaker=%u%% notifications=%u%% alc=%s noise_gate=%s sidetone=%s firmware_gain=%u%% line_bypass=%u%% mic_bypass=%u%%",
                 audio.server_control_enabled ? "on" : "off",
                 adc_input_name(audio.adc_input),
                 audio.mic_pga_gain_db,
                 capture_channel_name(audio.capture_channel),
                 audio.high_pass_enabled ? "on" : "off",
                 audio.mic_software_gain_percent,
                 audio.speaker_software_gain_percent,
                 audio.notification_gain_percent,
                 audio.alc_enabled ? "on" : "off",
                 audio.noise_gate_enabled ? "on" : "off",
                 sidetone_mode_name(audio.sidetone_mode),
                 audio.sidetone_firmware_gain_percent,
                 audio.sidetone_codec_bypass_gain_percent,
                 audio.sidetone_mic_bypass_gain_percent);
    }
}

static void playback_task(void *arg)
{
    (void)arg;
    watchdog_register_current_task("ic_playback");
    ESP_LOGI(TAG, "task ic_playback running on core %d", xPortGetCoreID());
    bool playback_started = false;
    int16_t last_playback_sample = 0;
    playback_codec_mute_gate_t mute_gate = {0};

    for (;;) {
        esp32_audio_config_t audio = audio_config_snapshot();
        size_t samples_per_frame = ESP32_AUDIO_HW_SAMPLES_PER_FRAME;
        size_t prefill_frames = CONFIG_INTERCOM_PLAYBACK_PREFILL_FRAMES;
        if (prefill_frames > CONFIG_INTERCOM_JITTER_FRAMES) {
            prefill_frames = CONFIG_INTERCOM_JITTER_FRAMES;
        }
        bool have_playback_frame = false;
        if (!playback_started) {
            if (ic_pcm_frame_ring_count(&s_playback_ring) >= prefill_frames) {
                have_playback_frame = ic_pcm_frame_ring_pop(&s_playback_ring, s_playback_mono_frame);
                if (have_playback_frame) {
                    apply_playback_fade_in(s_playback_mono_frame, samples_per_frame);
                    playback_started = true;
                }
            }
        } else {
            have_playback_frame = ic_pcm_frame_ring_pop(&s_playback_ring, s_playback_mono_frame);
            if (!have_playback_frame) {
                playback_started = false;
            }
        }
        if (!have_playback_frame) {
            fill_playback_fade_to_silence(s_playback_mono_frame, samples_per_frame, last_playback_sample);
        }
        if (!ic_pcm_frame_ring_pop(&s_sidetone_ring, s_playback_sidetone_frame)) {
            memset(s_playback_sidetone_frame, 0, sizeof(s_playback_sidetone_frame));
        }
        bool frame_active = false;
        for (size_t i = 0; i < samples_per_frame; i++) {
            int mixed = (int)s_playback_mono_frame[i];
            if (audio.sidetone_mode == ESP32_SIDETONE_FIRMWARE) {
                mixed += (int)s_playback_sidetone_frame[i] * audio.sidetone_firmware_gain_percent / 100;
            }
            mixed += audio_cue_next_sample();
            mixed = mixed * audio.speaker_software_gain_percent / 100;
            int abs_mixed = mixed < 0 ? -mixed : mixed;
            if (abs_mixed > PLAYBACK_CODEC_ACTIVE_THRESHOLD) {
                frame_active = true;
            }
            int16_t sample = clamp_i16(mixed);
            if (sample == 0) {
                sample = playback_idle_floor_sample();
            }
            s_playback_stereo_frame[i * 2] = sample;
            s_playback_stereo_frame[i * 2 + 1] = sample;
        }
        last_playback_sample = s_playback_mono_frame[samples_per_frame - 1];
        size_t bytes_written = 0;
        size_t frame_bytes = samples_per_frame * 2 * sizeof(int16_t);
        esp_err_t err = audio_hw_write(s_playback_stereo_frame, frame_bytes);
        bytes_written = err == ESP_OK ? frame_bytes : 0;
        if (err != ESP_OK) {
            ESP_LOGW(TAG, "codec playback short write: err=%s bytes=%u/%u",
                     esp_err_to_name(err),
                     (unsigned)bytes_written,
                     (unsigned)frame_bytes);
        }
        if (!PLAYBACK_CODEC_MUTE_GATE_ENABLED || audio.sidetone_mode == ESP32_SIDETONE_CODEC_BYPASS) {
            request_codec_output_mute(false);
        } else {
            playback_codec_mute_gate_update(&mute_gate, frame_active);
        }
        watchdog_reset_current_task();
    }
}

static void capture_channel_accumulate(capture_channel_accumulator_t *channel, int sample)
{
    int abs_sample = sample < 0 ? -sample : sample;
    if (abs_sample > channel->peak_abs) {
        channel->peak_abs = abs_sample;
    }
    channel->sum += (double)sample;
    channel->sum_squares += (double)sample * (double)sample;
}

static float capture_channel_rms(const capture_channel_accumulator_t *channel, uint32_t samples)
{
    if (samples == 0) {
        return 0.0f;
    }
    return (float)(sqrt(channel->sum_squares / (double)samples) / 32768.0);
}

static float capture_channel_peak(const capture_channel_accumulator_t *channel)
{
    return (float)channel->peak_abs / 32768.0f;
}

static float capture_channel_dc_offset(const capture_channel_accumulator_t *channel, uint32_t samples)
{
    if (samples == 0) {
        return 0.0f;
    }
    return (float)((channel->sum / (double)samples) / 32768.0);
}

static void capture_health_maybe_publish(capture_health_accumulator_t *acc, int64_t now_us)
{
    if (acc->started_us == 0) {
        acc->started_us = now_us;
    }
    if (now_us - acc->started_us < CAPTURE_HEALTH_REPORT_US || acc->samples == 0) {
        return;
    }

    capture_health_report_t report = {
        .left_rms = capture_channel_rms(&acc->left, acc->samples),
        .left_peak = capture_channel_peak(&acc->left),
        .left_dc_offset = capture_channel_dc_offset(&acc->left, acc->samples),
        .right_rms = capture_channel_rms(&acc->right, acc->samples),
        .right_peak = capture_channel_peak(&acc->right),
        .right_dc_offset = capture_channel_dc_offset(&acc->right, acc->samples),
        .selected_rms = capture_channel_rms(&acc->selected, acc->samples),
        .selected_peak = capture_channel_peak(&acc->selected),
        .selected_dc_offset = capture_channel_dc_offset(&acc->selected, acc->samples),
        .raw_clipped_samples = acc->raw_clipped_samples,
        .software_clipped_samples = acc->software_clipped_samples,
        .tx_target_count = acc->tx_target_count,
        .tx_packets_sent = acc->tx_packets_sent,
        .tx_send_failures = acc->tx_send_failures,
        .ready = true,
    };

    taskENTER_CRITICAL(&s_capture_health_lock);
    s_capture_health_report = report;
    taskEXIT_CRITICAL(&s_capture_health_lock);

    memset(acc, 0, sizeof(*acc));
    acc->started_us = now_us;
}

static void diagnostic_play_tone(uint32_t sample_rate, float frequency, float amplitude, uint32_t duration_ms)
{
    uint32_t total_samples = sample_rate * duration_ms / 1000U;
    uint32_t ramp_samples = samples_for_ms_u32(sample_rate, 20);
    uint32_t sample_index = 0;
    uint32_t writes = 0;
    uint32_t bytes_total = 0;
    int peak = 0;
    while (sample_index < total_samples) {
        size_t frame_samples = ESP32_AUDIO_HW_SAMPLES_PER_FRAME;
        uint32_t remaining = total_samples - sample_index;
        if (remaining < frame_samples) {
            frame_samples = remaining;
        }
        for (size_t i = 0; i < frame_samples; i++) {
            uint32_t absolute = sample_index + (uint32_t)i;
            float envelope = 1.0f;
            if (absolute < ramp_samples) {
                envelope = smoothstep_unit((float)absolute / (float)ramp_samples);
            } else if (total_samples > absolute && total_samples - absolute < ramp_samples) {
                envelope = smoothstep_unit((float)(total_samples - absolute) / (float)ramp_samples);
            }
            float phase = 2.0f * AUDIO_CUE_PI * frequency * (float)absolute / (float)sample_rate;
            int16_t sample = clamp_i16((int)lrintf(sinf(phase) * amplitude * envelope));
            int abs_sample = abs_i16_value(sample);
            if (abs_sample > peak) {
                peak = abs_sample;
            }
            s_playback_stereo_frame[i * 2] = sample;
            s_playback_stereo_frame[i * 2 + 1] = sample;
        }
        size_t bytes_written = 0;
        size_t frame_bytes = frame_samples * 2 * sizeof(int16_t);
        esp_err_t err = audio_hw_write(s_playback_stereo_frame, frame_bytes);
        bytes_written = err == ESP_OK ? frame_bytes : 0;
        if (err != ESP_OK) {
            ESP_LOGW(TAG,
                     "diagnostic tone write failed: err=%s bytes=%u/%u",
                     esp_err_to_name(err),
                     (unsigned)bytes_written,
                     (unsigned)frame_bytes);
            break;
        }
        sample_index += (uint32_t)frame_samples;
        writes++;
        bytes_total += (uint32_t)bytes_written;
    }
    ESP_LOGI(TAG,
             "diagnostic tone wrote freq=%.1fHz duration=%ums samples=%" PRIu32 "/%" PRIu32
             " writes=%" PRIu32 " bytes=%" PRIu32 " peak=%.3f",
             (double)frequency,
             (unsigned)duration_ms,
             sample_index,
             total_samples,
             writes,
             bytes_total,
             (double)peak / 32768.0);
}

static void diagnostic_output_test(void)
{
    uint32_t pass = 0;
    const struct {
        const char *label;
        uint8_t dac_power;
        uint8_t out1_volume;
        uint8_t out2_volume;
        bool pa_high;
    } profiles[] = {
        {"Community Aux/headphone: DACPOWER 0x0c, OUT2 max, PA high",
         0x0c,
         ES8388_OUTPUT_VOLUME_MUTE,
         ES8388_OUTPUT_VOLUME_PLUS_4_5DB,
         true},
        {"Community Aux/headphone: DACPOWER 0x0c, OUT2 max, PA low",
         0x0c,
         ES8388_OUTPUT_VOLUME_MUTE,
         ES8388_OUTPUT_VOLUME_PLUS_4_5DB,
         false},
        {"Community speaker/line1: DACPOWER 0x30, OUT1 max, PA high",
         0x30,
         ES8388_OUTPUT_VOLUME_PLUS_4_5DB,
         ES8388_OUTPUT_VOLUME_MUTE,
         true},
        {"Community speaker/line1: DACPOWER 0x30, OUT1 max, PA low",
         0x30,
         ES8388_OUTPUT_VOLUME_PLUS_4_5DB,
         ES8388_OUTPUT_VOLUME_MUTE,
         false},
        {"ADF all outputs: DACPOWER 0x3c, OUT1+OUT2 max, PA high",
         ES8388_DACPOWER_ENABLE_ALL_OUTPUTS,
         ES8388_OUTPUT_VOLUME_PLUS_4_5DB,
         ES8388_OUTPUT_VOLUME_PLUS_4_5DB,
         true},
        {"ADF all outputs: DACPOWER 0x3c, OUT1+OUT2 max, PA low",
         ES8388_DACPOWER_ENABLE_ALL_OUTPUTS,
         ES8388_OUTPUT_VOLUME_PLUS_4_5DB,
         ES8388_OUTPUT_VOLUME_PLUS_4_5DB,
         false},
    };
    ESP_LOGW(TAG,
             "audio diagnostic output-test started through legacy I2S/ES8388; Wi-Fi/control/UDP/capture/PTT are disabled");
    for (;;) {
        for (size_t i = 0; i < sizeof(profiles) / sizeof(profiles[0]); i++) {
            ESP_ERROR_CHECK_WITHOUT_ABORT(es8388_set_playback_mute(true));
            ESP_ERROR_CHECK_WITHOUT_ABORT(audio_hw_apply_output_registers(profiles[i].label,
                                                                          profiles[i].dac_power,
                                                                          profiles[i].out1_volume,
                                                                          profiles[i].out2_volume,
                                                                          true));
            ESP_ERROR_CHECK_WITHOUT_ABORT(audio_hw_set_pa_gpio_level(profiles[i].pa_high));
            vTaskDelay(pdMS_TO_TICKS(180));
            ESP_ERROR_CHECK_WITHOUT_ABORT(es8388_set_playback_mute(false));
            vTaskDelay(pdMS_TO_TICKS(20));
            ESP_LOGW(TAG,
                     "output-test pass=%" PRIu32 " profile=%s fixed=48000Hz/16-bit/stereo/%s/MCLK: "
                     "listen for three clean tones",
                     pass++,
                     profiles[i].label,
                     i2s_format_name());
            diagnostic_play_tone(ESP32_AUDIO_HW_SAMPLE_RATE, 440.0f, 12000.0f, 220);
            vTaskDelay(pdMS_TO_TICKS(120));
            diagnostic_play_tone(ESP32_AUDIO_HW_SAMPLE_RATE, 660.0f, 12000.0f, 220);
            vTaskDelay(pdMS_TO_TICKS(120));
            diagnostic_play_tone(ESP32_AUDIO_HW_SAMPLE_RATE, 880.0f, 12000.0f, 300);
            vTaskDelay(pdMS_TO_TICKS(900));
        }
    }
}

static void diagnostic_log_capture_report(const char *prefix,
                                          esp32_adc_input_t input,
                                          const capture_health_accumulator_t *acc)
{
    if (!acc || acc->samples == 0) {
        return;
    }
    capture_channel_accumulator_t average = {0};
    average.peak_abs = (acc->left.peak_abs + acc->right.peak_abs) / 2;
    average.sum = (acc->left.sum + acc->right.sum) / 2.0;
    average.sum_squares = (acc->left.sum_squares + acc->right.sum_squares) / 2.0;
    ESP_LOGW(TAG,
             "%s input=%s samples=%" PRIu32
             " left:rms=%.4f peak=%.4f dc=%.4f right:rms=%.4f peak=%.4f dc=%.4f avg:rms=%.4f peak=%.4f dc=%.4f raw_clips=%" PRIu32,
             prefix,
             adc_input_name(input),
             acc->samples,
             capture_channel_rms(&acc->left, acc->samples),
             capture_channel_peak(&acc->left),
             capture_channel_dc_offset(&acc->left, acc->samples),
             capture_channel_rms(&acc->right, acc->samples),
             capture_channel_peak(&acc->right),
             capture_channel_dc_offset(&acc->right, acc->samples),
             capture_channel_rms(&average, acc->samples),
             capture_channel_peak(&average),
             capture_channel_dc_offset(&average, acc->samples),
             acc->raw_clipped_samples);
}

static void diagnostic_accumulate_capture_frame(capture_health_accumulator_t *acc, size_t samples_per_frame)
{
    for (size_t i = 0; i < samples_per_frame; i++) {
        int16_t left = s_capture_stereo_frame[i * 2];
        int16_t right = s_capture_stereo_frame[i * 2 + 1];
        if (abs_i16_value(left) >= CAPTURE_CLIP_THRESHOLD || abs_i16_value(right) >= CAPTURE_CLIP_THRESHOLD) {
            acc->raw_clipped_samples++;
        }
        capture_channel_accumulate(&acc->left, left);
        capture_channel_accumulate(&acc->right, right);
        acc->samples++;
    }
}

static void diagnostic_capture_test(void)
{
    ESP_LOGW(TAG,
             "audio diagnostic capture-test started through legacy I2S/ES8388; Wi-Fi/control/UDP/playback/PTT are disabled");
    esp32_audio_config_t initial_audio = audio_config_snapshot();
    ESP_ERROR_CHECK_WITHOUT_ABORT(audio_hw_apply_audio_config(&initial_audio));
    for (;;) {
        esp32_audio_config_t audio = audio_config_snapshot();
        int64_t report_start = esp_timer_get_time();
        capture_health_accumulator_t acc = {0};
        while (esp_timer_get_time() - report_start < 1000000) {
            esp_err_t err = audio_hw_read(s_capture_stereo_frame, ESP32_AUDIO_HW_FRAME_BYTES);
            if (err != ESP_OK) {
                ESP_LOGW(TAG, "capture-test read failed: %s", esp_err_to_name(err));
                continue;
            }
            diagnostic_accumulate_capture_frame(&acc, ESP32_AUDIO_HW_SAMPLES_PER_FRAME);
        }
        diagnostic_log_capture_report("capture-test", audio.adc_input, &acc);
    }
}

static void diagnostic_local_loopback(void)
{
    ESP_LOGW(TAG, "audio diagnostic local-loopback started; Wi-Fi/control/UDP/PTT are disabled");
    capture_health_accumulator_t acc = {0};
    int64_t report_start = esp_timer_get_time();
    for (;;) {
        esp32_audio_config_t audio = audio_config_snapshot();
        size_t samples_per_frame = ESP32_AUDIO_HW_SAMPLES_PER_FRAME;
        esp_err_t err = audio_hw_read(s_capture_stereo_frame, ESP32_AUDIO_HW_FRAME_BYTES);
        if (err != ESP_OK) {
            ESP_LOGW(TAG, "local-loopback capture read failed: %s", esp_err_to_name(err));
            continue;
        }

        diagnostic_accumulate_capture_frame(&acc, samples_per_frame);
        for (size_t i = 0; i < samples_per_frame; i++) {
            int16_t left = s_capture_stereo_frame[i * 2];
            int16_t right = s_capture_stereo_frame[i * 2 + 1];
            int selected = capture_select_sample(&audio, left, right);
            int filtered = capture_high_pass_sample(&audio, selected);
            int mixed = filtered * audio.mic_software_gain_percent / 100;
            int16_t sample = clamp_i16(mixed);
            s_playback_stereo_frame[i * 2] = sample;
            s_playback_stereo_frame[i * 2 + 1] = sample;
        }
        err = audio_hw_write(s_playback_stereo_frame, ESP32_AUDIO_HW_FRAME_BYTES);
        if (err != ESP_OK) {
            ESP_LOGW(TAG,
                     "local-loopback playback write failed: err=%s bytes=0/%u",
                     esp_err_to_name(err),
                     (unsigned)ESP32_AUDIO_HW_FRAME_BYTES);
        }
        int64_t now = esp_timer_get_time();
        if (now - report_start >= 1000000) {
            diagnostic_log_capture_report("local-loopback", audio.adc_input, &acc);
            memset(&acc, 0, sizeof(acc));
            report_start = now;
        }
    }
}

static void capture_task(void *arg)
{
    (void)arg;
    watchdog_register_current_task("ic_capture");
    ESP_LOGI(TAG, "task ic_capture running on core %d", xPortGetCoreID());
    capture_health_accumulator_t capture_health = {0};

    for (;;) {
        ic_codec_t codec = runtime_codec_snapshot();
        size_t hw_samples_per_frame = ESP32_AUDIO_HW_SAMPLES_PER_FRAME;
        size_t packet_samples_per_frame = codec_samples_per_frame(codec);
        uint16_t payload_bytes = 0;
        esp_err_t err = audio_hw_read(s_capture_stereo_frame, ESP32_AUDIO_HW_FRAME_BYTES);
        if (err != ESP_OK) {
            ESP_LOGW(TAG, "codec capture read failed: %s", esp_err_to_name(err));
            continue;
        }

        esp32_audio_config_t audio = audio_config_snapshot();
        memset(s_capture_mono_frame, 0, sizeof(s_capture_mono_frame));
        for (size_t i = 0; i < hw_samples_per_frame; i++) {
            int16_t left = s_capture_stereo_frame[i * 2];
            int16_t right = s_capture_stereo_frame[i * 2 + 1];
            int selected = capture_select_sample(&audio, left, right);
            int filtered = capture_high_pass_sample(&audio, selected);
            int amplified = filtered * audio.mic_software_gain_percent / 100;
            if (abs_i16_value(left) >= CAPTURE_CLIP_THRESHOLD || abs_i16_value(right) >= CAPTURE_CLIP_THRESHOLD) {
                capture_health.raw_clipped_samples++;
            }
            if (amplified > INT16_MAX || amplified < INT16_MIN) {
                capture_health.software_clipped_samples++;
            }
            s_capture_mono_frame[i] = clamp_i16(amplified);
            capture_channel_accumulate(&capture_health.left, left);
            capture_channel_accumulate(&capture_health.right, right);
            capture_channel_accumulate(&capture_health.selected, s_capture_mono_frame[i]);
            capture_health.samples++;
        }
        resample_hw_to_network(s_capture_mono_frame, s_capture_packet_frame, packet_samples_per_frame);
        capture_health_maybe_publish(&capture_health, esp_timer_get_time());

        if (audio.sidetone_mode == ESP32_SIDETONE_FIRMWARE) {
            ic_pcm_frame_ring_push(&s_sidetone_ring, s_capture_mono_frame);
        }
        if (!build_encoded_audio_payload(codec,
                                         s_capture_packet_frame,
                                         packet_samples_per_frame,
                                         s_capture_payload,
                                         sizeof(s_capture_payload),
                                         &payload_bytes)) {
            capture_health.tx_send_failures++;
            continue;
        }
        size_t count = build_tx_targets(s_capture_targets, sizeof(s_capture_targets) / sizeof(s_capture_targets[0]));
        capture_health.tx_target_count = (uint16_t)count;
        for (size_t i = 0; i < count; i++) {
            if (enqueue_audio_payload(s_capture_targets[i].kind,
                                      s_capture_targets[i].id,
                                      codec,
                                      s_capture_payload,
                                      payload_bytes)) {
                capture_health.tx_packets_sent++;
            } else {
                capture_health.tx_send_failures++;
            }
        }
        watchdog_reset_current_task();
    }
}

static void set_local_regular_talk(bool active)
{
    xSemaphoreTake(s_config_lock, portMAX_DELAY);
    s_config.regular_talk_active = active;
    xSemaphoreGive(s_config_lock);
}

static bool set_local_button_pressed(const char *id, bool pressed, bool *active_out)
{
    bool found = false;
    xSemaphoreTake(s_config_lock, portMAX_DELAY);
    for (size_t i = 0; i < s_config.button_count; i++) {
        if (!string_eq(s_config.buttons[i].id, id)) {
            continue;
        }
        found = true;
        if (s_config.buttons[i].latching) {
            if (pressed) {
                s_config.buttons[i].active = !s_config.buttons[i].active;
            }
        } else {
            s_config.buttons[i].active = pressed;
        }
        if (active_out) {
            *active_out = s_config.buttons[i].active;
        }
        break;
    }
    xSemaphoreGive(s_config_lock);
    return found;
}

static void handle_physical_button(physical_button_t *button, bool is_ptt, bool pressed)
{
    if (is_ptt) {
        ESP_LOGI(TAG, "PTT %s", pressed ? "down" : "up");
        set_local_regular_talk(pressed);
        send_talk_control(pressed);
        return;
    }
    ESP_LOGI(TAG, "button %s %s", button->id, pressed ? "down" : "up");
    bool active = pressed;
    if (set_local_button_pressed(button->id, pressed, &active)) {
        ui_set_button_active(button->id, active);
    } else {
        ui_set_button_active(button->id, pressed);
    }
    send_button_control(button->id, pressed);
}

static void handle_reply_button(bool pressed)
{
    if (pressed) {
        ui_state_t ui = ui_state_snapshot();
        uint64_t alert_id = 0;
        uint16_t target = 0;
        if (ui.active_alert.present) {
            alert_id = ui.active_alert.id;
            if (ui.active_alert.sender != 0 && ui.active_alert.sender != current_user_id()) {
                target = ui.active_alert.sender;
            }
        }
        if (target == 0 && ui.has_last_direct_caller && ui.last_direct_caller != current_user_id()) {
            target = ui.last_direct_caller;
        }
        if (alert_id != 0) {
            send_ack_alert_control(alert_id);
        }
        if (target == 0) {
            ESP_LOGI(TAG, "reply button down with no alert sender or last direct caller");
            ui_set_transient_status("No reply target");
            ui_set_reply_state(false, 0);
            s_reply_active_target = 0;
            return;
        }
        ESP_LOGI(TAG, "reply button down: direct call target=%u", (unsigned)target);
        s_reply_active_target = target;
        ui_set_reply_state(true, target);
        send_direct_call_control(target, true);
        return;
    }

    uint16_t target = s_reply_active_target;
    s_reply_active_target = 0;
    ui_set_reply_state(false, 0);
    if (target != 0) {
        ESP_LOGI(TAG, "reply button up: direct call target=%u", (unsigned)target);
        send_direct_call_control(target, false);
    }
}

static void poll_button_state(physical_button_t *button, bool is_ptt)
{
    if (!button || button->gpio < 0) {
        return;
    }
    bool raw_pressed = gpio_get_level(button->gpio) == 0;
    int64_t now = esp_timer_get_time();
    if (raw_pressed != button->last_raw_pressed) {
        button->last_raw_pressed = raw_pressed;
        button->last_change_us = now;
    }
    if (raw_pressed != button->stable_pressed && now - button->last_change_us >= BUTTON_DEBOUNCE_US) {
        button->stable_pressed = raw_pressed;
        handle_physical_button(button, is_ptt, raw_pressed);
    }
}

static void poll_reply_button_state(void)
{
    if (s_reply_button.gpio < 0) {
        return;
    }
    bool raw_pressed = gpio_get_level(s_reply_button.gpio) == 0;
    int64_t now = esp_timer_get_time();
    if (raw_pressed != s_reply_button.last_raw_pressed) {
        s_reply_button.last_raw_pressed = raw_pressed;
        s_reply_button.last_change_us = now;
    }
    if (raw_pressed != s_reply_button.stable_pressed && now - s_reply_button.last_change_us >= BUTTON_DEBOUNCE_US) {
        s_reply_button.stable_pressed = raw_pressed;
        handle_reply_button(raw_pressed);
    }
}

static void button_task(void *arg)
{
    (void)arg;
    watchdog_register_current_task("ic_buttons");
    ESP_LOGI(TAG, "task ic_buttons running on core %d", xPortGetCoreID());
    physical_button_t ptt = {CONFIG_INTERCOM_PTT_GPIO, "regular", "PTT", false, false, 0};
    for (;;) {
        poll_button_state(&ptt, true);
        for (size_t i = 0; i < sizeof(s_dedicated_buttons) / sizeof(s_dedicated_buttons[0]); i++) {
            poll_button_state(&s_dedicated_buttons[i], false);
        }
        poll_reply_button_state();
        watchdog_reset_current_task();
        vTaskDelay(pdMS_TO_TICKS(10));
    }
}

#if CONFIG_INTERCOM_DISPLAY_ST7789
static uint16_t rgb565(uint8_t r, uint8_t g, uint8_t b)
{
    return (uint16_t)(((uint16_t)(r & 0xF8) << 8) | ((uint16_t)(g & 0xFC) << 3) | (b >> 3));
}

static void display_fill(uint16_t color)
{
    if (!s_display_framebuffer) {
        return;
    }
    for (size_t i = 0; i < DISPLAY_PIXELS; i++) {
        s_display_framebuffer[i] = color;
    }
}

static void display_fill_rect(int x, int y, int w, int h, uint16_t color)
{
    if (!s_display_framebuffer || w <= 0 || h <= 0) {
        return;
    }
    int x0 = x < 0 ? 0 : x;
    int y0 = y < 0 ? 0 : y;
    int x1 = x + w > DISPLAY_WIDTH ? DISPLAY_WIDTH : x + w;
    int y1 = y + h > DISPLAY_HEIGHT ? DISPLAY_HEIGHT : y + h;
    for (int yy = y0; yy < y1; yy++) {
        for (int xx = x0; xx < x1; xx++) {
            s_display_framebuffer[yy * DISPLAY_WIDTH + xx] = color;
        }
    }
}

static void display_draw_rect(int x, int y, int w, int h, uint16_t color)
{
    display_fill_rect(x, y, w, 1, color);
    display_fill_rect(x, y + h - 1, w, 1, color);
    display_fill_rect(x, y, 1, h, color);
    display_fill_rect(x + w - 1, y, 1, h, color);
}

#define GLYPH(g0, g1, g2, g3, g4) \
    do {                          \
        out[0] = (g0);            \
        out[1] = (g1);            \
        out[2] = (g2);            \
        out[3] = (g3);            \
        out[4] = (g4);            \
        return true;              \
    } while (0)

static bool display_glyph(char input, uint8_t out[5])
{
    char c = input;
    if (c >= 'a' && c <= 'z') {
        c = (char)(c - 'a' + 'A');
    }
    switch (c) {
    case ' ':
        GLYPH(0x00, 0x00, 0x00, 0x00, 0x00);
    case '!':
        GLYPH(0x00, 0x00, 0x5F, 0x00, 0x00);
    case '%':
        GLYPH(0x23, 0x13, 0x08, 0x64, 0x62);
    case '+':
        GLYPH(0x08, 0x08, 0x3E, 0x08, 0x08);
    case '-':
        GLYPH(0x08, 0x08, 0x08, 0x08, 0x08);
    case '.':
        GLYPH(0x00, 0x60, 0x60, 0x00, 0x00);
    case '/':
        GLYPH(0x20, 0x10, 0x08, 0x04, 0x02);
    case ':':
        GLYPH(0x00, 0x36, 0x36, 0x00, 0x00);
    case '?':
        GLYPH(0x02, 0x01, 0x51, 0x09, 0x06);
    case '_':
        GLYPH(0x40, 0x40, 0x40, 0x40, 0x40);
    case '0':
        GLYPH(0x3E, 0x51, 0x49, 0x45, 0x3E);
    case '1':
        GLYPH(0x00, 0x42, 0x7F, 0x40, 0x00);
    case '2':
        GLYPH(0x42, 0x61, 0x51, 0x49, 0x46);
    case '3':
        GLYPH(0x21, 0x41, 0x45, 0x4B, 0x31);
    case '4':
        GLYPH(0x18, 0x14, 0x12, 0x7F, 0x10);
    case '5':
        GLYPH(0x27, 0x45, 0x45, 0x45, 0x39);
    case '6':
        GLYPH(0x3C, 0x4A, 0x49, 0x49, 0x30);
    case '7':
        GLYPH(0x01, 0x71, 0x09, 0x05, 0x03);
    case '8':
        GLYPH(0x36, 0x49, 0x49, 0x49, 0x36);
    case '9':
        GLYPH(0x06, 0x49, 0x49, 0x29, 0x1E);
    case 'A':
        GLYPH(0x7E, 0x11, 0x11, 0x11, 0x7E);
    case 'B':
        GLYPH(0x7F, 0x49, 0x49, 0x49, 0x36);
    case 'C':
        GLYPH(0x3E, 0x41, 0x41, 0x41, 0x22);
    case 'D':
        GLYPH(0x7F, 0x41, 0x41, 0x22, 0x1C);
    case 'E':
        GLYPH(0x7F, 0x49, 0x49, 0x49, 0x41);
    case 'F':
        GLYPH(0x7F, 0x09, 0x09, 0x09, 0x01);
    case 'G':
        GLYPH(0x3E, 0x41, 0x49, 0x49, 0x7A);
    case 'H':
        GLYPH(0x7F, 0x08, 0x08, 0x08, 0x7F);
    case 'I':
        GLYPH(0x00, 0x41, 0x7F, 0x41, 0x00);
    case 'J':
        GLYPH(0x20, 0x40, 0x41, 0x3F, 0x01);
    case 'K':
        GLYPH(0x7F, 0x08, 0x14, 0x22, 0x41);
    case 'L':
        GLYPH(0x7F, 0x40, 0x40, 0x40, 0x40);
    case 'M':
        GLYPH(0x7F, 0x02, 0x0C, 0x02, 0x7F);
    case 'N':
        GLYPH(0x7F, 0x04, 0x08, 0x10, 0x7F);
    case 'O':
        GLYPH(0x3E, 0x41, 0x41, 0x41, 0x3E);
    case 'P':
        GLYPH(0x7F, 0x09, 0x09, 0x09, 0x06);
    case 'Q':
        GLYPH(0x3E, 0x41, 0x51, 0x21, 0x5E);
    case 'R':
        GLYPH(0x7F, 0x09, 0x19, 0x29, 0x46);
    case 'S':
        GLYPH(0x46, 0x49, 0x49, 0x49, 0x31);
    case 'T':
        GLYPH(0x01, 0x01, 0x7F, 0x01, 0x01);
    case 'U':
        GLYPH(0x3F, 0x40, 0x40, 0x40, 0x3F);
    case 'V':
        GLYPH(0x1F, 0x20, 0x40, 0x20, 0x1F);
    case 'W':
        GLYPH(0x3F, 0x40, 0x38, 0x40, 0x3F);
    case 'X':
        GLYPH(0x63, 0x14, 0x08, 0x14, 0x63);
    case 'Y':
        GLYPH(0x07, 0x08, 0x70, 0x08, 0x07);
    case 'Z':
        GLYPH(0x61, 0x51, 0x49, 0x45, 0x43);
    default:
        GLYPH(0x00, 0x00, 0x00, 0x00, 0x00);
    }
}

#undef GLYPH

static int display_text_width(const char *text, int scale)
{
    return text ? (int)strlen(text) * 6 * scale : 0;
}

static void display_draw_char(int x, int y, char c, uint16_t fg, uint16_t bg, int scale, bool fill_bg)
{
    uint8_t glyph[5];
    display_glyph(c, glyph);
    if (fill_bg) {
        display_fill_rect(x, y, 6 * scale, 8 * scale, bg);
    }
    for (int col = 0; col < 5; col++) {
        for (int row = 0; row < 7; row++) {
            if (glyph[col] & (1U << row)) {
                display_fill_rect(x + col * scale, y + row * scale, scale, scale, fg);
            }
        }
    }
}

static void display_draw_text_clipped(int x, int y, int max_w, const char *text, uint16_t fg, uint16_t bg, int scale)
{
    if (!text || max_w <= 0) {
        return;
    }
    int cursor = x;
    for (size_t i = 0; text[i] != '\0'; i++) {
        if (cursor + 6 * scale > x + max_w) {
            break;
        }
        display_draw_char(cursor, y, text[i], fg, bg, scale, true);
        cursor += 6 * scale;
    }
}

static void display_draw_centered_text(int y, const char *text, uint16_t fg, uint16_t bg, int scale)
{
    int width = display_text_width(text, scale);
    int x = (DISPLAY_WIDTH - width) / 2;
    if (x < 0) {
        x = 0;
    }
    display_draw_text_clipped(x, y, DISPLAY_WIDTH - x, text, fg, bg, scale);
}

static void display_draw_button_box(size_t index, const ui_button_state_t *button)
{
    const uint16_t white = rgb565(245, 247, 250);
    const uint16_t dim = rgb565(108, 117, 125);
    const uint16_t border = rgb565(82, 94, 105);
    const uint16_t active_bg = rgb565(12, 122, 80);
    const uint16_t inactive_bg = rgb565(18, 24, 30);
    const uint16_t disabled_bg = rgb565(8, 11, 14);
    int x = (index == 1 || index == 3) ? 132 : 8;
    int y = index < 2 ? 76 : 164;
    int w = 100;
    int h = 36;
    uint16_t bg = button->active ? active_bg : (button->enabled && button->configured ? inactive_bg : disabled_bg);
    uint16_t fg = button->enabled && button->configured ? white : dim;
    display_fill_rect(x, y, w, h, bg);
    display_draw_rect(x, y, w, h, button->active ? white : border);
    char label[48];
    const char corner = (char)('A' + index);
    if (button->label[0]) {
        snprintf(label, sizeof(label), "%c %s", corner, button->label);
    } else {
        snprintf(label, sizeof(label), "%c", corner);
    }
    display_draw_text_clipped(x + 6, y + 11, w - 12, label, fg, bg, 1);
}

static void display_render_fullscreen(const ui_state_t *ui)
{
    const uint16_t bg = rgb565(5, 8, 12);
    const uint16_t fg = rgb565(245, 247, 250);
    const uint16_t muted = rgb565(136, 146, 158);
    display_fill(bg);

    const char *headline = "STARTING";
    char detail[MAX_UI_STATUS] = "";
    if (!ui->wifi_connected) {
        headline = "WIFI";
        strlcpy(detail, "CONNECTING", sizeof(detail));
    } else if (!ui->control_connected) {
        headline = "SERVER";
        strlcpy(detail, "CONNECTING", sizeof(detail));
    } else if (!ui->config_received) {
        headline = "CONFIG";
        strlcpy(detail,
                ui->blocking_status[0] ? ui->blocking_status : "WAITING",
                sizeof(detail));
    }

    display_draw_centered_text(82, headline, fg, bg, 3);
    display_draw_centered_text(124, detail, muted, bg, 2);

    char identity[64];
    snprintf(identity,
             sizeof(identity),
             "ID %u REQ %u",
             (unsigned)ui->user_id,
             (unsigned)CONFIG_INTERCOM_USER_ID);
    display_draw_centered_text(166, identity, muted, bg, 1);
    display_draw_centered_text(182, CONFIG_INTERCOM_SERVER_HOST, muted, bg, 1);
    char uid[32];
    snprintf(uid, sizeof(uid), "UID %.8s", s_client_uid[0] ? s_client_uid : "--------");
    display_draw_centered_text(198, uid, muted, bg, 1);
}

static void display_render_normal(const ui_state_t *ui)
{
    const uint16_t bg = rgb565(4, 7, 10);
    const uint16_t panel = rgb565(12, 17, 22);
    const uint16_t white = rgb565(245, 247, 250);
    const uint16_t muted = rgb565(140, 150, 160);
    const uint16_t ok = rgb565(77, 171, 122);
    const uint16_t alert = rgb565(174, 36, 44);
    const uint16_t call = rgb565(28, 99, 170);
    display_fill(bg);

    display_fill_rect(0, 0, DISPLAY_WIDTH, 20, panel);
    display_draw_text_clipped(4, 6, 62, "BAT --", muted, panel, 1);
    display_draw_text_clipped(84, 6, 62, ui->wifi_connected ? "WIFI OK" : "WIFI --", ui->wifi_connected ? ok : muted, panel, 1);
    display_draw_text_clipped(166, 6, 70, ui->control_connected ? "SRV OK" : "SRV --", ui->control_connected ? ok : muted, panel, 1);

    uint16_t banner_bg = panel;
    char banner[96] = "";
    uint16_t banner_fg = muted;
    int64_t now = esp_timer_get_time();
    if (ui->reply_held && ui->reply_target != 0) {
        banner_bg = call;
        banner_fg = white;
        snprintf(banner, sizeof(banner), "CALLING USER %u", (unsigned)ui->reply_target);
    } else if (ui->active_alert.present) {
        banner_bg = alert;
        banner_fg = white;
        if (ui->active_alert.message[0]) {
            snprintf(banner, sizeof(banner), "ALERT %u %s", (unsigned)ui->active_alert.sender, ui->active_alert.message);
        } else {
            snprintf(banner, sizeof(banner), "ALERT FROM %u", (unsigned)ui->active_alert.sender);
        }
    } else if (ui->transient_status[0] && now < ui->transient_until_us) {
        banner_bg = panel;
        banner_fg = white;
        strlcpy(banner, ui->transient_status, sizeof(banner));
    } else if (ui->active_direct_call_count > 0) {
        banner_bg = call;
        banner_fg = white;
        snprintf(banner, sizeof(banner), "%u DIRECT CALL", (unsigned)ui->active_direct_call_count);
    } else {
        strlcpy(banner, "READY", sizeof(banner));
    }
    display_fill_rect(0, 24, DISPLAY_WIDTH, 36, banner_bg);
    display_draw_centered_text(35, banner, banner_fg, banner_bg, 1);

    for (size_t i = 0; i < MAX_BUTTONS; i++) {
        display_draw_button_box(i, &ui->buttons[i]);
    }

    char identity[96];
    if (ui->unit_name[0]) {
        snprintf(identity, sizeof(identity), "ID %u  %s", (unsigned)ui->user_id, ui->unit_name);
    } else {
        snprintf(identity, sizeof(identity), "ID %u", (unsigned)ui->user_id);
    }
    display_draw_centered_text(226, identity, white, bg, 1);
}

static void display_render(void)
{
    if (!s_display_panel || !s_display_framebuffer) {
        return;
    }
    ui_state_t ui = ui_state_snapshot();
    if (!ui.wifi_connected || !ui.control_connected || !ui.config_received) {
        display_render_fullscreen(&ui);
    } else {
        display_render_normal(&ui);
    }
    esp_err_t err = esp_lcd_panel_draw_bitmap(s_display_panel, 0, 0, DISPLAY_WIDTH, DISPLAY_HEIGHT, s_display_framebuffer);
    if (err != ESP_OK) {
        ESP_LOGW(TAG, "display draw failed: %s", esp_err_to_name(err));
    }
}

static spi_host_device_t display_spi_host(void)
{
    switch (CONFIG_INTERCOM_DISPLAY_SPI_HOST) {
    case 1:
        return SPI1_HOST;
    case 3:
        return SPI3_HOST;
    case 2:
    default:
        return SPI2_HOST;
    }
}

static bool display_required_pins_configured(void)
{
    return CONFIG_INTERCOM_DISPLAY_SPI_MOSI_GPIO >= 0 &&
           CONFIG_INTERCOM_DISPLAY_SPI_SCLK_GPIO >= 0 &&
           CONFIG_INTERCOM_DISPLAY_SPI_CS_GPIO >= 0 &&
           CONFIG_INTERCOM_DISPLAY_DC_GPIO >= 0;
}

static esp_err_t display_apply_rotation(void)
{
#if CONFIG_INTERCOM_DISPLAY_ROTATION_90
    ESP_RETURN_ON_ERROR(esp_lcd_panel_swap_xy(s_display_panel, true), TAG, "display swap xy");
    ESP_RETURN_ON_ERROR(esp_lcd_panel_mirror(s_display_panel, true, false), TAG, "display mirror");
#elif CONFIG_INTERCOM_DISPLAY_ROTATION_180
    ESP_RETURN_ON_ERROR(esp_lcd_panel_swap_xy(s_display_panel, false), TAG, "display swap xy");
    ESP_RETURN_ON_ERROR(esp_lcd_panel_mirror(s_display_panel, true, true), TAG, "display mirror");
#elif CONFIG_INTERCOM_DISPLAY_ROTATION_270
    ESP_RETURN_ON_ERROR(esp_lcd_panel_swap_xy(s_display_panel, true), TAG, "display swap xy");
    ESP_RETURN_ON_ERROR(esp_lcd_panel_mirror(s_display_panel, false, true), TAG, "display mirror");
#else
    ESP_RETURN_ON_ERROR(esp_lcd_panel_swap_xy(s_display_panel, false), TAG, "display swap xy");
    ESP_RETURN_ON_ERROR(esp_lcd_panel_mirror(s_display_panel, false, false), TAG, "display mirror");
#endif
    return ESP_OK;
}

static esp_err_t display_init(void)
{
    if (!display_required_pins_configured()) {
        ESP_LOGW(TAG, "ST7789 display enabled but required SPI/DC pins are not configured");
        return ESP_ERR_INVALID_STATE;
    }

    if (CONFIG_INTERCOM_DISPLAY_BACKLIGHT_GPIO >= 0) {
        gpio_config_t backlight = {
            .pin_bit_mask = 1ULL << (unsigned)CONFIG_INTERCOM_DISPLAY_BACKLIGHT_GPIO,
            .mode = GPIO_MODE_OUTPUT,
            .pull_up_en = GPIO_PULLUP_DISABLE,
            .pull_down_en = GPIO_PULLDOWN_DISABLE,
            .intr_type = GPIO_INTR_DISABLE,
        };
        ESP_RETURN_ON_ERROR(gpio_config(&backlight), TAG, "configure display backlight");
        gpio_set_level(CONFIG_INTERCOM_DISPLAY_BACKLIGHT_GPIO,
#if CONFIG_INTERCOM_DISPLAY_BACKLIGHT_ACTIVE_LOW
                       1
#else
                       0
#endif
        );
    }

    spi_bus_config_t buscfg = {
        .mosi_io_num = CONFIG_INTERCOM_DISPLAY_SPI_MOSI_GPIO,
        .miso_io_num = -1,
        .sclk_io_num = CONFIG_INTERCOM_DISPLAY_SPI_SCLK_GPIO,
        .quadwp_io_num = -1,
        .quadhd_io_num = -1,
        .max_transfer_sz = DISPLAY_WIDTH * 40 * sizeof(uint16_t),
    };
    ESP_RETURN_ON_ERROR(spi_bus_initialize(display_spi_host(), &buscfg, SPI_DMA_CH_AUTO), TAG, "initialize display SPI bus");

    esp_lcd_panel_io_handle_t io_handle = NULL;
    esp_lcd_panel_io_spi_config_t io_config = {
        .dc_gpio_num = CONFIG_INTERCOM_DISPLAY_DC_GPIO,
        .cs_gpio_num = CONFIG_INTERCOM_DISPLAY_SPI_CS_GPIO,
        .pclk_hz = CONFIG_INTERCOM_DISPLAY_SPI_CLOCK_HZ,
        .lcd_cmd_bits = 8,
        .lcd_param_bits = 8,
        .spi_mode = 0,
        .trans_queue_depth = 10,
    };
    ESP_RETURN_ON_ERROR(esp_lcd_new_panel_io_spi((esp_lcd_spi_bus_handle_t)display_spi_host(), &io_config, &io_handle),
                        TAG,
                        "create display panel IO");

    esp_lcd_panel_dev_config_t panel_config = {
        .reset_gpio_num = CONFIG_INTERCOM_DISPLAY_RST_GPIO,
        .rgb_ele_order = LCD_RGB_ELEMENT_ORDER_RGB,
        .bits_per_pixel = 16,
    };
    ESP_RETURN_ON_ERROR(esp_lcd_new_panel_st7789(io_handle, &panel_config, &s_display_panel), TAG, "create ST7789 panel");
    ESP_RETURN_ON_ERROR(esp_lcd_panel_reset(s_display_panel), TAG, "reset ST7789");
    ESP_RETURN_ON_ERROR(esp_lcd_panel_init(s_display_panel), TAG, "init ST7789");
    ESP_RETURN_ON_ERROR(esp_lcd_panel_invert_color(s_display_panel, true), TAG, "invert ST7789 colors");
    ESP_RETURN_ON_ERROR(display_apply_rotation(), TAG, "rotate ST7789");
    ESP_RETURN_ON_ERROR(esp_lcd_panel_disp_on_off(s_display_panel, true), TAG, "enable ST7789 display");

    s_display_framebuffer = heap_caps_malloc(DISPLAY_PIXELS * sizeof(uint16_t), MALLOC_CAP_SPIRAM | MALLOC_CAP_8BIT);
    s_display_framebuffer_in_psram = s_display_framebuffer && esp_ptr_external_ram(s_display_framebuffer);
    if (!s_display_framebuffer) {
        s_display_framebuffer = heap_caps_malloc(DISPLAY_PIXELS * sizeof(uint16_t), MALLOC_CAP_8BIT);
        s_display_framebuffer_in_psram = false;
    }
    if (!s_display_framebuffer) {
        return ESP_ERR_NO_MEM;
    }
    s_display_framebuffer_bytes = DISPLAY_PIXELS * sizeof(uint16_t);

    if (CONFIG_INTERCOM_DISPLAY_BACKLIGHT_GPIO >= 0) {
        gpio_set_level(CONFIG_INTERCOM_DISPLAY_BACKLIGHT_GPIO,
#if CONFIG_INTERCOM_DISPLAY_BACKLIGHT_ACTIVE_LOW
                       0
#else
                       1
#endif
        );
    }
    display_render();
    s_display_initialized = true;
    ESP_LOGI(TAG, "ST7789 display initialized");
    return ESP_OK;
}

static void display_task(void *arg)
{
    (void)arg;
    watchdog_register_current_task("ic_display");
    ESP_LOGI(TAG, "task ic_display running on core %d", xPortGetCoreID());
    uint32_t last_version = UINT32_MAX;
    int64_t last_render_us = 0;
    for (;;) {
        int64_t now = esp_timer_get_time();
        uint32_t version = s_ui_state_version;
        ui_state_t ui = ui_state_snapshot();
        bool transient_active = ui.transient_status[0] && now < ui.transient_until_us;
        bool due_to_change = version != last_version && now - last_render_us >= DISPLAY_REFRESH_MIN_US;
        bool due_to_idle = now - last_render_us >= DISPLAY_IDLE_REFRESH_US;
        bool due_to_transient = transient_active && now - last_render_us >= DISPLAY_REFRESH_MIN_US;
        if (due_to_change || due_to_idle || due_to_transient) {
            display_render();
            last_version = version;
            last_render_us = now;
        }
        watchdog_reset_current_task();
        vTaskDelay(pdMS_TO_TICKS(100));
    }
}
#endif

static void runtime_config_init(void)
{
    s_config_lock = xSemaphoreCreateMutex();
    memset(&s_config, 0, sizeof(s_config));
    parse_channel_csv(CONFIG_INTERCOM_LISTEN_CHANNELS, &s_config.listen);
    parse_channel_csv(CONFIG_INTERCOM_TX_CHANNELS, &s_config.tx);
    s_config.codec = default_codec();
    s_config.talk_mode = TALK_MODE_PTT;
    s_runtime_user_id = CONFIG_INTERCOM_USER_ID;
}

void app_main(void)
{
    ESP_ERROR_CHECK(nvs_flash_init());
    client_identity_init();
    audio_diagnostic_mode_t diagnostic_mode = audio_diagnostic_mode();
    ESP_LOGW(TAG, "audio diagnostic mode: %s", audio_diagnostic_mode_name(diagnostic_mode));
    runtime_config_init();
    ui_state_init();
    audio_config_init();
    audio_cue_lut_init();
    s_ws_send_lock = xSemaphoreCreateMutex();
    ESP_ERROR_CHECK(s_ws_send_lock ? ESP_OK : ESP_ERR_NO_MEM);
    s_codec_mute_queue = xQueueCreate(1, sizeof(bool));
    ESP_ERROR_CHECK(s_codec_mute_queue ? ESP_OK : ESP_ERR_NO_MEM);
    s_audio_config_queue = xQueueCreate(1, sizeof(esp32_audio_config_t));
    ESP_ERROR_CHECK(s_audio_config_queue ? ESP_OK : ESP_ERR_NO_MEM);
    s_audio_tx_queue = xQueueCreate(CONFIG_INTERCOM_AUDIO_TX_QUEUE_PACKETS, sizeof(audio_tx_packet_t));
    ESP_ERROR_CHECK(s_audio_tx_queue ? ESP_OK : ESP_ERR_NO_MEM);
    ESP_ERROR_CHECK(ic_pcm_frame_ring_init(&s_playback_ring, CONFIG_INTERCOM_JITTER_FRAMES));
    ESP_ERROR_CHECK(ic_pcm_frame_ring_init(&s_sidetone_ring, 1));
#if CONFIG_INTERCOM_OPUS
    ESP_ERROR_CHECK(opus_codec_init());
#endif
    watchdog_init_if_enabled();
    if (diagnostic_mode == AUDIO_DIAGNOSTIC_NORMAL) {
        buttons_init();
#if CONFIG_INTERCOM_DISPLAY_ST7789
        if (display_init() == ESP_OK) {
            xTaskCreatePinnedToCore(display_task,
                                    "ic_display",
                                    4096,
                                    NULL,
                                    2,
                                    &s_display_task_handle,
                                    task_core(CONFIG_INTERCOM_NET_TASK_CORE));
        }
#endif
    }

    ESP_ERROR_CHECK(i2c_init());
    ESP_ERROR_CHECK(audio_hw_probe_codec());
    ESP_ERROR_CHECK(audio_i2s_init());
    ESP_ERROR_CHECK(audio_hw_init());
    vTaskDelay(pdMS_TO_TICKS(30));
    if (diagnostic_mode == AUDIO_DIAGNOSTIC_NORMAL) {
        xTaskCreatePinnedToCore(codec_mute_task, "ic_codec_mute", 3072, NULL, 6, NULL, task_core(CONFIG_INTERCOM_AUDIO_TASK_CORE));
        xTaskCreatePinnedToCore(audio_config_apply_task,
                                "ic_audio_cfg",
                                4096,
                                NULL,
                                6,
                                NULL,
                                task_core(CONFIG_INTERCOM_AUDIO_TASK_CORE));
        xTaskCreatePinnedToCore(playback_task,
                                "ic_playback",
                                CONFIG_INTERCOM_PLAYBACK_TASK_STACK_SIZE,
                                NULL,
                                10,
                                &s_playback_task_handle,
                                task_core(CONFIG_INTERCOM_AUDIO_TASK_CORE));
        vTaskDelay(pdMS_TO_TICKS(PLAYBACK_STARTUP_SETTLE_MS));
        audio_cue_start(AUDIO_CUE_RECONNECTING);
    }

    if (diagnostic_mode == AUDIO_DIAGNOSTIC_OUTPUT_TEST) {
        diagnostic_output_test();
    }

    if (diagnostic_mode == AUDIO_DIAGNOSTIC_CAPTURE_TEST) {
        diagnostic_capture_test();
    }
    if (diagnostic_mode == AUDIO_DIAGNOSTIC_LOCAL_LOOPBACK) {
        diagnostic_local_loopback();
    }

    wifi_init();
    xEventGroupWaitBits(s_wifi_events, WIFI_CONNECTED_BIT, false, true, portMAX_DELAY);
    websocket_start();

    xTaskCreatePinnedToCore(udp_task,
                            "ic_udp",
                            CONFIG_INTERCOM_UDP_TASK_STACK_SIZE,
                            NULL,
                            8,
                            &s_udp_task_handle,
                            task_core(CONFIG_INTERCOM_NET_TASK_CORE));
    xTaskCreatePinnedToCore(registration_task,
                            "ic_register",
                            4096,
                            NULL,
                            4,
                            &s_registration_task_handle,
                            task_core(CONFIG_INTERCOM_NET_TASK_CORE));
    xTaskCreatePinnedToCore(capture_task,
                            "ic_capture",
                            CONFIG_INTERCOM_CAPTURE_TASK_STACK_SIZE,
                            NULL,
                            8,
                            &s_capture_task_handle,
                            task_core(CONFIG_INTERCOM_AUDIO_TASK_CORE));
    xTaskCreatePinnedToCore(button_task,
                            "ic_buttons",
                            4096,
                            NULL,
                            5,
                            &s_button_task_handle,
                            task_core(CONFIG_INTERCOM_NET_TASK_CORE));

    ESP_LOGI(TAG,
             "ESP32-A1S intercom client started: user=%u uid=%s server=%s audio=%d control=%d",
             (unsigned)current_user_id(),
             s_client_uid,
             CONFIG_INTERCOM_SERVER_HOST,
             CONFIG_INTERCOM_AUDIO_PORT,
             CONFIG_INTERCOM_CONTROL_PORT);
}
