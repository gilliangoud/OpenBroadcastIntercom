#include "intercom_protocol.h"

static void write_be16(uint8_t *out, uint16_t value)
{
    out[0] = (uint8_t)(value >> 8);
    out[1] = (uint8_t)value;
}

static void write_be32(uint8_t *out, uint32_t value)
{
    out[0] = (uint8_t)(value >> 24);
    out[1] = (uint8_t)(value >> 16);
    out[2] = (uint8_t)(value >> 8);
    out[3] = (uint8_t)value;
}

static uint16_t read_be16(const uint8_t *in)
{
    return ((uint16_t)in[0] << 8) | (uint16_t)in[1];
}

static uint32_t read_be32(const uint8_t *in)
{
    return ((uint32_t)in[0] << 24) | ((uint32_t)in[1] << 16) | ((uint32_t)in[2] << 8) | (uint32_t)in[3];
}

bool ic_encode_audio_packet(const ic_audio_packet_t *packet, uint8_t *out, size_t out_len, size_t *written)
{
    if (!packet || !out || !written) {
        return false;
    }
    if ((size_t)packet->payload_len + IC_HEADER_LEN > out_len) {
        return false;
    }
    if (packet->payload_len > 0 && packet->payload == NULL) {
        return false;
    }

    out[0] = IC_MAGIC0;
    out[1] = IC_MAGIC1;
    out[2] = IC_VERSION;
    write_be16(&out[3], packet->user_id);
    out[5] = (uint8_t)packet->target_kind;
    write_be16(&out[6], packet->target_id);
    out[8] = (uint8_t)packet->codec;
    write_be16(&out[9], packet->seq);
    write_be32(&out[11], packet->timestamp_ms);
    write_be16(&out[15], packet->payload_len);
    for (uint16_t i = 0; i < packet->payload_len; i++) {
        out[IC_HEADER_LEN + i] = packet->payload[i];
    }
    *written = IC_HEADER_LEN + packet->payload_len;
    return true;
}

bool ic_decode_audio_packet(const uint8_t *bytes, size_t len, ic_audio_packet_t *packet)
{
    if (!bytes || !packet || len < IC_HEADER_LEN) {
        return false;
    }
    if (bytes[0] != IC_MAGIC0 || bytes[1] != IC_MAGIC1 || bytes[2] != IC_VERSION) {
        return false;
    }
    uint8_t kind = bytes[5];
    if (kind != IC_TARGET_CHANNEL && kind != IC_TARGET_DIRECT && kind != IC_TARGET_MIXED) {
        return false;
    }
    uint8_t codec = bytes[8];
    if (codec > IC_CODEC_PCM24) {
        return false;
    }
    uint16_t payload_len = read_be16(&bytes[15]);
    if ((size_t)payload_len != len - IC_HEADER_LEN) {
        return false;
    }

    packet->user_id = read_be16(&bytes[3]);
    packet->target_kind = (ic_target_kind_t)kind;
    packet->target_id = read_be16(&bytes[6]);
    packet->codec = (ic_codec_t)codec;
    packet->seq = read_be16(&bytes[9]);
    packet->timestamp_ms = read_be32(&bytes[11]);
    packet->payload_len = payload_len;
    packet->payload = &bytes[IC_HEADER_LEN];
    return true;
}
