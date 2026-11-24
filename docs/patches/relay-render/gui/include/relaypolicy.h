#pragma once

#include <algorithm>
#include <cmath>

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
