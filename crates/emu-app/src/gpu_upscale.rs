//! The upscaler's GPU half ([decision 0013]): draws a frame at an exact
//! size in device pixels on Metal, as crisp pixels (the default for pixel
//! art), smooth (MetalFX's spatial scaler), or rounded (OmniScale), and
//! reads the result back as the BGRA bytes a GPUI image is made from.
//!
//! GPUI 0.2.2 draws only images it is handed as bytes, so each frame goes
//! up to the GPU and comes back: measured at 0.3–2.5 ms a frame on an
//! M5 Max, more the larger the output ([research]).
//!
//! objc2's Metal calls are `unsafe`, so this module is the app's exception
//! for them, as `emu-3ds` is for Vulkan. One batch of screens is on the
//! GPU at a time ([`Gpu::submit`]), and the next is not uploaded until it
//! has finished ([`Gpu::finish`]), so the CPU never touches a texture or
//! buffer the GPU is using. The caller waits for a batch as long as it
//! likes: with other apps busy on the GPU, a batch that usually takes a
//! millisecond took up to 30, which the window should not wait out.
//!
//! [decision 0013]: ../../../docs/decisions/0013-upscaling-on-the-gpu.md
//! [research]: ../../../docs/research/upscaling.md#what-was-tested

use emu_core::Frame;

pub use imp::{Gpu, GpuDevice};

/// One screen to upscale: `frame`, with `cell` of its pixels to a console
/// pixel, enlarged by `method` to `width` × `height`.
// Only the Metal half reads it all.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
#[derive(Debug, Clone, Copy)]
pub struct Request<'a> {
    /// The screen as the core drew it, colour corrected and turned.
    pub frame: &'a Frame,
    /// How it is enlarged.
    pub method: Method,
    /// The frame's pixels to a console pixel along each side: more than 1
    /// for a DS drawing 3D larger.
    pub cell: u32,
    /// The output's width in device pixels.
    pub width: u32,
    /// The output's height in device pixels.
    pub height: u32,
}

/// How a frame is enlarged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    /// The game's own pixels as crisp, even squares at any size
    /// (`shaders/sharp.metal`).
    Sharp,
    /// MetalFX's spatial scaler: smooth, for painted and rendered pictures.
    Smooth,
    /// OmniScale: pixel art's edges reshaped into smooth curves and lines,
    /// for those who want it; it changes how the art looks.
    Rounded,
}

