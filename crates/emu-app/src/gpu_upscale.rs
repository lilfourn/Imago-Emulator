//! The upscaler's GPU half ([decision 0013]): enlarges a frame to an exact
//! size in device pixels on Metal, with OmniScale for pixel art or MetalFX's
//! spatial scaler for smooth pictures, and reads the result back as the
//! BGRA bytes a GPUI image is made from.
//!
//! GPUI 0.2.2 draws only images it is handed as bytes, so each frame goes
//! up to the GPU and comes back: measured at 0.3–2.5 ms a frame on an
//! M5 Max, more the larger the output ([research]).
//!
//! objc2's Metal calls are `unsafe`, so this module is the app's exception
//! for them, as `emu-3ds` is for Vulkan. Every frame waits for its own
//! command buffer, so the CPU never touches a texture or buffer the GPU is
//! using.
//!
//! [decision 0013]: ../../../docs/decisions/0013-upscaling-on-the-gpu.md
//! [research]: ../../../docs/research/upscaling.md#what-was-tested

use emu_core::Frame;

pub use imp::{Gpu, GpuDevice};

/// How a frame is enlarged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    /// OmniScale: pixel art's edges and curves rebuilt at the output's own
    /// resolution.
    PixelArt,
    /// MetalFX's spatial scaler: smooth, for painted and rendered pictures.
    Smooth,
}

/// Why the GPU could not upscale.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GpuError {
    /// The Mac has no Metal device, or this is not a Mac.
    #[error("this Mac has no Metal GPU")]
    NoDevice,
    /// The device cannot run MetalFX.
    #[error("this Mac's GPU cannot run MetalFX")]
    NoMetalFx,
    /// Making a shader, texture, buffer or scaler failed.
    #[error("cannot set up the GPU: {0}")]
    Setup(String),
    /// The output is empty or larger than a texture can be.
    #[error("cannot upscale to {0}×{1}")]
    Size(u32, u32),
    /// The GPU reported an error running the frame.
    #[error("the GPU failed: {0}")]
    Failed(String),
}

/// The upscaled image's bytes: `width * height` pixels, BGRA.
pub type Bgra = Vec<u8>;

#[cfg(target_os = "macos")]
mod imp {
    use std::ffi::c_void;
    use std::ptr::NonNull;

    use objc2::rc::Retained;
    use objc2::runtime::ProtocolObject;
    use objc2_foundation::NSString;
    use objc2_metal::{
        MTLBlitCommandEncoder, MTLBuffer, MTLCommandBuffer, MTLCommandBufferStatus,
        MTLCommandEncoder, MTLCommandQueue, MTLComputeCommandEncoder, MTLComputePipelineState,
        MTLCreateSystemDefaultDevice, MTLDevice, MTLLibrary, MTLOrigin, MTLPixelFormat, MTLRegion,
        MTLResourceOptions, MTLSize, MTLStorageMode, MTLTexture, MTLTextureDescriptor,
        MTLTextureUsage,
    };
    use objc2_metal_fx::{
        MTLFXSpatialScaler, MTLFXSpatialScalerBase, MTLFXSpatialScalerColorProcessingMode,
        MTLFXSpatialScalerDescriptor,
    };

    use super::{Bgra, Frame, GpuError, Method};

    /// The shaders, compiled from source when the GPU is first used, as
    /// GPUI's are, so building needs no Metal toolchain.
    const SHADERS: &str = include_str!("shaders/omniscale.metal");

    /// The largest side of a texture on Apple GPUs.
    const MAX_SIDE: u32 = 16_384;

    /// How many sizes of upscale stay ready: two screens, and the sizes a
    /// window passes through while resized.
    const KEPT: usize = 4;

    /// Threads per threadgroup along each side.
    const GROUP_SIDE: usize = 16;

    /// The Metal device with the shaders compiled: the part that takes
    /// time to start (about 130 ms, mostly compiling), made on any thread.
    pub struct GpuDevice {
        device: Retained<ProtocolObject<dyn MTLDevice>>,
        queue: Retained<ProtocolObject<dyn MTLCommandQueue>>,
        omniscale: Retained<ProtocolObject<dyn MTLComputePipelineState>>,
        metal_fx: bool,
    }

