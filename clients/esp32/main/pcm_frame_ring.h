#pragma once

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#include "esp_err.h"
#include "freertos/FreeRTOS.h"
#include "freertos/portmacro.h"

#include "intercom_protocol.h"

typedef struct {
    int16_t samples[IC_MAX_SAMPLES_PER_FRAME];
} ic_pcm_frame_t;

typedef struct {
    ic_pcm_frame_t *frames;
    size_t capacity;
    size_t read_index;
    size_t write_index;
    size_t count;
    uint32_t underflows;
    uint32_t overflows;
    portMUX_TYPE lock;
} ic_pcm_frame_ring_t;

esp_err_t ic_pcm_frame_ring_init(ic_pcm_frame_ring_t *ring, size_t capacity);
bool ic_pcm_frame_ring_push(ic_pcm_frame_ring_t *ring, const int16_t *samples);
bool ic_pcm_frame_ring_pop(ic_pcm_frame_ring_t *ring, int16_t *samples);
void ic_pcm_frame_ring_clear(ic_pcm_frame_ring_t *ring);
size_t ic_pcm_frame_ring_count(ic_pcm_frame_ring_t *ring);
void ic_pcm_frame_ring_stats(ic_pcm_frame_ring_t *ring, size_t *count, uint32_t *underflows, uint32_t *overflows);
