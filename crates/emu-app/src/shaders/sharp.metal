// Sharp Pixels, the default for pixel-art games (docs/PRD-upscaler.md):
// the game's own pixels, each a crisp square at the output's exact size.
//
// Plain nearest-neighbour makes pixels uneven at a size that is not a
// whole multiple, and bilinear blurs them. Here each output pixel takes the
// console pixel under its middle, except the one output pixel straddling
// an edge between two, which mixes them by how much of it each covers: the
// pixels stay even and sharp at any size, with no staircase from uneven
// widths. At a whole multiple it is exactly nearest-neighbour. Mixing is
// in linear light, so edges keep their brightness. Shrinking, it is
// bilinear.
//
// Compiled after omniscale.metal, whose GAMMA this uses.

// One thread per output pixel: `target` is the frame drawn at its own size.
kernel void sharp(texture2d<float, access::read> source [[texture(0)]],
                  texture2d<float, access::write> target [[texture(1)]],
                  uint2 gid [[thread_position_in_grid]])
{
    uint2 out_size = uint2(target.get_width(), target.get_height());
    if (gid.x >= out_size.x || gid.y >= out_size.y) {
        return;
    }
    float2 in_size = float2(source.get_width(), source.get_height());
    // Output pixels to a console pixel, along each side.
    float2 scale = float2(out_size) / in_size;
    // The output pixel's middle, in console pixels; the console pixel whose
    // middle is up and to the left of it; and how far it is towards the
    // next one's middle, 0 to 1.
    float2 texel = (float2(gid) + 0.5) / scale;
    float2 base = floor(texel - 0.5);
    float2 along = texel - 0.5 - base;
    // Only the output pixel across the edge, half way, mixes.
    along = clamp((along - 0.5) * max(scale, 1.0) + 0.5, 0.0, 1.0);
    int2 last = int2(in_size) - 1;
    int2 first = clamp(int2(base), int2(0), last);
    int2 second = clamp(int2(base) + 1, int2(0), last);
    float4 top = mix(pow(source.read(uint2(first.x, first.y)), GAMMA),
                     pow(source.read(uint2(second.x, first.y)), GAMMA), along.x);
    float4 bottom = mix(pow(source.read(uint2(first.x, second.y)), GAMMA),
                        pow(source.read(uint2(second.x, second.y)), GAMMA), along.x);
    float4 color = pow(mix(top, bottom, along.y), 1.0 / GAMMA);
    target.write(float4(color.rgb, 1.0), gid);
}