    /// The upscaler on the GPU: the device and the textures and buffers it
    /// keeps for recent sizes, used from one thread.
    pub struct Gpu {
        device: GpuDevice,
        /// Recently used sizes, the most recent last.
        jobs: Vec<Job>,
    }

    /// One size of upscale: the frame's size, the output's and how.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    struct Key {
        method: Method,
        input: (u32, u32),
        output: (u32, u32),
    }

    /// What one size of upscale needs, kept for the next frame of the size.
    struct Job {
        key: Key,
        /// The frame, uploaded by the CPU.
        input: Retained<ProtocolObject<dyn MTLTexture>>,
        /// The upscaled frame, in the GPU's own memory.
        output: Retained<ProtocolObject<dyn MTLTexture>>,
        /// The upscaled frame copied where the CPU can read it.
        readback: Retained<ProtocolObject<dyn MTLBuffer>>,
        /// MetalFX's scaler, for [`Method::Smooth`].
        scaler: Option<Retained<ProtocolObject<dyn MTLFXSpatialScaler>>>,
    }

    impl GpuDevice {
        /// The system's Metal device, with the shaders compiled.
        ///
        /// # Errors
        ///
        /// [`GpuError::NoDevice`] without Metal, or [`GpuError::Setup`] if
        /// the shaders do not compile.
        pub fn start() -> Result<Self, GpuError> {
            let device = MTLCreateSystemDefaultDevice().ok_or(GpuError::NoDevice)?;
            let queue = device
                .newCommandQueue()
                .ok_or_else(|| setup("no command queue"))?;
            let library = device
                .newLibraryWithSource_options_error(&NSString::from_str(SHADERS), None)
                .map_err(|error| setup(&error.localizedDescription().to_string()))?;
            let function = library
                .newFunctionWithName(&NSString::from_str("omniscale"))
                .ok_or_else(|| setup("no omniscale kernel"))?;
            let omniscale = device
                .newComputePipelineStateWithFunction_error(&function)
                .map_err(|error| setup(&error.localizedDescription().to_string()))?;
            // SAFETY: asks whether `device`, a live Metal device, can run
            // MetalFX; reads nothing else.
            let metal_fx = unsafe { MTLFXSpatialScalerDescriptor::supportsDevice(&device) };
            Ok(Self {
                device,
                queue,
                omniscale,
                metal_fx,
            })
        }
    }

    impl Gpu {
        /// The upscaler on `device`, with no sizes kept yet.
        pub fn new(device: GpuDevice) -> Self {
            Self {
                device,
                jobs: Vec::new(),
            }
        }

        /// `frame` enlarged to `width` × `height` by `method`, as BGRA bytes.
        ///
        /// # Errors
        ///
        /// [`GpuError::Size`] for an empty or oversized output,
        /// [`GpuError::NoMetalFx`] for [`Method::Smooth`] on a GPU without
        /// MetalFX, and [`GpuError::Setup`] or [`GpuError::Failed`] if Metal
        /// fails.
        pub fn upscale(
            &mut self,
            frame: &Frame,
            method: Method,
            width: u32,
            height: u32,
        ) -> Result<Bgra, GpuError> {
            if width == 0 || height == 0 || width > MAX_SIDE || height > MAX_SIDE {
                return Err(GpuError::Size(width, height));
            }
            if method == Method::Smooth && !self.device.metal_fx {
                return Err(GpuError::NoMetalFx);
            }
            let size = frame.size();
            let key = Key {
                method,
                input: (u32::from(size.width), u32::from(size.height)),
                output: (width, height),
            };
            self.make_ready(key)?;
            let job = self.jobs.last().expect("make_ready leaves the job last");
            let (input, output) = (to_size(key.input), to_size(key.output));

            let pixels = frame.pixels();
            // SAFETY: `pixels` holds `width * height` pixels of 4 bytes, BGRA
            // in memory, which is the texture's format and the region given,
            // one row of `width * 4` bytes after another. The texture is in
            // shared memory, and no command buffer is using it: each
            // `upscale` waits for its own before returning.
            unsafe {
                job.input.replaceRegion_mipmapLevel_withBytes_bytesPerRow(
                    MTLRegion {
                        origin: MTLOrigin { x: 0, y: 0, z: 0 },
                        size: input,
                    },
                    0,
                    NonNull::from(pixels).cast::<c_void>(),
                    input.width * 4,
                );
            }

            let commands = self
                .device
                .queue
                .commandBuffer()
                .ok_or_else(|| setup("no command buffer"))?;
            match &job.scaler {
                Some(scaler) => {
                    // SAFETY: the scaler was made for exactly these textures'
                    // sizes, format and usage, and both live until the
                    // command buffer is done, which `upscale` waits for.
                    unsafe {
                        scaler.setColorTexture(Some(&job.input));
                        scaler.setOutputTexture(Some(&job.output));
                        scaler.setInputContentWidth(input.width);
                        scaler.setInputContentHeight(input.height);
                        scaler.encodeToCommandBuffer(&commands);
                    }
                }
                None => {
                    let encoder = commands
                        .computeCommandEncoder()
                        .ok_or_else(|| setup("no compute encoder"))?;
                    encoder.setComputePipelineState(&self.device.omniscale);
                    // SAFETY: indices 0 and 1 are the kernel's `source`
                    // (read) and `target` (write) textures, which these are
                    // made for, and both outlive the command buffer.
                    unsafe {
                        encoder.setTexture_atIndex(Some(&job.input), 0);
                        encoder.setTexture_atIndex(Some(&job.output), 1);
                    }
                    encoder.dispatchThreads_threadsPerThreadgroup(
                        output,
                        MTLSize {
                            width: GROUP_SIDE,
                            height: GROUP_SIDE,
                            depth: 1,
                        },
                    );
                    encoder.endEncoding();
                }
            }
            let blit = commands
                .blitCommandEncoder()
                .ok_or_else(|| setup("no blit encoder"))?;
            let row = output.width * 4;
            // SAFETY: copies the whole output texture, `output` in size,
            // into `readback`, which was made `row * height` bytes long for
            // this key; the texture has one slice and one level.
            unsafe {
                blit.copyFromTexture_sourceSlice_sourceLevel_sourceOrigin_sourceSize_toBuffer_destinationOffset_destinationBytesPerRow_destinationBytesPerImage(
                    &job.output,
                    0,
                    0,
                    MTLOrigin { x: 0, y: 0, z: 0 },
                    output,
                    &job.readback,
                    0,
                    row,
                    row * output.height,
                );
            }
            blit.endEncoding();
            commands.commit();
            commands.waitUntilCompleted();
            if commands.status() != MTLCommandBufferStatus::Completed {
                let why = commands.error().map_or_else(
                    || "unknown error".to_owned(),
                    |error| error.localizedDescription().to_string(),
                );
                return Err(GpuError::Failed(why));
            }
            let length = row * output.height;
            // SAFETY: `readback` is a shared buffer of `length` bytes, which
            // the finished command buffer wrote; nothing writes it until the
            // next `upscale`, and the slice is copied out at once.
            let bytes = unsafe {
                std::slice::from_raw_parts(job.readback.contents().as_ptr().cast::<u8>(), length)
            };
            Ok(bytes.to_vec())
        }

        /// The output sizes kept ready, the most recent last.
        #[cfg(test)]
        pub fn kept_outputs(&self) -> Vec<(u32, u32)> {
            self.jobs.iter().map(|job| job.key.output).collect()
        }

        /// Puts the job for `key` last in `jobs`, the most recent, making it
        /// if it is not kept.
        fn make_ready(&mut self, key: Key) -> Result<(), GpuError> {
            if let Some(index) = self.jobs.iter().position(|job| job.key == key) {
                let job = self.jobs.remove(index);
                self.jobs.push(job);
            } else {
                let job = self.make_job(key)?;
                if self.jobs.len() == KEPT {
                    self.jobs.remove(0);
                }
                self.jobs.push(job);
            }
            Ok(())
        }

        /// The textures, buffer and scaler `key` needs.
        fn make_job(&self, key: Key) -> Result<Job, GpuError> {
            let (input, output) = (to_size(key.input), to_size(key.output));
            let (scaler, input_usage, output_usage) = match key.method {
                Method::PixelArt => (
                    None,
                    MTLTextureUsage::ShaderRead,
                    MTLTextureUsage::ShaderWrite,
                ),
                Method::Smooth => {
                    let scaler = self.metal_fx_scaler(input, output)?;
                    // SAFETY: reads two properties of a live scaler.
                    let usages =
                        unsafe { (scaler.colorTextureUsage(), scaler.outputTextureUsage()) };
                    (Some(scaler), usages.0, usages.1)
                }
            };
            let readback = self
                .device
                .device
                .newBufferWithLength_options(
                    output.width * output.height * 4,
                    MTLResourceOptions::StorageModeShared,
                )
                .ok_or_else(|| setup("no readback buffer"))?;
            Ok(Job {
                key,
                input: self.texture(input, input_usage, MTLStorageMode::Shared)?,
                output: self.texture(output, output_usage, MTLStorageMode::Private)?,
                readback,
                scaler,
            })
        }

        /// A BGRA texture of `size`.
        fn texture(
            &self,
            size: MTLSize,
            usage: MTLTextureUsage,
            storage: MTLStorageMode,
        ) -> Result<Retained<ProtocolObject<dyn MTLTexture>>, GpuError> {
            // SAFETY: a plain descriptor for a 2D texture in a valid format,
            // whose sides `upscale` checked are 1 to MAX_SIDE.
            let descriptor = unsafe {
                MTLTextureDescriptor::texture2DDescriptorWithPixelFormat_width_height_mipmapped(
                    MTLPixelFormat::BGRA8Unorm,
                    size.width,
                    size.height,
                    false,
                )
            };
            descriptor.setUsage(usage);
            descriptor.setStorageMode(storage);
            self.device
                .device
                .newTextureWithDescriptor(&descriptor)
                .ok_or_else(|| setup("no texture"))
        }

        /// MetalFX's spatial scaler from `input` to `output`, for sRGB-encoded
        /// BGRA frames.
        fn metal_fx_scaler(
            &self,
            input: MTLSize,
            output: MTLSize,
        ) -> Result<Retained<ProtocolObject<dyn MTLFXSpatialScaler>>, GpuError> {
            // SAFETY: a new descriptor, given plain sizes and formats, then
            // asked for a scaler on this live device.
            unsafe {
                let descriptor = MTLFXSpatialScalerDescriptor::new();
                descriptor.setInputWidth(input.width);
                descriptor.setInputHeight(input.height);
                descriptor.setOutputWidth(output.width);
                descriptor.setOutputHeight(output.height);
                descriptor.setColorTextureFormat(MTLPixelFormat::BGRA8Unorm);
                descriptor.setOutputTextureFormat(MTLPixelFormat::BGRA8Unorm);
                descriptor
                    .setColorProcessingMode(MTLFXSpatialScalerColorProcessingMode::Perceptual);
                descriptor.newSpatialScalerWithDevice(&self.device.device)
            }
            .ok_or_else(|| setup("MetalFX made no scaler"))
        }
    }

    /// `(width, height)` as a Metal size.
    fn to_size((width, height): (u32, u32)) -> MTLSize {
        // u32 to usize is lossless on the 64-bit Macs the app runs on.
        MTLSize {
            width: width as usize,
            height: height as usize,
            depth: 1,
        }
    }

    fn setup(why: &str) -> GpuError {
        GpuError::Setup(why.to_owned())
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    use super::{Bgra, Frame, GpuError, Method};

    /// No GPU upscaling away from macOS: the picture stays on the CPU path.
    pub struct GpuDevice;

    impl GpuDevice {
        /// Always [`GpuError::NoDevice`].
        pub fn start() -> Result<Self, GpuError> {
            Err(GpuError::NoDevice)
        }
    }

    /// Never made: [`GpuDevice::start`] fails.
    pub struct Gpu;

    impl Gpu {
        /// The upscaler on `device`.
        pub fn new(_device: GpuDevice) -> Self {
            Self
        }

        /// Never called: there is no device to make a [`Gpu`] on.
        pub fn upscale(
            &mut self,
            _frame: &Frame,
            _method: Method,
            _width: u32,
            _height: u32,
        ) -> Result<Bgra, GpuError> {
            Err(GpuError::NoDevice)
        }
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    use emu_core::{ScreenSize, rgb};

    /// The GPU, or `None` where there is none (as on a CI runner without
    /// one), so GPU tests skip themselves.
    fn gpu() -> Option<Gpu> {
        match GpuDevice::start() {
            Ok(device) => Some(Gpu::new(device)),
            Err(GpuError::NoDevice) => None,
            Err(error) => panic!("the GPU is there but will not start: {error}"),
        }
    }

    fn pixel(bytes: &[u8], width: u32, x: u32, y: u32) -> [u8; 4] {
        let at = ((y * width + x) * 4) as usize;
        bytes[at..at + 4].try_into().expect("four bytes")
    }

    #[test]
    fn a_flat_frame_stays_flat_at_any_size_and_both_methods() {
        let Some(mut gpu) = gpu() else { return };
        let mut frame = Frame::new(ScreenSize::new(16, 12));
        frame.pixels_mut().fill(rgb(200, 120, 40));
        for method in [Method::PixelArt, Method::Smooth] {
            for (width, height) in [(80, 60), (123, 77), (16 * 13, 12 * 13)] {
                let bytes = gpu
                    .upscale(&frame, method, width, height)
                    .expect("upscaled");
                assert_eq!(bytes.len(), (width * height * 4) as usize);
                for (x, y) in [(0, 0), (width / 2, height / 2), (width - 1, height - 1)] {
                    let [b, g, r, a] = pixel(&bytes, width, x, y);
                    // BGRA in memory, as the frame's own pixels are.
                    let near = |got: u8, want: u8| got.abs_diff(want) <= 1;
                    assert!(
                        near(r, 200) && near(g, 120) && near(b, 40) && a == 255,
                        "{method:?} at {width}×{height}, ({x}, {y}): {r} {g} {b} {a}"
                    );
                }
            }
        }
    }

    #[test]
    fn pixel_art_keeps_flat_areas_and_straightens_a_staircase() {
        let Some(mut gpu) = gpu() else { return };
        // A black staircase on white, one pixel a step.
        let (white, black) = (rgb(255, 255, 255), rgb(0, 0, 0));
        let mut frame = Frame::new(ScreenSize::new(8, 8));
        for (y, row) in frame.rows_mut().enumerate() {
            for (x, pixel) in row.iter_mut().enumerate() {
                *pixel = if x <= y { black } else { white };
            }
        }
        let scale = 8;
        let side = 8 * scale;
        let bytes = gpu
            .upscale(&frame, Method::PixelArt, side, side)
            .expect("upscaled");
        let green = |x: u32, y: u32| pixel(&bytes, side, x, y)[1];
        // Away from the edge, pixels keep their colour.
        assert!(green(4, side - 4) < 8, "inside the black");
        assert!(green(side - 4, 4) > 247, "inside the white");
        // Enlarged plainly, the edge in each output row would stay put for
        // a whole console row, then jump by `scale`. OmniScale draws a
        // straight diagonal: across one console row, the edge moves along
        // with nearly every output row.
        let edge = |y: u32| (0..side).find(|&x| green(x, y) > 128).unwrap_or(side);
        let positions: std::collections::BTreeSet<u32> = (3 * scale..4 * scale).map(edge).collect();
        assert!(
            positions.len() >= 4,
            "the edge stays a staircase: {positions:?}"
        );
    }

    #[test]
    fn sizes_are_kept_and_reused() {
        let Some(mut gpu) = gpu() else { return };
        let frame = Frame::new(ScreenSize::new(10, 10));
        for side in [20, 30, 40, 50, 60, 20] {
            gpu.upscale(&frame, Method::PixelArt, side, side)
                .expect("upscaled");
        }
        assert_eq!(gpu.kept_outputs(), [(40, 40), (50, 50), (60, 60), (20, 20)]);
    }

    #[test]
    fn rejects_empty_and_oversized_outputs() {
        let Some(mut gpu) = gpu() else { return };
        let frame = Frame::new(ScreenSize::new(4, 4));
        assert_eq!(
            gpu.upscale(&frame, Method::PixelArt, 0, 10),
            Err(GpuError::Size(0, 10))
        );
        assert_eq!(
            gpu.upscale(&frame, Method::Smooth, 20_000, 10),
            Err(GpuError::Size(20_000, 10))
        );
    }
}

/// The device starts on a background thread and moves to the window's.
#[cfg(target_os = "macos")]
const _: () = {
    const fn assert_send<T: Send>() {}
    assert_send::<GpuDevice>();
};
