//! Fixed-point matrices, the four current matrices and their stacks
//! (GBATEK, "DS 3D Matrix Load/Multiply", "DS 3D Matrix Types" and "DS 3D
//! Matrix Stack").
//!
//! Elements are 20.12 fixed point. Vectors are rows multiplied on the
//! left (`v' = v * M`), and every MTX_MULT-style command sets `C = M * C`.
//! Products keep their full 24-bit fractions until each sum is shifted
//! back to 12 bits, as the hardware's multiply unit does.

use emu_state::payload;
use serde::{Deserialize, Serialize};

/// 1.0 in 20.12 fixed point.
pub(crate) const ONE: i32 = 1 << 12;

/// A 4×4 matrix, `self.0[row][column]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Matrix(pub(crate) [[i32; 4]; 4]);

impl Default for Matrix {
    fn default() -> Self {
        Self::IDENTITY
    }
}

impl Matrix {
    pub(crate) const IDENTITY: Self = Self([
        [ONE, 0, 0, 0],
        [0, ONE, 0, 0],
        [0, 0, ONE, 0],
        [0, 0, 0, ONE],
    ]);

    /// `self * other`.
    pub(crate) fn mul(&self, other: &Self) -> Self {
        let mut out = [[0; 4]; 4];
        for (row, out_row) in self.0.iter().zip(&mut out) {
            for (column, value) in out_row.iter_mut().enumerate() {
                let sum: i64 = row
                    .iter()
                    .zip(&other.0)
                    .map(|(&a, b)| i64::from(a) * i64::from(b[column]))
                    .sum();
                *value = (sum >> 12) as i32;
            }
        }
        Self(out)
    }

    /// MTX_LOAD_4x4 / MTX_MULT_4x4 parameters, row by row.
    pub(crate) fn from_4x4(params: &[u32]) -> Self {
        let mut out = Self::IDENTITY;
        for (index, &param) in params.iter().take(16).enumerate() {
            out.0[index / 4][index % 4] = param as i32;
        }
        out
    }

    /// MTX_LOAD_4x3 / MTX_MULT_4x3 parameters: four rows of three, the
    /// last column (0, 0, 0, 1).
    pub(crate) fn from_4x3(params: &[u32]) -> Self {
        let mut out = Self::IDENTITY;
        for (index, &param) in params.iter().take(12).enumerate() {
            out.0[index / 3][index % 3] = param as i32;
        }
        out
    }

    /// MTX_MULT_3x3 parameters: the upper-left 3×3.
    pub(crate) fn from_3x3(params: &[u32]) -> Self {
        let mut out = Self::IDENTITY;
        for (index, &param) in params.iter().take(9).enumerate() {
            out.0[index / 3][index % 3] = param as i32;
        }
        out
    }

    /// MTX_SCALE parameters.
    pub(crate) fn scale(params: &[u32]) -> Self {
        let mut out = Self::IDENTITY;
        for (axis, &param) in params.iter().take(3).enumerate() {
            out.0[axis][axis] = param as i32;
        }
        out
    }

    /// MTX_TRANS parameters.
    pub(crate) fn translation(params: &[u32]) -> Self {
        let mut out = Self::IDENTITY;
        for (axis, &param) in params.iter().take(3).enumerate() {
            out.0[3][axis] = param as i32;
        }
        out
    }

    /// `(x, y, z, 1.0) * self`, for 20.12 inputs.
    pub(crate) fn transform_point(&self, point: [i32; 3]) -> [i32; 4] {
        let m = &self.0;
        std::array::from_fn(|column| {
            let sum = i64::from(point[0]) * i64::from(m[0][column])
                + i64::from(point[1]) * i64::from(m[1][column])
                + i64::from(point[2]) * i64::from(m[2][column])
                + (i64::from(m[3][column]) << 12);
            (sum >> 12) as i32
        })
    }

    /// `(x, y, z) * the upper-left 3×3`; the input's fraction carries
    /// through.
    pub(crate) fn transform_direction(&self, vector: [i32; 3]) -> [i32; 3] {
        let m = &self.0;
        std::array::from_fn(|column| {
            let sum = i64::from(vector[0]) * i64::from(m[0][column])
                + i64::from(vector[1]) * i64::from(m[1][column])
                + i64::from(vector[2]) * i64::from(m[2][column]);
            (sum >> 12) as i32
        })
    }
}

/// The current projection, position, vector (directional) and texture
/// matrices, their stacks, and the clip matrix derived from them.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Matrices {
    /// MTX_MODE: 0 projection, 1 position, 2 position and vector,
    /// 3 texture.
    mode: u8,
    pub(crate) projection: Matrix,
    pub(crate) position: Matrix,
    pub(crate) vector: Matrix,
    pub(crate) texture: Matrix,
    projection_stack: Matrix,
    /// 1-bit pointer.
    projection_pointer: u8,
    position_stack: [Matrix; 32],
    vector_stack: [Matrix; 32],
    /// 6-bit pointer; entries 31–63 are errors that mirror 0–31.
    #[serde(with = "payload::Below::<64>")]
    position_pointer: u8,
    texture_stack: Matrix,
    texture_pointer: u8,
    /// GXSTAT bit 15.
    pub(crate) stack_error: bool,
    /// `position * projection` when `clip_dirty` is false.
    clip: Matrix,
    clip_dirty: bool,
}

