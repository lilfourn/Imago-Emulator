// OmniScale, the Pixel Art upscaler (docs/PRD-upscaler.md, decision 0013).
//
// Ported to a Metal compute kernel from SameBoy's Shaders/OmniScale.fsh and
// Shaders/MasterShader.metal (https://github.com/LIJI32/SameBoy). The
// algorithm and its constants are unchanged. Texels are read by index,
// clamped to the frame, instead of sampled, and in linear light as SameBoy
// does (gamma 2.2); frame blending is left out. SameBoy's licence:
//
// Expat License
//
// Copyright (c) 2015-2026 Lior Halphon
//
// Permission is hereby granted, free of charge, to any person obtaining a copy
// of this software and associated documentation files (the "Software"), to deal
// in the Software without restriction, including without limitation the rights
// to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
// copies of the Software, and to permit persons to whom the Software is
// furnished to do so, subject to the following conditions:
//
// The above copyright notice and this permission notice shall be included in all
// copies or substantial portions of the Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
// IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
// FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
// AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
// LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
// OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
// SOFTWARE.

#include <metal_stdlib>
using namespace metal;

#define GAMMA 2.2

// A texel `offset` console pixels from the one under `position`, clamped
// to the frame, in linear light.
static float4 texture_relative(texture2d<float, access::read> image, float2 position, float2 offset)
{
    float2 size = float2(image.get_width(), image.get_height());
    float2 texel = clamp(floor(position * size) + offset, float2(0.0), size - 1.0);
    return pow(image.read(uint2(texel)), GAMMA);
}

static float3 rgb_to_hq_colospace(float4 rgb)
{
    return float3( 0.250 * rgb.r + 0.250 * rgb.g + 0.250 * rgb.b,
                   0.250 * rgb.r - 0.000 * rgb.g - 0.250 * rgb.b,
                  -0.125 * rgb.r + 0.250 * rgb.g - 0.125 * rgb.b);
}

static bool is_different(float4 a, float4 b)
{
    float3 diff = abs(rgb_to_hq_colospace(a) - rgb_to_hq_colospace(b));
    return diff.x > 0.018 || diff.y > 0.002 || diff.z > 0.005;
}

#define P(m, r) ((pattern & (m)) == (r))

