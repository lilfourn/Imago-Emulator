//! The frontend's side of libretro's Vulkan contract, on MoltenVK.
//!
//! The frontend makes the instance, the core makes its device through its
//! negotiation interface, and the frontend hands the core
//! [`lr::VulkanInterface`]. The core submits its own work to the queue,
//! holding the queue's lock, and names each frame's image through
//! `set_image`. After each frame the helper copies that image into a
//! buffer the CPU can read and waits for the copy, so the frame is
//! complete when the app gets it and nothing is left in flight: one frame
//! at a time, sync index 0.

use std::ffi::{CStr, c_char, c_void};
use std::path::Path;
use std::sync::{Condvar, Mutex, PoisonError};
use std::time::Duration;

use ash::vk;

use crate::host::Picture;
use crate::libretro::{self as lr, VulkanImage, VulkanInterface, VulkanNegotiation};

/// How long the copy of a frame may take before the GPU counts as hung.
const COPY_TIMEOUT: Duration = Duration::from_secs(5);
/// How long to wait for the core to submit a frame it has named: three
/// frames, far beyond the tens of microseconds it takes.
const SUBMIT_TIMEOUT: Duration = Duration::from_millis(50);

/// Loads MoltenVK and finds a GPU, to choose the renderer before the core
/// loads the game. Returns the GPU's name.
pub fn probe(moltenvk: &Path) -> Result<String, String> {
    // SAFETY: `moltenvk` is MoltenVK, checked against its pinned digest
    // when it was installed; loading it runs no code of the game's.
    let entry = unsafe { ash::Entry::load_from(moltenvk) }.map_err(|error| error.to_string())?;
    let app = vk::ApplicationInfo::default().api_version(vk::API_VERSION_1_1);
    let instance = create_instance(&entry, &app)?;
    // SAFETY: the instance is alive until destroyed below.
    let result = unsafe { instance.enumerate_physical_devices() }
        .map_err(|error| error.to_string())
        .and_then(|gpus| gpus.first().copied().ok_or_else(|| "no GPU".to_owned()))
        .map(|gpu| {
            // SAFETY: `gpu` came from this instance.
            let properties = unsafe { instance.get_physical_device_properties(gpu) };
            properties.device_name_as_c_str().map_or_else(
                |_| "a GPU".to_owned(),
                |name| name.to_string_lossy().into_owned(),
            )
        });
    // SAFETY: nothing made from the instance outlives it.
    unsafe { instance.destroy_instance(None) };
    result
}

/// Creates an instance with the portability extensions MoltenVK offers.
fn create_instance(
    entry: &ash::Entry,
    app: &vk::ApplicationInfo<'_>,
) -> Result<ash::Instance, String> {
    // SAFETY: the entry is loaded; this asks only what it offers.
    let offered = unsafe { entry.enumerate_instance_extension_properties(None) }
        .map_err(|error| error.to_string())?;
    let offers = |name: &CStr| {
        offered
            .iter()
            .any(|extension| extension.extension_name_as_c_str() == Ok(name))
    };
    let mut extensions: Vec<*const c_char> = Vec::new();
    let mut flags = vk::InstanceCreateFlags::empty();
    if offers(vk::KHR_PORTABILITY_ENUMERATION_NAME) {
        extensions.push(vk::KHR_PORTABILITY_ENUMERATION_NAME.as_ptr());
        flags |= vk::InstanceCreateFlags::ENUMERATE_PORTABILITY_KHR;
    }
    if offers(vk::KHR_GET_PHYSICAL_DEVICE_PROPERTIES2_NAME) {
        extensions.push(vk::KHR_GET_PHYSICAL_DEVICE_PROPERTIES2_NAME.as_ptr());
    }
    let info = vk::InstanceCreateInfo::default()
        .flags(flags)
        .application_info(app)
        .enabled_extension_names(&extensions);
    // SAFETY: `info` and everything it points at outlive the call.
    unsafe { entry.create_instance(&info, None) }.map_err(|error| error.to_string())
}

