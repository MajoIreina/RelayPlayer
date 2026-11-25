#pragma once

#include <cmath>
#include <libplacebo/renderer.h>
#include <libplacebo/shaders/colorspace.h>
#include <libplacebo/tone_mapping.h>

// A real color conversion, before SDR-only enhancement engines. Keep the
// decoded HDR frame intact; filters and exposure belong to the final pass.
inline bool relayHdrToSdr(pl_gpu gpu, pl_renderer renderer, const pl_frame &source,
                          pl_tex *texture, pl_frame *output)
{
    const int w = static_cast<int>(std::lround(std::fabs(pl_rect_w(source.crop))));
    const int h = static_cast<int>(std::lround(std::fabs(pl_rect_h(source.crop))));
    if (source.num_planes <= 0 || w <= 0 || h <= 0)
        return false;
    pl_tex_params tp = {};
    tp.w = w;
    tp.h = h;
    tp.format = pl_find_named_fmt(gpu, "rgba16hf");
    tp.renderable = true;
    tp.sampleable = true;
    if (!tp.format || !pl_tex_recreate(gpu, texture, &tp))
        return false;
    pl_frame target = {};
    target.num_planes = 1;
    target.planes[0].texture = *texture;
    target.planes[0].components = 4;
    for (int i = 0; i < 4; ++i)
        target.planes[0].component_mapping[i] = i;
    target.repr = pl_color_repr_rgb;
    target.color = pl_color_space_bt709;
    target.color.transfer = PL_COLOR_TRC_BT_1886;
    target.color.hdr.max_luma = PL_COLOR_SDR_WHITE;
    target.crop = {0, 0, static_cast<float>(w), static_cast<float>(h)};
    auto color = pl_color_map_default_params;
    color.tone_mapping_function = &pl_tone_map_bt2390;
    auto params = pl_render_fast_params;
    params.color_map_params = &color;
    params.peak_detect_params = nullptr;
    params.dither_params = nullptr;
    if (!pl_render_image(renderer, &source, &target, &params))
        return false;
    *output = target;
    return true;
}