/// Why the GPU could not upscale.
// Off macOS there is never a device, so only `NoDevice` is made.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
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
    use std::time::{Duration, Instant};

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

    #[cfg(test)]
    use super::Frame;
    use super::{Bgra, GpuError, Method, Request};

    /// The shaders, compiled from source when the GPU is first used, as
    /// GPUI's are, so building needs no Metal toolchain.
    const SHADERS: &str = concat!(
        include_str!("shaders/omniscale.metal"),
        include_str!("shaders/sharp.metal"),
        include_str!("shaders/beside_3d.metal")
    );

    /// The largest side of a texture on Apple GPUs.
    const MAX_SIDE: u32 = 16_384;

    /// How many sizes of upscale stay ready: a few screens, and the sizes a
    /// window passes through while resized.
    const KEPT: usize = 8;

    /// Threads per threadgroup along each side.
    const GROUP_SIDE: usize = 16;

    /// The Metal device with the shaders compiled: the part that takes
    /// time to start (about 130 ms, mostly compiling), made on any thread.
    pub struct GpuDevice {
        device: Retained<ProtocolObject<dyn MTLDevice>>,
        queue: Retained<ProtocolObject<dyn MTLCommandQueue>>,
        sharp: Retained<ProtocolObject<dyn MTLComputePipelineState>>,
        omniscale: Retained<ProtocolObject<dyn MTLComputePipelineState>>,
        split_cells: Retained<ProtocolObject<dyn MTLComputePipelineState>>,
        near_3d: Retained<ProtocolObject<dyn MTLComputePipelineState>>,
        beside_3d: Retained<ProtocolObject<dyn MTLComputePipelineState>>,
        metal_fx: bool,
    }

    /// The upscaler on the GPU: the device and the textures and buffers it
    /// keeps for recent sizes, used from one thread.
    pub struct Gpu {
        device: GpuDevice,
        /// Recently used sizes, the most recent last.
        jobs: Vec<Job>,
        /// The batch on the GPU, until [`Gpu::finish`] takes it.
        in_flight: Option<InFlight>,
    }

    /// A batch of screens on the GPU: its commands, and its jobs' keys, in
    /// the order the screens were asked for.
    struct InFlight {
        commands: Retained<ProtocolObject<dyn MTLCommandBuffer>>,
        keys: Vec<Key>,
    }

    /// One size of upscale: which of a frame's screens, how, the frame's
    /// size and pixels to a console pixel, and the output's size. Screens
    /// upscaled together each have their own textures.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    struct Key {
        slot: usize,
        method: Method,
        cell: u32,
        input: (u32, u32),
        output: (u32, u32),
    }

    impl Key {
        /// Whether this is Rounded Pixels beside larger 3D.
        fn beside_3d(self) -> bool {
            self.method == Method::Rounded && self.cell > 1
        }

        /// The console pixels' size: the frame's over the cell.
        fn console(self) -> (u32, u32) {
            (self.input.0 / self.cell, self.input.1 / self.cell)
        }
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
        /// MetalFX's scaler: for [`Method::Smooth`], and for the 3D beside
        /// Rounded Pixels when the output is larger than the frame.
        scaler: Option<Retained<ProtocolObject<dyn MTLFXSpatialScaler>>>,
        /// Beside larger 3D: the console pixels, alpha 0 where 3D added
        /// detail (`split_cells`), then alpha 0 near it (`near_3d`).
        console: Option<[Retained<ProtocolObject<dyn MTLTexture>>; 2]>,
        /// Beside larger 3D: the frame enlarged by `scaler`.
        smooth: Option<Retained<ProtocolObject<dyn MTLTexture>>>,
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
            let kernel = |name: &str| {
                let function = library
                    .newFunctionWithName(&NSString::from_str(name))
                    .ok_or_else(|| setup(&format!("no {name} kernel")))?;
                device
                    .newComputePipelineStateWithFunction_error(&function)
                    .map_err(|error| setup(&error.localizedDescription().to_string()))
            };
            let (sharp, omniscale, split_cells, near_3d, beside_3d) = (
                kernel("sharp")?,
                kernel("omniscale")?,
                kernel("split_cells")?,
                kernel("near_3d")?,
                kernel("beside_3d")?,
            );
            // SAFETY: asks whether `device`, a live Metal device, can run
            // MetalFX; reads nothing else.
            let metal_fx = unsafe { MTLFXSpatialScalerDescriptor::supportsDevice(&device) };
            Ok(Self {
                device,
                queue,
                sharp,
                omniscale,
                split_cells,
                near_3d,
                beside_3d,
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
                in_flight: None,
            }
        }

        /// `frame` enlarged to `width` × `height` by `method`, as BGRA bytes:
        /// [`Gpu::upscale_all`] with one screen.
        ///
        /// # Errors
        ///
        /// As [`Gpu::upscale_all`].
        #[cfg(test)]
        pub fn upscale(
            &mut self,
            frame: &Frame,
            method: Method,
            cell: u32,
            width: u32,
            height: u32,
        ) -> Result<Bgra, GpuError> {
            let request = Request {
                frame,
                method,
                cell,
                width,
                height,
            };
            let mut images = self.upscale_all(&[request])?;
            Ok(images.remove(0))
        }

        /// Each request's screen enlarged, as BGRA bytes, in order:
        /// [`Gpu::submit`], then [`Gpu::finish`] waiting as long as it takes.
        ///
        /// # Errors
        ///
        /// As [`Gpu::submit`] and [`Gpu::finish`].
        #[cfg(test)]
        pub fn upscale_all(&mut self, requests: &[Request<'_>]) -> Result<Vec<Bgra>, GpuError> {
            assert!(self.submit(requests)?, "nothing else is on the GPU");
            self.finish(None).expect("a batch was submitted")
        }

        /// Whether a batch is on the GPU, not yet taken by [`Gpu::finish`].
        pub fn busy(&self) -> bool {
            self.in_flight.is_some()
        }

        /// Whether the batch on the GPU has finished, for [`Gpu::finish`] to
        /// take without waiting; false with none.
        pub fn finished(&self) -> bool {
            self.in_flight.as_ref().is_some_and(|in_flight| {
                matches!(
                    in_flight.commands.status(),
                    MTLCommandBufferStatus::Completed | MTLCommandBufferStatus::Error
                )
            })
        }

        /// Sends each request's screen to the GPU to be enlarged, in one
        /// command buffer that [`Gpu::finish`] waits for: the wait, not the
        /// work, is most of an upscale's time, so a frame's screens share
        /// it. A frame drawn with 3D `cell` times larger than the console's
        /// pixels gets Rounded Pixels only where 3D added no detail
        /// (`beside_3d.metal`). Returns false, sending nothing, while an
        /// earlier batch is still on the GPU.
        ///
        /// # Errors
        ///
        /// [`GpuError::Size`] for an empty or oversized output,
        /// [`GpuError::NoMetalFx`] for [`Method::Smooth`] on a GPU without
        /// MetalFX, and [`GpuError::Setup`] or [`GpuError::Failed`] if Metal
        /// fails.
        pub fn submit(&mut self, requests: &[Request<'_>]) -> Result<bool, GpuError> {
            if self.in_flight.is_some() {
                return Ok(false);
            }
            let mut keys = Vec::with_capacity(requests.len());
            for (slot, request) in requests.iter().enumerate() {
                let (width, height) = (request.width, request.height);
                if width == 0 || height == 0 || width > MAX_SIDE || height > MAX_SIDE {
                    return Err(GpuError::Size(width, height));
                }
                if request.method == Method::Smooth && !self.device.metal_fx {
                    return Err(GpuError::NoMetalFx);
                }
                let size = request.frame.size();
                let input = (u32::from(size.width), u32::from(size.height));
                // A frame that is not whole squares of `cell` is taken as it
                // is.
                let cell = request.cell;
                let whole = cell > 1 && input.0 % cell == 0 && input.1 % cell == 0;
                keys.push(Key {
                    slot,
                    method: request.method,
                    cell: if whole { cell } else { 1 },
                    input,
                    output: (width, height),
                });
            }
            for &key in &keys {
                self.make_ready(key, &keys)?;
            }
            let jobs: Vec<&Job> = keys
                .iter()
                .map(|key| {
                    self.jobs
                        .iter()
                        .find(|job| job.key == *key)
                        .expect("make_ready keeps every job of the batch")
                })
                .collect();

            for (job, request) in jobs.iter().zip(requests) {
                let input = to_size(job.key.input);
                let pixels = request.frame.pixels();
                // SAFETY: `pixels` holds `width * height` pixels of 4 bytes,
                // BGRA in memory, which is the texture's format and the region
                // given, one row of `width * 4` bytes after another. The
                // texture is in shared memory, and no command buffer is using
                // it: a batch is submitted only once the last has finished.
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
            }

            let commands = self
                .device
                .queue
                .commandBuffer()
                .ok_or_else(|| setup("no command buffer"))?;
            for job in &jobs {
                self.encode(&commands, job)?;
            }
            commands.commit();
            self.in_flight = Some(InFlight { commands, keys });
            Ok(true)
        }

        /// The submitted batch's screens, as BGRA bytes in the order asked
        /// for, once the GPU has finished them: waiting up to `wait` for
        /// that, or with `None` as long as it takes. `None` if nothing was
        /// submitted, or it is not finished in time; it stays on the GPU
        /// for a later call to take.
        ///
        /// # Errors
        ///
        /// [`GpuError::Failed`] if the GPU reported an error running it.
        pub fn finish(&mut self, wait: Option<Duration>) -> Option<Result<Vec<Bgra>, GpuError>> {
            let in_flight = self.in_flight.as_ref()?;
            let done = |commands: &ProtocolObject<dyn MTLCommandBuffer>| {
                matches!(
                    commands.status(),
                    MTLCommandBufferStatus::Completed | MTLCommandBufferStatus::Error
                )
            };
            match wait {
                None => in_flight.commands.waitUntilCompleted(),
                Some(wait) => {
                    let deadline = Instant::now() + wait;
                    while !done(&in_flight.commands) {
                        if Instant::now() >= deadline {
                            return None;
                        }
                        // A tenth of a millisecond's nap: Metal's own wait
                        // has no time limit.
                        std::thread::sleep(Duration::from_micros(100));
                    }
                }
            }
            let InFlight { commands, keys } = self.in_flight.take()?;
            if commands.status() != MTLCommandBufferStatus::Completed {
                let why = commands.error().map_or_else(
                    || "unknown error".to_owned(),
                    |error| error.localizedDescription().to_string(),
                );
                return Some(Err(GpuError::Failed(why)));
            }
            Some(Ok(keys
                .iter()
                .map(|key| {
                    let job = self
                        .jobs
                        .iter()
                        .find(|job| job.key == *key)
                        .expect("no job goes while its batch is on the GPU");
                    let output = to_size(job.key.output);
                    let length = output.width * output.height * 4;
                    // SAFETY: `readback` is a shared buffer of `length` bytes,
                    // which the finished command buffer wrote; nothing writes
                    // it until the next batch is submitted, and the slice is
                    // copied out at once.
                    let bytes = unsafe {
                        std::slice::from_raw_parts(
                            job.readback.contents().as_ptr().cast::<u8>(),
                            length,
                        )
                    };
                    bytes.to_vec()
                })
                .collect()))
        }

        /// Encodes `job`'s passes into `commands`: its upscale, then the copy
        /// of the output where the CPU can read it.
        fn encode(
            &self,
            commands: &ProtocolObject<dyn MTLCommandBuffer>,
            job: &Job,
        ) -> Result<(), GpuError> {
            let key = job.key;
            let (input, output) = (to_size(key.input), to_size(key.output));
            match (key.method, &job.console) {
                (Method::Sharp, _) => dispatch(
                    commands,
                    &self.device.sharp,
                    &[&job.input, &job.output],
                    None,
                    output,
                )?,
                (Method::Smooth, _) => {
                    let scaler = job.scaler.as_ref().expect("Smooth jobs have a scaler");
                    encode_metal_fx(commands, scaler, &job.input, &job.output, input);
                }
                (Method::Rounded, Some([split, console])) => {
                    let console_size = to_size(key.console());
                    dispatch(
                        commands,
                        &self.device.split_cells,
                        &[&job.input, split],
                        Some(key.cell),
                        console_size,
                    )?;
                    dispatch(
                        commands,
                        &self.device.near_3d,
                        &[split, console],
                        None,
                        console_size,
                    )?;
                    // The 3D's part, smooth: MetalFX's enlargement, or the
                    // frame itself, sampled, where that is about as large.
                    let smooth = match (&job.scaler, &job.smooth) {
                        (Some(scaler), Some(smooth)) => {
                            encode_metal_fx(commands, scaler, &job.input, smooth, input);
                            smooth
                        }
                        _ => &job.input,
                    };
                    dispatch(
                        commands,
                        &self.device.beside_3d,
                        &[console, smooth, &job.output],
                        None,
                        output,
                    )?;
                }
                (Method::Rounded, None) => dispatch(
                    commands,
                    &self.device.omniscale,
                    &[&job.input, &job.output],
                    None,
                    output,
                )?,
            }
            let blit = commands
                .blitCommandEncoder()
                .ok_or_else(|| setup("no blit encoder"))?;
            let row = output.width * 4;
            // SAFETY: copies the whole output texture, `output` in size, into
            // `readback`, which was made `row * height` bytes long for this
            // key; the texture has one slice and one level.
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
            Ok(())
        }

        /// The output sizes kept ready, the most recent last.
        #[cfg(test)]
        pub fn kept_outputs(&self) -> Vec<(u32, u32)> {
            self.jobs.iter().map(|job| job.key.output).collect()
        }

        /// Puts the job for `key` last in `jobs`, the most recent, making it
        /// if it is not kept. Past [`KEPT`], the oldest job goes, unless it is
        /// one of `batch`'s.
        fn make_ready(&mut self, key: Key, batch: &[Key]) -> Result<(), GpuError> {
            if let Some(index) = self.jobs.iter().position(|job| job.key == key) {
                let job = self.jobs.remove(index);
                self.jobs.push(job);
                return Ok(());
            }
            let job = self.make_job(key)?;
            if self.jobs.len() >= KEPT
                && let Some(oldest) = self.jobs.iter().position(|job| !batch.contains(&job.key))
            {
                self.jobs.remove(oldest);
            }
            self.jobs.push(job);
            Ok(())
        }

        /// The textures, buffer and scaler `key` needs.
        fn make_job(&self, key: Key) -> Result<Job, GpuError> {
            let (input, output) = (to_size(key.input), to_size(key.output));
            let read = MTLTextureUsage::ShaderRead;
            let write = MTLTextureUsage::ShaderWrite;
            // MetalFX for Smooth, and for 3D beside Rounded Pixels if it
            // enlarges by a quarter or more; by less, sampling the frame
            // looks the same for less work.
            let enlarges =
                4 * output.width >= 5 * input.width && 4 * output.height >= 5 * input.height;
            let metal_fx = match key.method {
                Method::Sharp => false,
                Method::Smooth => true,
                Method::Rounded => key.beside_3d() && enlarges && self.device.metal_fx,
            };
            let scaler = metal_fx
                .then(|| self.metal_fx_scaler(input, output))
                .transpose()?;
            // SAFETY: reads two properties of a live scaler.
            let usages = scaler
                .as_ref()
                .map(|scaler| unsafe { (scaler.colorTextureUsage(), scaler.outputTextureUsage()) });
            let (input_usage, smooth_usage) = usages.unwrap_or((read, write));
            let output_usage = match key.method {
                Method::Smooth => smooth_usage,
                Method::Sharp | Method::Rounded => write,
            };
            let readback = self
                .device
                .device
                .newBufferWithLength_options(
                    output.width * output.height * 4,
                    MTLResourceOptions::StorageModeShared,
                )
                .ok_or_else(|| setup("no readback buffer"))?;
            let (console, smooth) = if key.beside_3d() {
                let console = || {
                    self.texture(
                        to_size(key.console()),
                        read | write,
                        MTLStorageMode::Private,
                    )
                };
                let console = [console()?, console()?];
                let smooth = scaler
                    .as_ref()
                    .map(|_| self.texture(output, smooth_usage | read, MTLStorageMode::Private))
                    .transpose()?;
                (Some(console), smooth)
            } else {
                (None, None)
            };
            Ok(Job {
                key,
                input: self.texture(input, input_usage | read, MTLStorageMode::Shared)?,
                output: self.texture(output, output_usage, MTLStorageMode::Private)?,
                readback,
                scaler,
                console,
                smooth,
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

    /// Runs `pipeline` over `grid`, one thread per pixel, with `textures`
    /// at indices 0, 1, … and `cell`, if given, as buffer 0.
    fn dispatch(
        commands: &ProtocolObject<dyn MTLCommandBuffer>,
        pipeline: &ProtocolObject<dyn MTLComputePipelineState>,
        textures: &[&ProtocolObject<dyn MTLTexture>],
        cell: Option<u32>,
        grid: MTLSize,
    ) -> Result<(), GpuError> {
        let encoder = commands
            .computeCommandEncoder()
            .ok_or_else(|| setup("no compute encoder"))?;
        encoder.setComputePipelineState(pipeline);
        for (index, texture) in textures.iter().enumerate() {
            // SAFETY: the kernels take their textures at these indices, in
            // this order, with the access each was made for, and each
            // outlives the command buffer, which `upscale` waits for.
            unsafe { encoder.setTexture_atIndex(Some(texture), index) };
        }
        if let Some(cell) = cell {
            // SAFETY: Metal copies the 4 bytes of `cell`, the kernel's
            // `constant uint &cell [[buffer(0)]]`, before this returns.
            unsafe {
                encoder.setBytes_length_atIndex(NonNull::from(&cell).cast::<c_void>(), 4, 0);
            }
        }
        encoder.dispatchThreads_threadsPerThreadgroup(
            grid,
            MTLSize {
                width: GROUP_SIDE,
                height: GROUP_SIDE,
                depth: 1,
            },
        );
        encoder.endEncoding();
        Ok(())
    }

    /// Encodes MetalFX's `scaler`, made for these textures' sizes, from
    /// `input` (`size`, all of it the picture) to `output`.
    fn encode_metal_fx(
        commands: &ProtocolObject<dyn MTLCommandBuffer>,
        scaler: &ProtocolObject<dyn MTLFXSpatialScaler>,
        input: &ProtocolObject<dyn MTLTexture>,
        output: &ProtocolObject<dyn MTLTexture>,
        size: MTLSize,
    ) {
        // SAFETY: the scaler was made for exactly these textures' sizes,
        // format and usage, and both live until the command buffer is
        // done, which `upscale` waits for.
        unsafe {
            scaler.setColorTexture(Some(input));
            scaler.setOutputTexture(Some(output));
            scaler.setInputContentWidth(size.width);
            scaler.setInputContentHeight(size.height);
            scaler.encodeToCommandBuffer(commands);
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    use super::{Bgra, GpuError, Request};

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
        pub fn submit(&mut self, _requests: &[Request<'_>]) -> Result<bool, GpuError> {
            Err(GpuError::NoDevice)
        }

        /// Never called, as [`Gpu::submit`].
        pub fn finish(
            &mut self,
            _wait: Option<std::time::Duration>,
        ) -> Option<Result<Vec<Bgra>, GpuError>> {
            None
        }

        /// Never busy.
        pub fn busy(&self) -> bool {
            false
        }

        /// Never finished, as never busy.
        pub fn finished(&self) -> bool {
            false
        }
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use std::time::Duration;

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
    fn one_batch_is_on_the_gpu_at_a_time() {
        let Some(mut gpu) = gpu() else { return };
        let mut frame = Frame::new(ScreenSize::new(16, 12));
        frame.pixels_mut().fill(rgb(10, 20, 30));
        let request = Request {
            frame: &frame,
            method: Method::Sharp,
            cell: 1,
            width: 64,
            height: 48,
        };
        assert!(!gpu.busy());
        assert!(gpu.finish(Some(Duration::ZERO)).is_none(), "nothing sent");
        assert!(gpu.submit(&[request]).unwrap());
        assert!(gpu.busy());
        // A second batch waits for the first to be taken.
        assert!(!gpu.submit(&[request]).unwrap());
        let bytes = gpu.finish(None).expect("sent").expect("upscaled");
        assert_eq!(bytes[0].len(), 64 * 48 * 4);
        assert_eq!(&bytes[0][..4], &[30, 20, 10, 255]);
        assert!(!gpu.busy() && !gpu.finished());
        // Taken with a time limit: finished within a second, or still on
        // the GPU for the next try.
        assert!(gpu.submit(&[request]).unwrap());
        let deadline = std::time::Instant::now() + Duration::from_secs(1);
        let taken = loop {
            if let Some(taken) = gpu.finish(Some(Duration::from_millis(1))) {
                break taken;
            }
            assert!(gpu.busy(), "a batch not taken stays on the GPU");
            assert!(
                std::time::Instant::now() < deadline,
                "the GPU never finished"
            );
        };
        assert_eq!(taken.expect("upscaled")[0].len(), 64 * 48 * 4);
    }

    #[test]
    fn a_flat_frame_stays_flat_at_any_size_and_both_methods() {
        let Some(mut gpu) = gpu() else { return };
        let mut frame = Frame::new(ScreenSize::new(16, 12));
        frame.pixels_mut().fill(rgb(200, 120, 40));
        for method in [Method::Sharp, Method::Rounded, Method::Smooth] {
            for (width, height) in [(80, 60), (123, 77), (16 * 13, 12 * 13)] {
                let bytes = gpu
                    .upscale(&frame, method, 1, width, height)
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
            .upscale(&frame, Method::Rounded, 1, side, side)
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

    /// `frame` with each pixel spread over a square of `cell`, as the DS
    /// enlarges what is not 3D.
    fn enlarged(frame: &Frame, cell: u16) -> Frame {
        let size = frame.size();
        let width = usize::from(size.width);
        let cell = usize::from(cell);
        let source = frame.pixels();
        let side = |length: u16| u16::try_from(usize::from(length) * cell).expect("small");
        let mut out = Frame::new(ScreenSize::new(side(size.width), side(size.height)));
        for (y, row) in out.rows_mut().enumerate() {
            for (x, pixel) in row.iter_mut().enumerate() {
                *pixel = source[y / cell * width + x / cell];
            }
        }
        out
    }

    /// A black staircase on white, one pixel a step.
    fn staircase(side: u16) -> Frame {
        let mut frame = Frame::new(ScreenSize::new(side, side));
        for (y, row) in frame.rows_mut().enumerate() {
            for (x, pixel) in row.iter_mut().enumerate() {
                *pixel = if x <= y {
                    rgb(0, 0, 0)
                } else {
                    rgb(255, 255, 255)
                };
            }
        }
        frame
    }

    #[test]
    fn a_larger_frame_without_3d_detail_gets_pixel_art_from_its_console_pixels() {
        let Some(mut gpu) = gpu() else { return };
        let native = staircase(16);
        let side = 16 * 5;
        let direct = gpu
            .upscale(&native, Method::Rounded, 1, side, side)
            .expect("upscaled");
        let beside = gpu
            .upscale(&enlarged(&native, 4), Method::Rounded, 4, side, side)
            .expect("upscaled");
        let most = direct
            .iter()
            .zip(&beside)
            .map(|(a, b)| a.abs_diff(*b))
            .max();
        assert!(most.is_some_and(|most| most <= 1), "{most:?}");
    }

    #[test]
    fn squares_with_3d_detail_keep_it() {
        let Some(mut gpu) = gpu() else { return };
        let grey = rgb(128, 128, 128);
        let mut native = Frame::new(ScreenSize::new(16, 12));
        native.pixels_mut().fill(grey);
        let mut large = enlarged(&native, 4);
        // One console pixel's square, (8, 6), white above and black below:
        // detail the console's own pixel cannot hold.
        let width = 16 * 4;
        for y in 24..28 {
            for x in 32..36 {
                large.pixels_mut()[y * width + x] = if y < 26 {
                    rgb(255, 255, 255)
                } else {
                    rgb(0, 0, 0)
                };
            }
        }
        // Enlarged (through MetalFX) and shrunk (sampled), the square keeps
        // a light top and a dark bottom; far from it, the grey stays.
        for scale in [8, 3] {
            let (out_width, out_height) = (16 * scale, 12 * scale);
            let bytes = gpu
                .upscale(&large, Method::Rounded, 4, out_width, out_height)
                .expect("upscaled");
            let green = |x: u32, y: u32| pixel(&bytes, out_width, x, y)[1];
            let (left, top) = (8 * scale, 6 * scale);
            let middle = left + scale / 2;
            assert!(
                green(middle, top + scale / 5) > 180,
                "light top at {scale}×"
            );
            assert!(
                green(middle, top + scale * 4 / 5) < 80,
                "dark bottom at {scale}×"
            );
            assert!(green(1, 1).abs_diff(128) <= 2, "grey far away at {scale}×");
        }
    }

    #[test]
    fn sharp_pixels_are_even_crisp_squares_at_any_size() {
        let Some(mut gpu) = gpu() else { return };
        // Alternate black and white columns, the hardest case for evenness.
        let (black, white) = (rgb(0, 0, 0), rgb(255, 255, 255));
        let mut frame = Frame::new(ScreenSize::new(8, 2));
        for row in frame.rows_mut() {
            for (x, pixel) in row.iter_mut().enumerate() {
                *pixel = if x % 2 == 0 { black } else { white };
            }
        }
        // At a whole multiple, exactly nearest-neighbour.
        let (width, height) = (8 * 4, 2 * 4);
        let bytes = gpu
            .upscale(&frame, Method::Sharp, 1, width, height)
            .expect("upscaled");
        for x in 0..width {
            let expected = if (x / 4) % 2 == 0 { 0 } else { 255 };
            assert_eq!(pixel(&bytes, width, x, 3)[1], expected, "column {x} at 4×");
        }
        // At 4.5×, every console pixel is 4 or 5 output pixels, each
        // fully its colour but the one across an edge: never blurred
        // further, never a column lost.
        let width = 36;
        let bytes = gpu
            .upscale(&frame, Method::Sharp, 1, width, 9)
            .expect("upscaled");
        let row: Vec<u8> = (0..width).map(|x| pixel(&bytes, width, x, 4)[1]).collect();
        let mixed = row
            .iter()
            .filter(|&&value| value != 0 && value != 255)
            .count();
        assert!(
            mixed <= 7,
            "at most one mixed output pixel per edge: {row:?}"
        );
        let mut runs = Vec::new();
        for &value in row.iter().filter(|&&value| value == 0 || value == 255) {
            match runs.last_mut() {
                Some((last, count)) if *last == value => *count += 1,
                _ => runs.push((value, 1_usize)),
            }
        }
        assert_eq!(runs.len(), 8, "eight columns: {row:?}");
        assert!(
            runs.iter().all(|(_, count)| (3..=5).contains(count)),
            "each column 3 to 5 pure pixels wide: {row:?}"
        );
    }

    #[test]
    fn screens_upscaled_together_stay_apart() {
        let Some(mut gpu) = gpu() else { return };
        let (mut light, mut dark) = (
            Frame::new(ScreenSize::new(16, 12)),
            Frame::new(ScreenSize::new(16, 12)),
        );
        light.pixels_mut().fill(rgb(240, 240, 240));
        dark.pixels_mut().fill(rgb(10, 10, 10));
        let request = |frame| Request {
            frame,
            method: Method::Rounded,
            cell: 1,
            width: 64,
            height: 48,
        };
        let images = gpu
            .upscale_all(&[request(&light), request(&dark)])
            .expect("upscaled");
        assert_eq!(pixel(&images[0], 64, 30, 20)[1], 240);
        assert_eq!(pixel(&images[1], 64, 30, 20)[1], 10);
        assert_eq!(gpu.kept_outputs(), [(64, 48), (64, 48)]);
    }

    #[test]
    fn sizes_are_kept_and_reused() {
        let Some(mut gpu) = gpu() else { return };
        let frame = Frame::new(ScreenSize::new(10, 10));
        for side in [20, 30, 40, 50, 60, 70, 80, 90, 100, 20] {
            gpu.upscale(&frame, Method::Rounded, 1, side, side)
                .expect("upscaled");
        }
        assert_eq!(
            gpu.kept_outputs(),
            [
                (40, 40),
                (50, 50),
                (60, 60),
                (70, 70),
                (80, 80),
                (90, 90),
                (100, 100),
                (20, 20)
            ]
        );
    }

    #[test]
    fn rejects_empty_and_oversized_outputs() {
        let Some(mut gpu) = gpu() else { return };
        let frame = Frame::new(ScreenSize::new(4, 4));
        assert_eq!(
            gpu.upscale(&frame, Method::Rounded, 1, 0, 10),
            Err(GpuError::Size(0, 10))
        );
        assert_eq!(
            gpu.upscale(&frame, Method::Smooth, 1, 20_000, 10),
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