/// A lock the core takes and releases in two separate calls, so it cannot
/// be a guard: a flag and a condition variable.
#[derive(Default)]
struct QueueLock {
    held: Mutex<bool>,
    free: Condvar,
}

impl QueueLock {
    fn lock(&self) {
        let mut held = self.held.lock().unwrap_or_else(PoisonError::into_inner);
        while *held {
            held = self.free.wait(held).unwrap_or_else(PoisonError::into_inner);
        }
        *held = true;
    }

    fn unlock(&self) {
        *self.held.lock().unwrap_or_else(PoisonError::into_inner) = false;
        self.free.notify_one();
    }
}

/// What the interface's callbacks share: the handle the core passes back.
#[derive(Default)]
struct Handle {
    queue: QueueLock,
    /// The image the core last named, with the queue family it was used
    /// on.
    image: Mutex<Option<VulkanImage>>,
    /// Command buffers the core asked the frontend to submit with the
    /// frame.
    commands: Mutex<Vec<vk::CommandBuffer>>,
    /// How many times the core has submitted work to the queue, and how
    /// many it had when it last named an image.
    progress: Mutex<Progress>,
    /// Signalled whenever the core submits.
    submitted: Condvar,
}

/// The core's submissions, counted for [`Gpu::read_frame`]. The core
/// records its commands on a worker thread of its own: it hands the
/// frame's last commands to that thread and names the frame's image at
/// once, and the thread submits them tens of microseconds later. The copy
/// must come after that submission, or it reads the frame before.
#[derive(Default)]
struct Progress {
    submits: u64,
    at_image: Option<u64>,
}

impl Handle {
    /// The handle behind the core's pointer.
    ///
    /// # Safety
    ///
    /// `handle` is the pointer [`Gpu::new`] put in the interface, whose
    /// `Handle` lives as long as the process.
    unsafe fn from<'a>(handle: *mut c_void) -> &'a Self {
        // SAFETY: as the caller vouches.
        unsafe { &*handle.cast::<Self>() }
    }
}

// SAFETY (for these callbacks): the core passes back the handle it was
// given, and the pointers it passes are valid for the call, as libretro
// defines them.

unsafe extern "C" fn set_image(
    handle: *mut c_void,
    image: *const VulkanImage,
    _semaphores: u32,
    _waits: *const vk::Semaphore,
    _queue_family: u32,
) {
    if image.is_null() {
        return;
    }
    // SAFETY: see above.
    let (handle, image) = unsafe { (Handle::from(handle), *image) };
    *handle.image.lock().unwrap_or_else(PoisonError::into_inner) = Some(image);
    let mut progress = handle
        .progress
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    progress.at_image = Some(progress.submits);
}

unsafe extern "C" fn get_sync_index(_handle: *mut c_void) -> u32 {
    0
}

unsafe extern "C" fn get_sync_index_mask(_handle: *mut c_void) -> u32 {
    1
}

unsafe extern "C" fn set_command_buffers(
    handle: *mut c_void,
    count: u32,
    commands: *const vk::CommandBuffer,
) {
    if commands.is_null() {
        return;
    }
    // SAFETY: see above; `commands` holds `count` buffers.
    let (handle, commands) = unsafe {
        (
            Handle::from(handle),
            std::slice::from_raw_parts(commands, count as usize),
        )
    };
    handle
        .commands
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .extend_from_slice(commands);
}

/// Every frame's copy is waited for before the next frame runs, so there
/// is never anything to wait for here.
unsafe extern "C" fn wait_sync_index(_handle: *mut c_void) {}

unsafe extern "C" fn lock_queue(handle: *mut c_void) {
    // SAFETY: see above.
    unsafe { Handle::from(handle) }.queue.lock();
}