impl Default for Matrices {
    fn default() -> Self {
        Self {
            mode: 0,
            projection: Matrix::IDENTITY,
            position: Matrix::IDENTITY,
            vector: Matrix::IDENTITY,
            texture: Matrix::IDENTITY,
            projection_stack: Matrix::IDENTITY,
            projection_pointer: 0,
            position_stack: [Matrix::IDENTITY; 32],
            vector_stack: [Matrix::IDENTITY; 32],
            position_pointer: 0,
            texture_stack: Matrix::IDENTITY,
            texture_pointer: 0,
            stack_error: false,
            clip: Matrix::IDENTITY,
            clip_dirty: false,
        }
    }
}

impl Matrices {
    /// MTX_MODE.
    pub(crate) fn set_mode(&mut self, param: u32) {
        self.mode = (param & 3) as u8;
    }

    /// The clip matrix, `position * projection`, recomputed if needed.
    pub(crate) fn clip(&mut self) -> &Matrix {
        if self.clip_dirty {
            self.clip = self.position.mul(&self.projection);
            self.clip_dirty = false;
        }
        &self.clip
    }

    /// The clip matrix without updating the cache (for register reads).
    pub(crate) fn clip_now(&self) -> Matrix {
        if self.clip_dirty {
            self.position.mul(&self.projection)
        } else {
            self.clip
        }
    }

    /// GXSTAT bits 8–13: the position and projection stack levels.
    pub(crate) fn stack_levels(&self) -> u32 {
        u32::from(self.position_pointer & 0x1F) | u32::from(self.projection_pointer) << 5
    }

    /// Writing 1 to GXSTAT bit 15: clears the error and the projection and
    /// texture stack pointers.
    pub(crate) fn acknowledge_error(&mut self) {
        self.stack_error = false;
        self.projection_pointer = 0;
        self.texture_pointer = 0;
    }

    /// Sets the current matrix (or matrices, in mode 2) to `matrix`.
    pub(crate) fn load(&mut self, matrix: Matrix) {
        match self.mode {
            0 => self.projection = matrix,
            1 => self.position = matrix,
            2 => {
                self.position = matrix;
                self.vector = matrix;
            }
            _ => self.texture = matrix,
        }
        self.clip_dirty |= self.mode != 3;
    }

    /// `C = matrix * C` for the current matrix (or matrices).
    pub(crate) fn multiply(&mut self, matrix: &Matrix) {
        match self.mode {
            0 => self.projection = matrix.mul(&self.projection),
            1 => self.position = matrix.mul(&self.position),
            2 => {
                self.position = matrix.mul(&self.position);
                self.vector = matrix.mul(&self.vector);
            }
            _ => self.texture = matrix.mul(&self.texture),
        }
        self.clip_dirty |= self.mode != 3;
    }

    /// MTX_SCALE: like [`Self::multiply`], but mode 2 leaves the vector
    /// matrix alone so light vectors keep their length.
    pub(crate) fn scale(&mut self, matrix: &Matrix) {
        if self.mode == 2 {
            self.position = matrix.mul(&self.position);
            self.clip_dirty = true;
        } else {
            self.multiply(matrix);
        }
    }

    /// MTX_PUSH.
    pub(crate) fn push(&mut self) {
        match self.mode {
            0 => {
                self.stack_error |= self.projection_pointer != 0;
                self.projection_stack = self.projection;
                self.projection_pointer ^= 1;
            }
            1 | 2 => {
                let pointer = self.position_pointer;
                self.stack_error |= pointer >= 31;
                self.position_stack[usize::from(pointer & 0x1F)] = self.position;
                self.vector_stack[usize::from(pointer & 0x1F)] = self.vector;
                self.position_pointer = (pointer + 1) & 0x3F;
            }
            _ => {
                self.stack_error |= self.texture_pointer != 0;
                self.texture_stack = self.texture;
                self.texture_pointer ^= 1;
            }
        }
    }

    /// MTX_POP: a 6-bit signed offset in the position stacks; always one
    /// level in the others.
    pub(crate) fn pop(&mut self, param: u32) {
        match self.mode {
            0 => {
                self.projection_pointer ^= 1;
                self.stack_error |= self.projection_pointer != 0;
                self.projection = self.projection_stack;
                self.clip_dirty = true;
            }
            1 | 2 => {
                let offset = ((param as i8) << 2) >> 2;
                let pointer = (self.position_pointer as i8).wrapping_sub(offset) as u8 & 0x3F;
                self.stack_error |= pointer >= 31;
                self.position_pointer = pointer;
                self.position = self.position_stack[usize::from(pointer & 0x1F)];
                self.vector = self.vector_stack[usize::from(pointer & 0x1F)];
                self.clip_dirty = true;
            }
            _ => {
                self.texture_pointer ^= 1;
                self.stack_error |= self.texture_pointer != 0;
                self.texture = self.texture_stack;
            }
        }
    }

