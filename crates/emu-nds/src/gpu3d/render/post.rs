//! Whole-frame passes after the polygons are drawn: edge marking and fog
//! (GBATEK, "DS 3D Toon, Edge, Fog, Alpha-Blending, Anti-Aliasing").

use super::pixel::Buffers;
use super::raster::{HEIGHT, WIDTH};
use crate::gpu3d::color;
use crate::gpu3d::registers::RenderConfig;

/// Paints opaque pixels at the boundary of their polygon ID in the edge
/// colour for that ID: where a neighbour (up, down, left, right) has
/// another ID and is further away. Beyond the screen, neighbours are the
/// rear plane's ID and depth.
pub(crate) fn mark_edges(buffers: &mut Buffers, config: &RenderConfig) {
    let clear_id = ((config.clear_color >> 24) & 0x3F) as u8;
    let edge_colors = config.edge_colors.map(|c| color::rgb6(u32::from(c)));
    let Buffers {
        color: pixels,
        depth,
        attrs,
        ..
    } = buffers;
    let neighbour = |x: Option<usize>, y: Option<usize>| match (x, y) {
        (Some(x), Some(y)) if x < WIDTH && y < HEIGHT => {
            let index = y * WIDTH + x;
            (attrs[index].opaque_id, depth[index])
        }
        _ => (clear_id, config.clear_depth),
    };
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            let index = y * WIDTH + x;
            let attr = attrs[index];
            if !attr.opaque {
                continue;
            }
            let here = depth[index];
            let edge = [
                neighbour(x.checked_sub(1), Some(y)),
                neighbour(Some(x + 1), Some(y)),
                neighbour(Some(x), y.checked_sub(1)),
                neighbour(Some(x), Some(y + 1)),
            ]
            .into_iter()
            .any(|(id, there)| id != attr.opaque_id && here < there);
            if edge {
                let rgb = edge_colors[usize::from(attr.opaque_id >> 3)];
                pixels[index] = color::pack(rgb, color::alpha(pixels[index]));
            }
        }
    }
}

/// Fog density, 0–128, at 24-bit `depth`: FOG_TABLE entries sit at
/// `FOG_OFFSET + FOG_STEP * (n + 1)`, with linear steps between them.
pub(crate) fn fog_density(depth: u32, config: &RenderConfig) -> u32 {
    let density = |n: usize| match u32::from(config.fog_table[n]) {
        127 => 128,
        value => value,
    };
    let offset = config.fog_offset << 9;
    let step = (0x400 >> config.fog_shift.min(11)) << 9;
    if step == 0 {
        return if depth <= offset {
            density(0)
        } else {
            density(31)
        };
    }
    let relative = depth.saturating_sub(offset);
    match (relative / step) as usize {
        0 => density(0),
        n @ 1..=31 => {
            let fraction = relative % step;
            (density(n - 1) * (step - fraction) + density(n) * fraction) / step
        }
        _ => density(31),
    }
}

/// Blends fog into every pixel whose fog flag is set.
pub(crate) fn apply_fog(buffers: &mut Buffers, config: &RenderConfig) {
    let fog_rgb = color::rgb6(config.fog_color);
    let fog_alpha = (config.fog_color >> 16) & 0x1F;
    for ((pixel, &depth), attr) in buffers
        .color
        .iter_mut()
        .zip(&*buffers.depth)
        .zip(&*buffers.attrs)
    {
        if !attr.fog {
            continue;
        }
        let density = fog_density(depth, config);
        let mix = |fog: u32, value: u32| ((fog * density + value * (128 - density)) >> 7) as u8;
        let rgb = color::unpack(*pixel);
        let rgb = if config.fog_alpha_only {
            rgb
        } else {
            std::array::from_fn(|i| mix(u32::from(fog_rgb[i]), u32::from(rgb[i])))
        };
        *pixel = color::pack(rgb, mix(fog_alpha, u32::from(color::alpha(*pixel))));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gpu3d::registers::RenderRegisters;

    fn config(fog_offset: u16, shift: u16, table: &[(u32, u8)]) -> RenderConfig {
        let mut registers = RenderRegisters::default();
        registers.write_control(shift << 8, 0xF00);
        let [low, high] = fog_offset.to_le_bytes();
        registers.write_byte(0x35C, low);
        registers.write_byte(0x35D, high);
        for &(index, value) in table {
            registers.write_byte(0x360 + index, value);
        }
        registers.config()
    }

    #[test]
    fn fog_density_steps_through_the_table() {
        // Step 0x400 >> 2 = 0x100 (15-bit depth); entry n sits at
        // 0x1000 + 0x100 * (n + 1).
        let config = config(0x1000, 2, &[(0, 0), (1, 64), (31, 127)]);
        assert_eq!(fog_density(0x0800 << 9, &config), 0, "before the offset");
        assert_eq!(fog_density(0x1100 << 9, &config), 0, "at entry 0");
        assert_eq!(fog_density(0x1180 << 9, &config), 32, "halfway to entry 1");
        assert_eq!(fog_density(0x1200 << 9, &config), 64, "at entry 1");
        assert_eq!(fog_density(0x7FFF << 9, &config), 128, "127 counts as 128");
    }
}