unsafe extern "C" fn unlock_queue(handle: *mut c_void) {
    // SAFETY: see above.
    let handle = unsafe { Handle::from(handle) };
    // Only the core comes here: the helper's own copy takes the lock
    // directly, so it is not counted.
    handle
        .progress
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .submits += 1;
    handle.submitted.notify_all();
    handle.queue.unlock();
}

/// The core never waits on this semaphore; the frame is consumed by the
/// time the next one runs.
unsafe extern "C" fn set_signal_semaphore(_handle: *mut c_void, _semaphore: vk::Semaphore) {}

/// A host-visible buffer each frame is copied into.
struct Readback {
    buffer: vk::Buffer,
    memory: vk::DeviceMemory,
    size: vk::DeviceSize,
    mapped: *const u8,
    /// Whether the memory is coherent, or must be invalidated before
    /// reading.
    coherent: bool,
}

/// The Vulkan side of the helper.
pub struct Gpu {
    _entry: ash::Entry,
    instance: ash::Instance,
    gpu: vk::PhysicalDevice,
    device: ash::Device,
    queue: vk::Queue,
    _pool: vk::CommandPool,
    commands: vk::CommandBuffer,
    fence: vk::Fence,
    readback: Option<Readback>,
    /// Whether the log has said once that a frame came without its
    /// submission.
    warned_late: std::cell::Cell<bool>,
    /// Shared with the core through [`lr::VulkanInterface::handle`], and
    /// the interface itself: both kept for the life of the process, as
    /// the core's threads may hold them to the end.
    handle: &'static Handle,
    interface: &'static VulkanInterface,
}

impl Gpu {
    /// Makes the instance, has the core make its device, and builds the
    /// interface to hand it.
    ///
    /// # Safety
    ///
    /// `negotiation` is the core's, and the core is loaded.
    pub unsafe fn new(
        moltenvk: &Path,
        negotiation: &VulkanNegotiation,
    ) -> Result<Box<Self>, String> {
        // SAFETY: as in `probe`.
        let entry =
            unsafe { ash::Entry::load_from(moltenvk) }.map_err(|error| error.to_string())?;
        let default_app = vk::ApplicationInfo::default().api_version(vk::API_VERSION_1_1);
        let app = match negotiation.get_application_info {
            // SAFETY: the core's application info lives as long as it.
            Some(get) => unsafe { get().as_ref() }.copied().unwrap_or(default_app),
            None => default_app,
        };
        let instance = create_instance(&entry, &app)?;
        // SAFETY: the instance is alive.
        let gpu = unsafe { instance.enumerate_physical_devices() }
            .map_err(|error| error.to_string())?
            .first()
            .copied()
            .ok_or("no GPU")?;
        let create_device = negotiation
            .create_device
            .ok_or("the core cannot make its own device")?;
        let mut context = lr::VulkanContext::default();
        let moltenvk = (
            entry.static_fn().get_instance_proc_addr,
            instance.fp_v1_0().get_device_proc_addr,
        );
        // The core looks Vulkan's functions up through the helper, which
        // stands in for its pipeline caches' (see `pipelines`).
        let (look_up_instance, look_up_device) = crate::pipelines::lookups(moltenvk);
        // SAFETY: the core makes its device on our instance and GPU, asking
        // through our loader; no extensions, layers or features are
        // required of it.
        let made = unsafe {
            create_device(
                &mut context,
                instance.handle(),
                gpu,
                vk::SurfaceKHR::null(),
                look_up_instance,
                std::ptr::null(),
                0,
                std::ptr::null(),
                0,
                std::ptr::null(),
            )
        };
        if !made || context.device == vk::Device::null() {
            return Err("the core could not make its Vulkan device".to_owned());
        }
        // SAFETY: the core made `context.device` on this instance.
        let device = unsafe { ash::Device::load(instance.fp_v1_0(), context.device) };
        let pool_info = vk::CommandPoolCreateInfo::default()
            .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER)
            .queue_family_index(context.queue_family_index);
        // SAFETY: the device is alive; these objects are destroyed with it.
        let (pool, fence) = unsafe {
            let pool = device
                .create_command_pool(&pool_info, None)
                .map_err(|error| error.to_string())?;
            let fence = device
                .create_fence(&vk::FenceCreateInfo::default(), None)
                .map_err(|error| error.to_string())?;
            (pool, fence)
        };
        let allocate = vk::CommandBufferAllocateInfo::default()
            .command_pool(pool)
            .level(vk::CommandBufferLevel::PRIMARY)
            .command_buffer_count(1);
        // SAFETY: the pool is alive.
        let commands = unsafe { device.allocate_command_buffers(&allocate) }
            .map_err(|error| error.to_string())?[0];