    /// MTX_STORE: to stack entry `param` (0–30; 31 is an error).
    pub(crate) fn store(&mut self, param: u32) {
        match self.mode {
            0 => self.projection_stack = self.projection,
            1 | 2 => {
                let index = (param & 0x1F) as usize;
                self.stack_error |= index == 31;
                self.position_stack[index] = self.position;
                self.vector_stack[index] = self.vector;
            }
            _ => self.texture_stack = self.texture,
        }
    }

    /// MTX_RESTORE: from stack entry `param`.
    pub(crate) fn restore(&mut self, param: u32) {
        match self.mode {
            0 => {
                self.projection = self.projection_stack;
                self.clip_dirty = true;
            }
            1 | 2 => {
                let index = (param & 0x1F) as usize;
                self.stack_error |= index == 31;
                self.position = self.position_stack[index];
                self.vector = self.vector_stack[index];
                self.clip_dirty = true;
            }
            _ => self.texture = self.texture_stack,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixed(value: i32) -> u32 {
        (value * ONE) as u32
    }

    #[test]
    fn multiply_keeps_full_fractions_until_the_end() {
        // 0.5 * 0.5 + 0.5 * 0.5 = 0.5, with no rounding loss.
        let half = ONE / 2;
        let a = Matrix([
            [half, half, 0, 0],
            [0, ONE, 0, 0],
            [0, 0, ONE, 0],
            [0, 0, 0, ONE],
        ]);
        let b = Matrix([
            [half, 0, 0, 0],
            [half, ONE, 0, 0],
            [0, 0, ONE, 0],
            [0, 0, 0, ONE],
        ]);
        assert_eq!(a.mul(&b).0[0], [half, half, 0, 0]);
        assert_eq!(Matrix::IDENTITY.mul(&a), a);
    }

    #[test]
    fn mult_premultiplies_the_current_matrix() {
        let mut matrices = Matrices::default();
        matrices.set_mode(1);
        matrices.load(Matrix::translation(&[fixed(1), fixed(2), fixed(3)]));
        matrices.scale(&Matrix::scale(&[fixed(2), fixed(2), fixed(2)]));
        // Scale first, then translate: (1, 1, 1) -> (3, 4, 5).
        let point = matrices.clip().transform_point([ONE, ONE, ONE]);
        assert_eq!(
            point,
            [fixed(3) as i32, fixed(4) as i32, fixed(5) as i32, ONE]
        );
    }

    #[test]
    fn scale_in_mode_2_leaves_the_vector_matrix() {
        let mut matrices = Matrices::default();
        matrices.set_mode(2);
        matrices.scale(&Matrix::scale(&[fixed(2), fixed(2), fixed(2)]));
        matrices.multiply(&Matrix::translation(&[fixed(1), 0, 0]));
        assert_eq!(matrices.vector.0[0][0], ONE);
        assert_eq!(matrices.vector.0[3][0], fixed(1) as i32);
        assert_eq!(matrices.position.0[0][0], fixed(2) as i32);
    }

    #[test]
    fn position_stack_pushes_pops_and_flags_overflow() {
        let mut matrices = Matrices::default();
        matrices.set_mode(1);
        let moved = Matrix::translation(&[fixed(5), 0, 0]);
        matrices.push(); // Level 1 holds the identity.
        matrices.load(moved);
        matrices.push(); // Level 2 holds `moved`.
        matrices.load(Matrix::IDENTITY);
        assert_eq!(matrices.stack_levels(), 2);
        matrices.pop(2);
        assert_eq!(matrices.position, Matrix::IDENTITY);
        assert_eq!(matrices.vector, Matrix::IDENTITY, "mode 1 pops both");
        matrices.pop(0x3F); // -1: back up a level.
        assert_eq!(matrices.position, moved);
        assert!(!matrices.stack_error);
        for _ in 0..31 {
            matrices.push();
        }
        assert!(matrices.stack_error);
        matrices.acknowledge_error();
        assert!(!matrices.stack_error);
    }

    #[test]
    fn projection_stack_has_one_level() {
        let mut matrices = Matrices::default();
        matrices.load(Matrix::scale(&[fixed(3), fixed(3), fixed(3)]));
        matrices.push();
        matrices.load(Matrix::IDENTITY);
        assert_eq!(matrices.stack_levels(), 1 << 5);
        matrices.pop(0);
        assert_eq!(matrices.projection.0[0][0], fixed(3) as i32);
        assert_eq!(matrices.stack_levels(), 0);
        assert!(!matrices.stack_error);
        matrices.pop(0);
        assert!(matrices.stack_error, "popping an empty stack underflows");
    }

    #[test]
    fn store_and_restore_use_absolute_indices() {
        let mut matrices = Matrices::default();
        matrices.set_mode(2);
        let moved = Matrix::translation(&[0, fixed(1), 0]);
        matrices.load(moved);
        matrices.store(7);
        matrices.load(Matrix::IDENTITY);
        matrices.restore(7);
        assert_eq!((matrices.position, matrices.vector), (moved, moved));
        assert_eq!(matrices.stack_levels(), 0);
        matrices.store(31);
        assert!(matrices.stack_error);
    }
}
