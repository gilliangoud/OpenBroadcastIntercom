#include "pcm_frame_ring.h"

#include <string.h>

#include "esp_heap_caps.h"

esp_err_t ic_pcm_frame_ring_init(ic_pcm_frame_ring_t *ring, size_t capacity)
{
    if (!ring || capacity == 0) {
        return ESP_ERR_INVALID_ARG;
    }

    memset(ring, 0, sizeof(*ring));
    ring->frames = heap_caps_calloc(capacity, sizeof(ic_pcm_frame_t), MALLOC_CAP_SPIRAM | MALLOC_CAP_8BIT);
    if (!ring->frames) {
        ring->frames = heap_caps_calloc(capacity, sizeof(ic_pcm_frame_t), MALLOC_CAP_8BIT);
    }
    if (!ring->frames) {
        return ESP_ERR_NO_MEM;
    }
    ring->capacity = capacity;
    ring->lock = (portMUX_TYPE)portMUX_INITIALIZER_UNLOCKED;
    return ESP_OK;
}

bool ic_pcm_frame_ring_push(ic_pcm_frame_ring_t *ring, const int16_t *samples)
{
    if (!ring || !samples || !ring->frames) {
        return false;
    }

    taskENTER_CRITICAL(&ring->lock);
    if (ring->count == ring->capacity) {
        ring->read_index = (ring->read_index + 1) % ring->capacity;
        ring->count--;
        ring->overflows++;
    }
    memcpy(ring->frames[ring->write_index].samples, samples, sizeof(ring->frames[ring->write_index].samples));
    ring->write_index = (ring->write_index + 1) % ring->capacity;
    ring->count++;
    taskEXIT_CRITICAL(&ring->lock);
    return true;
}

bool ic_pcm_frame_ring_pop(ic_pcm_frame_ring_t *ring, int16_t *samples)
{
    if (!ring || !samples || !ring->frames) {
        return false;
    }

    bool ok = false;
    taskENTER_CRITICAL(&ring->lock);
    if (ring->count == 0) {
        ring->underflows++;
    } else {
        memcpy(samples, ring->frames[ring->read_index].samples, sizeof(ring->frames[ring->read_index].samples));
        ring->read_index = (ring->read_index + 1) % ring->capacity;
        ring->count--;
        ok = true;
    }
    taskEXIT_CRITICAL(&ring->lock);
    return ok;
}

void ic_pcm_frame_ring_clear(ic_pcm_frame_ring_t *ring)
{
    if (!ring) {
        return;
    }

    taskENTER_CRITICAL(&ring->lock);
    ring->read_index = 0;
    ring->write_index = 0;
    ring->count = 0;
    taskEXIT_CRITICAL(&ring->lock);
}

size_t ic_pcm_frame_ring_count(ic_pcm_frame_ring_t *ring)
{
    if (!ring) {
        return 0;
    }
    taskENTER_CRITICAL(&ring->lock);
    size_t count = ring->count;
    taskEXIT_CRITICAL(&ring->lock);
    return count;
}

void ic_pcm_frame_ring_stats(ic_pcm_frame_ring_t *ring, size_t *count, uint32_t *underflows, uint32_t *overflows)
{
    if (!ring) {
        if (count) {
            *count = 0;
        }
        if (underflows) {
            *underflows = 0;
        }
        if (overflows) {
            *overflows = 0;
        }
        return;
    }

    taskENTER_CRITICAL(&ring->lock);
    if (count) {
        *count = ring->count;
    }
    if (underflows) {
        *underflows = ring->underflows;
    }
    if (overflows) {
        *overflows = ring->overflows;
    }
    taskEXIT_CRITICAL(&ring->lock);
}