        let handle: &'static Handle = Box::leak(Box::default());
        let interface: &'static VulkanInterface = Box::leak(Box::new(VulkanInterface {
            interface_type: lr::HW_RENDER_INTERFACE_VULKAN,
            interface_version: lr::HW_RENDER_INTERFACE_VULKAN_VERSION,
            handle: std::ptr::from_ref(handle).cast_mut().cast(),
            instance: instance.handle(),
            gpu: context.gpu,
            device: context.device,
            get_device_proc_addr: look_up_device,
            get_instance_proc_addr: look_up_instance,
            queue: context.queue,
            queue_index: context.queue_family_index,
            set_image,
            get_sync_index,
            get_sync_index_mask,
            set_command_buffers,
            wait_sync_index,
            lock_queue,
            unlock_queue,
            set_signal_semaphore,
        }));
        Ok(Box::new(Self {
            _entry: entry,
            instance,
            gpu: context.gpu,
            device,
            queue: context.queue,
            _pool: pool,
            commands,
            fence,
            readback: None,
            warned_late: std::cell::Cell::new(false),
            handle,
            interface,
        }))
    }

    /// The interface to hand the core, which stays put as long as `self`.
    pub fn interface(&self) -> *const VulkanInterface {
        self.interface
    }

    /// Copies the image the core named for this frame, `width` × `height`,
    /// into `picture`.
    pub fn read_frame(
        &mut self,
        width: u32,
        height: u32,
        picture: &mut Picture,
    ) -> Result<(), String> {
        let image = (*self
            .handle
            .image
            .lock()
            .unwrap_or_else(PoisonError::into_inner))
        .ok_or("the core named no image")?;
        let bytes = vk::DeviceSize::from(width) * vk::DeviceSize::from(height) * 4;
        let readback = self.readback(bytes)?;
        let (buffer, mapped, coherent, memory) = (
            readback.buffer,
            readback.mapped,
            readback.coherent,
            readback.memory,
        );
        let extra: Vec<vk::CommandBuffer> = std::mem::take(
            &mut *self
                .handle
                .commands
                .lock()
                .unwrap_or_else(PoisonError::into_inner),
        );

        let range = image.create_info.subresource_range;
        let color = vk::ImageSubresourceRange {
            aspect_mask: vk::ImageAspectFlags::COLOR,
            base_mip_level: range.base_mip_level,
            level_count: 1,
            base_array_layer: range.base_array_layer,
            layer_count: 1,
        };
        let to_copy = vk::ImageMemoryBarrier::default()
            .src_access_mask(vk::AccessFlags::MEMORY_WRITE)
            .dst_access_mask(vk::AccessFlags::TRANSFER_READ)
            .old_layout(image.image_layout)
            .new_layout(vk::ImageLayout::TRANSFER_SRC_OPTIMAL)
            .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .image(image.create_info.image)
            .subresource_range(color);
        let back = vk::ImageMemoryBarrier::default()
            .src_access_mask(vk::AccessFlags::TRANSFER_READ)
            .dst_access_mask(vk::AccessFlags::SHADER_READ)
            .old_layout(vk::ImageLayout::TRANSFER_SRC_OPTIMAL)
            .new_layout(image.image_layout)
            .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .image(image.create_info.image)
            .subresource_range(color);
        let to_host = vk::BufferMemoryBarrier::default()
            .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)
            .dst_access_mask(vk::AccessFlags::HOST_READ)
            .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .buffer(buffer)
            .size(bytes);
        let region = vk::BufferImageCopy::default()
            .image_subresource(vk::ImageSubresourceLayers {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                mip_level: range.base_mip_level,
                base_array_layer: range.base_array_layer,
                layer_count: 1,
            })
            .image_extent(vk::Extent3D {
                width,
                height,
                depth: 1,
            });

        let device = &self.device;
        // SAFETY: every handle is alive; the command buffer is not in
        // flight (the last submit's fence was waited for), and the queue
        // is used under the lock the core uses.
        unsafe {
            device
                .begin_command_buffer(
                    self.commands,
                    &vk::CommandBufferBeginInfo::default()
                        .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
                )
                .map_err(|error| error.to_string())?;
            device.cmd_pipeline_barrier(
                self.commands,
                vk::PipelineStageFlags::ALL_COMMANDS,
                vk::PipelineStageFlags::TRANSFER,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[to_copy],
            );
            device.cmd_copy_image_to_buffer(
                self.commands,
                image.create_info.image,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                buffer,
                &[region],
            );
            device.cmd_pipeline_barrier(
                self.commands,
                vk::PipelineStageFlags::TRANSFER,
                vk::PipelineStageFlags::HOST | vk::PipelineStageFlags::ALL_COMMANDS,
                vk::DependencyFlags::empty(),
                &[],
                &[to_host],
                &[back],
            );
            device
                .end_command_buffer(self.commands)
                .map_err(|error| error.to_string())?;

            self.wait_for_the_frame();
            let mut all = extra;
            all.push(self.commands);
            let submit = vk::SubmitInfo::default().command_buffers(&all);
            self.handle.queue.lock();
            let submitted = device.queue_submit(self.queue, &[submit], self.fence);
            self.handle.queue.unlock();
            submitted.map_err(|error| error.to_string())?;
            let timeout = u64::try_from(COPY_TIMEOUT.as_nanos()).unwrap_or(u64::MAX);
            device
                .wait_for_fences(&[self.fence], true, timeout)
                .map_err(|error| format!("the GPU did not finish the frame: {error}"))?;
            device
                .reset_fences(&[self.fence])
                .map_err(|error| error.to_string())?;
            if !coherent {
                let range = vk::MappedMemoryRange::default()
                    .memory(memory)
                    .size(vk::WHOLE_SIZE);
                device
                    .invalidate_mapped_memory_ranges(&[range])
                    .map_err(|error| error.to_string())?;
            }
        }

        // SAFETY: the copy is finished, and the mapping holds `bytes`.
        let source = unsafe { std::slice::from_raw_parts(mapped, bytes as usize) };
        let swap = match image.create_info.format {
            vk::Format::B8G8R8A8_UNORM | vk::Format::B8G8R8A8_SRGB => false,
            vk::Format::R8G8B8A8_UNORM | vk::Format::R8G8B8A8_SRGB => true,
            other => return Err(format!("frames in Vulkan format {}", other.as_raw())),
        };
        picture.resize(width, height);
        for (pixel, word) in picture.pixels.iter_mut().zip(source.chunks_exact(4)) {
            let [a, b, c, _] = [word[0], word[1], word[2], word[3]];
            let (red, blue) = if swap { (a, c) } else { (c, a) };
            *pixel = 0xFF00_0000 | u32::from(red) << 16 | u32::from(b) << 8 | u32::from(blue);
        }
        Ok(())
    }

    /// Waits until the core has submitted the work it handed its worker
    /// thread before naming this frame's image: one submission after
    /// `set_image`, which comes within a few dozen microseconds. Gives up
    /// after [`SUBMIT_TIMEOUT`], reading what is there, so a core that
    /// submitted first cannot stall the game.
    fn wait_for_the_frame(&self) {
        let progress = self
            .handle
            .progress
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let Some(at) = progress.at_image else {
            return;
        };
        let (mut progress, waited) = self
            .handle
            .submitted
            .wait_timeout_while(progress, SUBMIT_TIMEOUT, |progress| progress.submits <= at)
            .unwrap_or_else(PoisonError::into_inner);
        progress.at_image = None;
        if waited.timed_out() && !self.warned_late.replace(true) {
            eprintln!("3DS helper: the core named a frame and submitted nothing after it");
        }
    }

    /// A readback buffer of at least `bytes`, made or grown as needed.
    fn readback(&mut self, bytes: vk::DeviceSize) -> Result<&Readback, String> {
        if self
            .readback
            .as_ref()
            .is_some_and(|readback| readback.size >= bytes)
        {
            return self.readback.as_ref().ok_or_else(String::new);
        }
        if let Some(old) = self.readback.take() {
            // SAFETY: nothing is in flight: the last copy was waited for.
            unsafe {
                self.device.destroy_buffer(old.buffer, None);
                self.device.free_memory(old.memory, None);
            }
        }
        let info = vk::BufferCreateInfo::default()
            .size(bytes)
            .usage(vk::BufferUsageFlags::TRANSFER_DST)
            .sharing_mode(vk::SharingMode::EXCLUSIVE);
        // SAFETY: the device is alive; the buffer and memory are freed
        // when replaced, or with the process.
        unsafe {
            let buffer = self
                .device
                .create_buffer(&info, None)
                .map_err(|error| error.to_string())?;
            let needs = self.device.get_buffer_memory_requirements(buffer);
            let types = self
                .instance
                .get_physical_device_memory_properties(self.gpu);
            // Reading back is fastest from cached memory; any host-visible
            // memory will do.
            let visible = vk::MemoryPropertyFlags::HOST_VISIBLE;
            let cached = visible | vk::MemoryPropertyFlags::HOST_CACHED;
            let coherent = visible | vk::MemoryPropertyFlags::HOST_COHERENT;
            let pick = |wanted: vk::MemoryPropertyFlags| {
                (0..types.memory_type_count).find(|&index| {
                    needs.memory_type_bits & 1 << index != 0
                        && types.memory_types[index as usize]
                            .property_flags
                            .contains(wanted)
                })
            };
            let index = pick(cached | coherent)
                .or_else(|| pick(cached))
                .or_else(|| pick(coherent))
                .or_else(|| pick(visible))
                .ok_or("no memory the CPU can read")?;
            let flags = types.memory_types[index as usize].property_flags;
            let memory = self
                .device
                .allocate_memory(
                    &vk::MemoryAllocateInfo::default()
                        .allocation_size(needs.size)
                        .memory_type_index(index),
                    None,
                )
                .map_err(|error| error.to_string())?;
            self.device
                .bind_buffer_memory(buffer, memory, 0)
                .map_err(|error| error.to_string())?;
            let mapped = self
                .device
                .map_memory(memory, 0, vk::WHOLE_SIZE, vk::MemoryMapFlags::empty())
                .map_err(|error| error.to_string())?;
            self.readback = Some(Readback {
                buffer,
                memory,
                size: bytes,
                mapped: mapped.cast_const().cast(),
                coherent: flags.contains(vk::MemoryPropertyFlags::HOST_COHERENT),
            });
        }
        self.readback.as_ref().ok_or_else(String::new)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_queue_lock_is_exclusive_across_threads() {
        let lock = std::sync::Arc::new(QueueLock::default());
        let count = std::sync::Arc::new(Mutex::new(0u32));
        let threads: Vec<_> = (0..4)
            .map(|_| {
                let (lock, count) = (lock.clone(), count.clone());
                std::thread::spawn(move || {
                    for _ in 0..1_000 {
                        lock.lock();
                        let before = *count.lock().unwrap();
                        *count.lock().unwrap() = before + 1;
                        lock.unlock();
                    }
                })
            })
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }
        assert_eq!(*count.lock().unwrap(), 4_000);
    }
}
