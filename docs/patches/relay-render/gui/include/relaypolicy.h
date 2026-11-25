#pragma once

#include <algorithm>
#include <cmath>

// Reason codes cross the render/UI threads as one atomic snapshot.
// A skipped HDR source is a compatibility limitation, not a no-scaling result.
inline int relayUpscalerStatus(bool enabled, bool active, bool has_source, bool scaling,
                              bool vsr, bool hdr, bool failed, bool superseded)
{
    if (!enabled) return 0;
    if (active) return 1;
    if (!has_source) return 2;
    if (superseded) return 7;
    if (!scaling) return 3;
    if (vsr && hdr) return 4;
    if (failed) return 5;
    return 6; // Frame processing did not succeed; never claim the algorithm is active.
}

// The Relay multiplier is per distinct source picture, including a deduplicated source.
// Cadence detection must never silently turn the user's 2x selection into 4x.
struct RelayFrameGenPlan {
    int multiplier;
    int limited_request;
    int needed_hz;
};

inline RelayFrameGenPlan relayFrameGenPlan(int requested, int cadence, double source_interval,
                                         double refresh_hz, int backend_max, bool player_owned)
{
    const int selected = std::clamp(requested, 2, 4);
    const int wanted = std::min(selected * (player_owned ? 1 : std::clamp(cadence, 1, 2)),
                                std::clamp(backend_max, 1, 4));
    const int refresh_cap = source_interval > 0.0 && std::isfinite(source_interval) &&
                            refresh_hz > 0.0 && std::isfinite(refresh_hz)
        ? static_cast<int>(std::min(4.0, std::floor(refresh_hz * source_interval + 0.15))) : 1;
    const bool limited = refresh_cap < wanted && source_interval > 0.0;
    return {std::min(wanted, std::max(1, refresh_cap)), limited ? wanted : 0,
            limited ? static_cast<int>(std::lround(wanted / source_interval)) : 0};
}
