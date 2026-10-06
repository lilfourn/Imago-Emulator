//! Vertex lighting, computed by the NORMAL command (GBATEK, "DS 3D Polygon
//! Light Parameters").
//!
//! Vectors arrive already multiplied by the directional matrix, with 12
//! fractional bits. Colours are 5 bits per channel. The line of sight is
//! always (0, 0, -1), so each light's half vector is fixed when its
//! direction is set.

use serde::{Deserialize, Serialize};

use super::matrix::ONE;

/// The red, green and blue fields of a BGR555 value.
pub(crate) fn rgb5(value: u32) -> [u32; 3] {
    [value & 0x1F, (value >> 5) & 0x1F, (value >> 10) & 0x1F]
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
struct Light {
    direction: [i32; 3],
    /// (direction + line of sight) / 2.
    half: [i32; 3],
    color: [u32; 3],
}

/// Lights 0–3 and the material.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Lighting {
    lights: [Light; 4],
    diffuse: [u32; 3],
    ambient: [u32; 3],
    specular: [u32; 3],
    emission: [u32; 3],
    shininess_enabled: bool,
    /// 128 entries, 8 fractional bits each.
    shininess: Vec<u8>,
}

impl Default for Lighting {
    fn default() -> Self {
        Self {
            lights: [Light::default(); 4],
            diffuse: [0; 3],
            ambient: [0; 3],
            specular: [0; 3],
            emission: [0; 3],
            shininess_enabled: false,
            shininess: vec![0; 128],
        }
    }
}

fn dot(a: [i32; 3], b: [i32; 3]) -> i64 {
    a.iter()
        .zip(b)
        .map(|(&a, b)| i64::from(a) * i64::from(b))
        .sum()
}

impl Lighting {
    /// LIGHT_VECTOR, after the directional matrix.
    pub(crate) fn set_direction(&mut self, light: usize, direction: [i32; 3]) {
        let [x, y, z] = direction;
        let light = &mut self.lights[light & 3];
        light.direction = direction;
        light.half = [x >> 1, y >> 1, (z - ONE) >> 1];
    }

    /// LIGHT_COLOR.
    pub(crate) fn set_color(&mut self, light: usize, color: u32) {
        self.lights[light & 3].color = rgb5(color);
    }

    /// DIF_AMB. Returns the new vertex colour if bit 15 asks to set it.
    pub(crate) fn set_diffuse_ambient(&mut self, param: u32) -> Option<u32> {
        self.diffuse = rgb5(param);
        self.ambient = rgb5(param >> 16);
        (param & 0x8000 != 0).then_some(param & 0x7FFF)
    }

    /// SPE_EMI.
    pub(crate) fn set_specular_emission(&mut self, param: u32) {
        self.specular = rgb5(param);
        self.shininess_enabled = param & 0x8000 != 0;
        self.emission = rgb5(param >> 16);
    }

    /// SHININESS: 32 words of four entries, low byte first.
    pub(crate) fn set_shininess(&mut self, params: &[u32]) {
        for (entries, param) in self.shininess.chunks_exact_mut(4).zip(params) {
            entries.copy_from_slice(&param.to_le_bytes());
        }
    }

    /// The vertex colour (BGR555) for `normal` lit by the lights in
    /// `enabled` (POLYGON_ATTR bits 0–3).
    pub(crate) fn color(&self, normal: [i32; 3], enabled: u32) -> u32 {
        // Every term is scaled to 17 fractional bits: 5 for the colour
        // product and 12 for the levels.
        let mut sum = [0i64; 3];
        for (index, light) in self.lights.iter().enumerate() {
            if enabled & (1 << index) == 0 {
                continue;
            }
            let diffuse = (-dot(light.direction, normal) >> 12).clamp(0, i64::from(ONE));
            let facing = (-dot(light.half, normal) >> 12).clamp(0, i64::from(ONE));
            let shine = (facing * facing) >> 12;
            let level = if self.shininess_enabled {
                i64::from(self.shininess[(shine >> 5).min(127) as usize])
            } else {
                (shine >> 4).min(255)
            };
            for (channel, total) in sum.iter_mut().enumerate() {
                let light_color = i64::from(light.color[channel]);
                *total += (i64::from(self.specular[channel]) * light_color * level) << 4;
                *total += i64::from(self.diffuse[channel]) * light_color * diffuse;
                *total += (i64::from(self.ambient[channel]) * light_color) << 12;
            }
        }
        let channel = |index: usize| {
            let value = i64::from(self.emission[index]) + (sum[index] >> 17);
            value.min(31) as u32
        };
        channel(0) | channel(1) << 5 | channel(2) << 10
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emission_alone_without_lights() {
        let mut lighting = Lighting::default();
        lighting.set_specular_emission(0x1F << 16);
        assert_eq!(lighting.color([0, 0, ONE], 0b1111), 0x1F);
    }

    #[test]
    fn diffuse_follows_the_angle_to_the_light() {
        let mut lighting = Lighting::default();
        lighting.set_color(0, 0x7FFF);
        lighting.set_diffuse_ambient(0x7FFF);
        // Light pointing down onto an upward-facing surface.
        lighting.set_direction(0, [0, -ONE, 0]);
        let lit = lighting.color([0, ONE, 0], 1);
        assert_eq!(lit & 0x1F, 30, "31 * 31 / 32");
        // Facing away receives nothing.
        assert_eq!(lighting.color([0, -ONE, 0], 1), 0);
        // Disabled lights do nothing.
        assert_eq!(lighting.color([0, ONE, 0], 0b1110), 0);
    }

    #[test]
    fn specular_peaks_when_the_normal_faces_the_viewer() {
        let mut lighting = Lighting::default();
        lighting.set_color(1, 0x7FFF);
        lighting.set_specular_emission(0x001F);
        lighting.set_direction(1, [0, 0, -ONE]);
        assert_eq!(lighting.color([0, 0, ONE], 0b10) & 0x1F, 29);
        // With the table enabled, its last entry decides the peak.
        let mut table = [0u32; 32];
        table[31] = 0x8000_0000;
        lighting.set_shininess(&table);
        lighting.set_specular_emission(0x801F);
        assert_eq!(lighting.color([0, 0, ONE], 0b10) & 0x1F, 15);
    }
}
