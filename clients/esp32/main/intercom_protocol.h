#pragma once

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#define IC_MAGIC0 'I'
#define IC_MAGIC1 'C'
#define IC_VERSION 2
#define IC_HEADER_LEN 17
#define IC_MAX_PACKET_BYTES 2048

#define IC_FRAME_MS 10
#define IC_PCM16_SAMPLE_RATE 16000
#define IC_PCM24_SAMPLE_RATE 24000
#define IC_PCM48_SAMPLE_RATE 48000
#define IC_PCM16_SAMPLES_PER_FRAME 160
#define IC_PCM24_SAMPLES_PER_FRAME 240
#define IC_PCM48_SAMPLES_PER_FRAME 480
#define IC_OPUS_SAMPLES_PER_FRAME IC_PCM24_SAMPLES_PER_FRAME
#define IC_OPUS_MAX_BYTES_PER_FRAME 1275
#define IC_MAX_SAMPLES_PER_FRAME IC_PCM48_SAMPLES_PER_FRAME
#define IC_PCM16_BYTES_PER_FRAME (IC_PCM16_SAMPLES_PER_FRAME * sizeof(int16_t))
#define IC_PCM24_BYTES_PER_FRAME (IC_PCM24_SAMPLES_PER_FRAME * sizeof(int16_t))
#define IC_PCM48_BYTES_PER_FRAME (IC_PCM48_SAMPLES_PER_FRAME * sizeof(int16_t))
#define IC_MAX_BYTES_PER_FRAME (IC_PCM48_BYTES_PER_FRAME * 2)

typedef enum {
    IC_CODEC_PCM16 = 0,
    IC_CODEC_ADPCM = 1,
    IC_CODEC_OPUS = 2,
    IC_CODEC_PCM48 = 3,
    IC_CODEC_PCM24 = 4,
} ic_codec_t;

typedef enum {
    IC_TARGET_CHANNEL = 1,
    IC_TARGET_DIRECT = 2,
    IC_TARGET_MIXED = 3,
} ic_target_kind_t;

typedef struct {
    uint16_t user_id;
    ic_target_kind_t target_kind;
    uint16_t target_id;
    ic_codec_t codec;
    uint16_t seq;
    uint32_t timestamp_ms;
    const uint8_t *payload;
    uint16_t payload_len;
} ic_audio_packet_t;

bool ic_encode_audio_packet(const ic_audio_packet_t *packet, uint8_t *out, size_t out_len, size_t *written);
bool ic_decode_audio_packet(const uint8_t *bytes, size_t len, ic_audio_packet_t *packet);