// OmniScale is derived from the pattern-based design of HQnx, redesigned to
// draw at any factor: it works out which of 16 neighbours belong to an edge
// and draws that edge, anti-aliased, at the output's own resolution.
static float4 scale(texture2d<float, access::read> image, float2 position, float2 input_resolution, float2 output_resolution)
{
    float2 o = float2(1, 1);
    float2 p = fract(position * input_resolution);
    if (p.x > 0.5) { o.x = -o.x; p.x = 1.0 - p.x; }
    if (p.y > 0.5) { o.y = -o.y; p.y = 1.0 - p.y; }

    float4 w0 = texture_relative(image, position, float2( -o.x, -o.y));
    float4 w1 = texture_relative(image, position, float2(    0, -o.y));
    float4 w2 = texture_relative(image, position, float2(  o.x, -o.y));
    float4 w3 = texture_relative(image, position, float2( -o.x,    0));
    float4 w4 = texture_relative(image, position, float2(    0,    0));
    float4 w5 = texture_relative(image, position, float2(  o.x,    0));
    float4 w6 = texture_relative(image, position, float2( -o.x,  o.y));
    float4 w7 = texture_relative(image, position, float2(    0,  o.y));
    float4 w8 = texture_relative(image, position, float2(  o.x,  o.y));

    int pattern = 0;
    if (is_different(w0, w4)) pattern |= 1 << 0;
    if (is_different(w1, w4)) pattern |= 1 << 1;
    if (is_different(w2, w4)) pattern |= 1 << 2;
    if (is_different(w3, w4)) pattern |= 1 << 3;
    if (is_different(w5, w4)) pattern |= 1 << 4;
    if (is_different(w6, w4)) pattern |= 1 << 5;
    if (is_different(w7, w4)) pattern |= 1 << 6;
    if (is_different(w8, w4)) pattern |= 1 << 7;

    if ((P(0xBF,0x37) || P(0xDB,0x13)) && is_different(w1, w5)) {
        return mix(w4, w3, 0.5 - p.x);
    }
    if ((P(0xDB,0x49) || P(0xEF,0x6D)) && is_different(w7, w3)) {
        return mix(w4, w1, 0.5 - p.y);
    }
    if ((P(0x0B,0x0B) || P(0xFE,0x4A) || P(0xFE,0x1A)) && is_different(w3, w1)) {
        return w4;
    }
    if ((P(0x6F,0x2A) || P(0x5B,0x0A) || P(0xBF,0x3A) || P(0xDF,0x5A) ||
         P(0x9F,0x8A) || P(0xCF,0x8A) || P(0xEF,0x4E) || P(0x3F,0x0E) ||
         P(0xFB,0x5A) || P(0xBB,0x8A) || P(0x7F,0x5A) || P(0xAF,0x8A) ||
         P(0xEB,0x8A)) && is_different(w3, w1)) {
        return mix(w4, mix(w4, w0, 0.5 - p.x), 0.5 - p.y);
    }
    if (P(0x0B,0x08)) {
        return mix(mix(w0 * 0.375 + w1 * 0.25 + w4 * 0.375, w4 * 0.5 + w1 * 0.5, p.x * 2.0), w4, p.y * 2.0);
    }
    if (P(0x0B,0x02)) {
        return mix(mix(w0 * 0.375 + w3 * 0.25 + w4 * 0.375, w4 * 0.5 + w3 * 0.5, p.y * 2.0), w4, p.x * 2.0);
    }
    if (P(0x2F,0x2F)) {
        float dist = length(p - float2(0.5));
        float pixel_size = length(1.0 / (output_resolution / input_resolution));
        if (dist < 0.5 - pixel_size / 2.0) {
            return w4;
        }
        float4 r;
        if (is_different(w0, w1) || is_different(w0, w3)) {
            r = mix(w1, w3, p.y - p.x + 0.5);
        }
        else {
            r = mix(mix(w1 * 0.375 + w0 * 0.25 + w3 * 0.375, w3, p.y * 2.0), w1, p.x * 2.0);
        }
        if (dist > 0.5 + pixel_size / 2.0) {
            return r;
        }
        return mix(w4, r, (dist - 0.5 + pixel_size / 2.0) / pixel_size);
    }
    if (P(0xBF,0x37) || P(0xDB,0x13)) {
        float dist = p.x - 2.0 * p.y;
        float pixel_size = length(1.0 / (output_resolution / input_resolution)) * sqrt(5.0);
        if (dist > pixel_size / 2.0) {
            return w1;
        }
        float4 r = mix(w3, w4, p.x + 0.5);
        if (dist < -pixel_size / 2.0) {
            return r;
        }
        return mix(r, w1, (dist + pixel_size / 2.0) / pixel_size);
    }
    if (P(0xDB,0x49) || P(0xEF,0x6D)) {
        float dist = p.y - 2.0 * p.x;
        float pixel_size = length(1.0 / (output_resolution / input_resolution)) * sqrt(5.0);
        if (p.y - 2.0 * p.x > pixel_size / 2.0) {
            return w3;
        }
        float4 r = mix(w1, w4, p.x + 0.5);
        if (dist < -pixel_size / 2.0) {
            return r;
        }
        return mix(r, w3, (dist + pixel_size / 2.0) / pixel_size);
    }
    if (P(0xBF,0x8F) || P(0x7E,0x0E)) {
        float dist = p.x + 2.0 * p.y;
        float pixel_size = length(1.0 / (output_resolution / input_resolution)) * sqrt(5.0);
        if (dist > 1.0 + pixel_size / 2.0) {
            return w4;
        }
        float4 r;
        if (is_different(w0, w1) || is_different(w0, w3)) {
            r = mix(w1, w3, p.y - p.x + 0.5);
        }
        else {
            r = mix(mix(w1 * 0.375 + w0 * 0.25 + w3 * 0.375, w3, p.y * 2.0), w1, p.x * 2.0);
        }
        if (dist < 1.0 - pixel_size / 2.0) {
            return r;
        }
        return mix(r, w4, (dist + pixel_size / 2.0 - 1.0) / pixel_size);
    }
    if (P(0x7E,0x2A) || P(0xEF,0xAB)) {
        float dist = p.y + 2.0 * p.x;
        float pixel_size = length(1.0 / (output_resolution / input_resolution)) * sqrt(5.0);
        if (p.y + 2.0 * p.x > 1.0 + pixel_size / 2.0) {
            return w4;
        }
        float4 r;
        if (is_different(w0, w1) || is_different(w0, w3)) {
            r = mix(w1, w3, p.y - p.x + 0.5);
        }
        else {
            r = mix(mix(w1 * 0.375 + w0 * 0.25 + w3 * 0.375, w3, p.y * 2.0), w1, p.x * 2.0);
        }
        if (dist < 1.0 - pixel_size / 2.0) {
            return r;
        }
        return mix(r, w4, (dist + pixel_size / 2.0 - 1.0) / pixel_size);
    }
    if (P(0x1B,0x03) || P(0x4F,0x43) || P(0x8B,0x83) || P(0x6B,0x43)) {
        return mix(w4, w3, 0.5 - p.x);
    }
    if (P(0x4B,0x09) || P(0x8B,0x89) || P(0x1F,0x19) || P(0x3B,0x19)) {
        return mix(w4, w1, 0.5 - p.y);
    }
    if (P(0xFB,0x6A) || P(0x6F,0x6E) || P(0x3F,0x3E) || P(0xFB,0xFA) ||
        P(0xDF,0xDE) || P(0xDF,0x1E)) {
        return mix(w4, w0, (1.0 - p.x - p.y) / 2.0);
    }
    if (P(0x4F,0x4B) || P(0x9F,0x1B) || P(0x2F,0x0B) ||
        P(0xBE,0x0A) || P(0xEE,0x0A) || P(0x7E,0x0A) || P(0xEB,0x4B) ||
        P(0x3B,0x1B)) {
        float dist = p.x + p.y;
        float pixel_size = length(1.0 / (output_resolution / input_resolution));
        if (dist > 0.5 + pixel_size / 2.0) {
            return w4;
        }
        float4 r;
        if (is_different(w0, w1) || is_different(w0, w3)) {
            r = mix(w1, w3, p.y - p.x + 0.5);
        }
        else {
            r = mix(mix(w1 * 0.375 + w0 * 0.25 + w3 * 0.375, w3, p.y * 2.0), w1, p.x * 2.0);
        }
        if (dist < 0.5 - pixel_size / 2.0) {
            return r;
        }
        return mix(r, w4, (dist + pixel_size / 2.0 - 0.5) / pixel_size);
    }
    if (P(0x0B,0x01)) {
        return mix(mix(w4, w3, 0.5 - p.x), mix(w1, (w1 + w3) / 2.0, 0.5 - p.x), 0.5 - p.y);
    }
    if (P(0x0B,0x00)) {
        return mix(mix(w4, w3, 0.5 - p.x), mix(w1, w0, 0.5 - p.x), 0.5 - p.y);
    }

    float dist = p.x + p.y;
    float pixel_size = length(1.0 / (output_resolution / input_resolution));
    if (dist > 0.5 + pixel_size / 2.0) {
        return w4;
    }

    float4 x0 = texture_relative(image, position, float2( -o.x * 2.0, -o.y * 2.0));
    float4 x1 = texture_relative(image, position, float2( -o.x      , -o.y * 2.0));
    float4 x2 = texture_relative(image, position, float2(  0.0      , -o.y * 2.0));
    float4 x3 = texture_relative(image, position, float2(  o.x      , -o.y * 2.0));
    float4 x4 = texture_relative(image, position, float2( -o.x * 2.0, -o.y      ));
    float4 x5 = texture_relative(image, position, float2( -o.x * 2.0,  0.0      ));
    float4 x6 = texture_relative(image, position, float2( -o.x * 2.0,  o.y      ));

    if (is_different(x0, w4)) pattern |= 1 << 8;
    if (is_different(x1, w4)) pattern |= 1 << 9;
    if (is_different(x2, w4)) pattern |= 1 << 10;
    if (is_different(x3, w4)) pattern |= 1 << 11;
    if (is_different(x4, w4)) pattern |= 1 << 12;
    if (is_different(x5, w4)) pattern |= 1 << 13;
    if (is_different(x6, w4)) pattern |= 1 << 14;

    int diagonal_bias = -7;
    while (pattern != 0) {
        diagonal_bias += pattern & 1;
        pattern >>= 1;
    }

    if (diagonal_bias <= 0) {
        float4 r = mix(w1, w3, p.y - p.x + 0.5);
        if (dist < 0.5 - pixel_size / 2.0) {
            return r;
        }
        return mix(r, w4, (dist + pixel_size / 2.0 - 0.5) / pixel_size);
    }
    return w4;
}

// One thread per output pixel: `target` is the frame drawn at its own size.
kernel void omniscale(texture2d<float, access::read> source [[texture(0)]],
                      texture2d<float, access::write> target [[texture(1)]],
                      uint2 gid [[thread_position_in_grid]])
{
    uint2 out_size = uint2(target.get_width(), target.get_height());
    if (gid.x >= out_size.x || gid.y >= out_size.y) {
        return;
    }
    float2 input_resolution = float2(source.get_width(), source.get_height());
    float2 output_resolution = float2(out_size);
    float2 position = (float2(gid) + 0.5) / output_resolution;
    float4 color = scale(source, position, input_resolution, output_resolution);
    target.write(pow(color, 1.0 / GAMMA), gid);
}
