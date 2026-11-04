// Pixel Art beside larger 3D (docs/PRD-upscaler.md, UP-13).
//
// A DS drawing its 3D `cell` times larger enlarges everything else as
// squares of `cell` × `cell` pixels. Where a console pixel's square is all
// one colour, the larger 3D added nothing there, so OmniScale can draw it
// from the console's pixels without smearing 3D. Near squares that do hold
// detail, the picture is the larger frame, drawn smooth.
//
// Three passes: split the frame into console pixels, mark those near 3D
// detail, then draw each output pixel. Compiled after omniscale.metal,
// whose `scale` and GAMMA these use.

// How many console pixels around 3D detail keep the smooth picture.
// OmniScale reads two pixels out, so none of what it draws comes from
// 3D.
constant int MARGIN = 2;

// Splits a frame drawn `cell` times larger into console pixels: each the
// middle of its square, as `video::shrink` takes it, with alpha 0 where the
// square holds more than one colour.
kernel void split_cells(texture2d<float, access::read> large [[texture(0)]],
                        texture2d<float, access::write> console [[texture(1)]],
                        constant uint &cell [[buffer(0)]],
                        uint2 gid [[thread_position_in_grid]])
{
    if (gid.x >= console.get_width() || gid.y >= console.get_height()) {
        return;
    }
    uint2 origin = gid * cell;
    float4 first = large.read(origin);
    bool detail = false;
    for (uint y = 0; y < cell && !detail; y++) {
        for (uint x = 0; x < cell; x++) {
            if (any(large.read(origin + uint2(x, y)) != first)) {
                detail = true;
                break;
            }
        }
    }
    float4 middle = large.read(origin + uint2(cell / 2));
    console.write(float4(middle.rgb, detail ? 0.0 : 1.0), gid);
}

// The console pixels again, with alpha 0 within MARGIN console pixels of
// 3D detail: worked out once per console pixel rather than once per
// output pixel, a sixteenth to a fiftieth of the work.
kernel void near_3d(texture2d<float, access::read> split [[texture(0)]],
                    texture2d<float, access::write> console [[texture(1)]],
                    uint2 gid [[thread_position_in_grid]])
{
    uint2 size = uint2(console.get_width(), console.get_height());
    if (gid.x >= size.x || gid.y >= size.y) {
        return;
    }
    int2 last = int2(size) - 1;
    bool near = false;
    for (int dy = -MARGIN; dy <= MARGIN && !near; dy++) {
        for (int dx = -MARGIN; dx <= MARGIN; dx++) {
            int2 at = clamp(int2(gid) + int2(dx, dy), int2(0), last);
            if (split.read(uint2(at)).a < 0.5) {
                near = true;
                break;
            }
        }
    }
    console.write(float4(split.read(gid).rgb, near ? 0.0 : 1.0), gid);
}

// One thread per output pixel: OmniScale from the console pixels, or the
// smooth picture near 3D detail.
kernel void beside_3d(texture2d<float, access::read> console [[texture(0)]],
                      texture2d<float> smooth [[texture(1)]],
                      texture2d<float, access::write> target [[texture(2)]],
                      uint2 gid [[thread_position_in_grid]])
{
    uint2 out_size = uint2(target.get_width(), target.get_height());
    if (gid.x >= out_size.x || gid.y >= out_size.y) {
        return;
    }
    float2 input_resolution = float2(console.get_width(), console.get_height());
    float2 output_resolution = float2(out_size);
    float2 position = (float2(gid) + 0.5) / output_resolution;
    uint2 pixel = uint2(min(floor(position * input_resolution), input_resolution - 1.0));
    float4 color;
    if (console.read(pixel).a < 0.5) {
        constexpr sampler linear(filter::linear, address::clamp_to_edge);
        color = smooth.sample(linear, position, level(0));
    } else {
        color = pow(scale(console, position, input_resolution, output_resolution), 1.0 / GAMMA);
    }
    target.write(float4(color.rgb, 1.0), gid);
}
