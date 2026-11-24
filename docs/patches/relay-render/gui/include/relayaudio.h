// SPDX-License-Identifier: AGPL-3.0-only
#pragma once

#include <algorithm>
#include <chrono>
#include <cstdint>
#include <cstring>
#include <mutex>
#include <vector>

// One bounded PCM queue, pulled by the audio device. Never stack a software
// drain queue ahead of SDL's queue or let a stalled device replay old audio.
class RelayAudioBuffer {
public:
    struct Stats {
        unsigned rate = 0, channels = 0, period_frames = 0;
        size_t queued_frames = 0, capacity_frames = 0, peak_frames = 0;
        uint64_t dropped_frames = 0, underruns = 0, played_frames = 0, callbacks = 0;
    };

    static int64_t nowMs() {
        return std::chrono::duration_cast<std::chrono::milliseconds>(
            std::chrono::steady_clock::now().time_since_epoch()).count();
    }

    void reset(unsigned rate, unsigned channels, unsigned period) {
        std::lock_guard<std::mutex> lock(mutex);
        stats = {};
        stats.rate = rate;
        stats.channels = channels;
        stats.period_frames = period;
        // 20 ms startup jitter allowance; 40 ms absolute application queue.
        // Some devices require a larger hardware period, so adapt to it.
        target = std::max<size_t>(rate / 50, period);
        stats.capacity_frames = std::max<size_t>(rate / 25, 2 * period);
        pcm.assign(stats.capacity_frames * channels, 0);
        previous.assign(channels, 0);
        fade_origin.assign(channels, 0);
        read_pos = write_pos = fill = 0;
        playing = false;
        last_push_ms = -1;
        fade_frames = (std::max)(1u, rate / 1000); // 1 ms de-click, only at a gap/drop.
        fade_pos = 0;
    }

    void push(const void *data, size_t bytes, int64_t now = nowMs()) {
        std::lock_guard<std::mutex> lock(mutex);
        const size_t stride = stats.channels * sizeof(int16_t);
        if (!stride || bytes % stride || !data) return;
        size_t frames = bytes / stride;
        if (!frames || pcm.empty()) return;
        expire(now);
        const auto *source = static_cast<const int16_t *>(data);
        if (frames > stats.capacity_frames || fill + frames > stats.capacity_frames) {
            // Keep the newest 20 ms after a producer/device stall. Frame alignment
            // is in sample frames (all channels), never arbitrary byte offsets.
            const size_t keep_new = (std::min)(frames, target);
            const size_t keep_old = (std::min)(fill, target - keep_new);
            stats.dropped_frames += fill - keep_old + frames - keep_new;
            read_pos = (read_pos + fill - keep_old) % stats.capacity_frames;
            fill = keep_old;
            source += (frames - keep_new) * stats.channels;
            frames = keep_new;
            write_pos = (read_pos + fill) % stats.capacity_frames;
            beginFade();
        }
        const size_t first = (std::min)(frames, stats.capacity_frames - write_pos);
        std::memcpy(pcm.data() + write_pos * stats.channels, source, first * stride);
        if (frames > first)
            std::memcpy(pcm.data(), source + first * stats.channels, (frames - first) * stride);
        write_pos = (write_pos + frames) % stats.capacity_frames;
        fill += frames;
        stats.peak_frames = (std::max)(stats.peak_frames, fill);
        last_push_ms = now;
    }

    void read(void *data, size_t bytes, int64_t now = nowMs()) {
        std::memset(data, 0, bytes);
        std::lock_guard<std::mutex> lock(mutex);
        ++stats.callbacks;
        const size_t stride = stats.channels * sizeof(int16_t);
        if (!stride || pcm.empty()) return;
        expire(now);
        const size_t requested = bytes / stride;
        if (!playing) {
            if (fill < target) return;
            playing = true;
            beginFade();
        }
        const size_t count = (std::min)(requested, fill);
        auto *output = static_cast<int16_t *>(data);
        for (size_t f = 0; f < count; ++f) {
            for (unsigned ch = 0; ch < stats.channels; ++ch) {
                int value = pcm[read_pos * stats.channels + ch];
                if (fade_pos < fade_frames)
                    value = (int(fade_origin[ch]) * int(fade_frames - fade_pos - 1)
                           + value * int(fade_pos + 1)) / int(fade_frames);
                output[f * stats.channels + ch] = static_cast<int16_t>(value);
                previous[ch] = static_cast<int16_t>(value);
            }
            if (fade_pos < fade_frames) ++fade_pos;
            read_pos = (read_pos + 1) % stats.capacity_frames;
        }
        fill -= count;
        stats.played_frames += count;
        if (count < requested) {
            ++stats.underruns;
            // Fade the last available millisecond to zero; silence initializes
            // the remainder. Re-prime on real data, never repeat stale samples.
            const size_t fade = std::min<size_t>(count, fade_frames);
            for (size_t f = 0; f < fade; ++f)
                for (unsigned ch = 0; ch < stats.channels; ++ch) {
                    auto &value = output[(count - fade + f) * stats.channels + ch];
                    value = static_cast<int16_t>(int(value) * int(fade - f - 1) / int(fade));
                }
            std::fill(previous.begin(), previous.end(), 0);
            playing = false;
        }
    }

    Stats snapshot() const {
        std::lock_guard<std::mutex> lock(mutex);
        Stats result = stats;
        result.queued_frames = fill;
        return result;
    }

private:
    void beginFade() {
        std::copy(previous.begin(), previous.end(), fade_origin.begin());
        fade_pos = 0;
    }
    void expire(int64_t now) {
        if (fill && last_push_ms >= 0 && now - last_push_ms > 80) {
            stats.dropped_frames += fill;
            read_pos = write_pos = fill = 0;
            playing = false;
            std::fill(previous.begin(), previous.end(), 0);
            beginFade();
        }
    }
    mutable std::mutex mutex;
    Stats stats;
    std::vector<int16_t> pcm, previous, fade_origin;
    size_t read_pos = 0, write_pos = 0, fill = 0, target = 0;
    unsigned fade_frames = 1, fade_pos = 0;
    bool playing = false;
    int64_t last_push_ms = -1;
};
