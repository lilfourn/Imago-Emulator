//! The part of the libretro API the helper uses, from `libretro.h` and
//! `libretro_vulkan.h` (MIT licence, © 2010–2024 the RetroArch team).

#![allow(missing_docs, non_camel_case_types, dead_code)]

use std::ffi::{c_char, c_uint, c_void};

use ash::vk;

pub const RETRO_API_VERSION: c_uint = 1;

pub const EXPERIMENTAL: c_uint = 0x10000;

pub const ENV_SET_MESSAGE: c_uint = 6;
pub const ENV_GET_SYSTEM_DIRECTORY: c_uint = 9;
pub const ENV_SET_PIXEL_FORMAT: c_uint = 10;
pub const ENV_SET_HW_RENDER: c_uint = 14;
pub const ENV_GET_VARIABLE: c_uint = 15;
pub const ENV_SET_VARIABLES: c_uint = 16;
pub const ENV_GET_VARIABLE_UPDATE: c_uint = 17;
pub const ENV_GET_LOG_INTERFACE: c_uint = 27;
pub const ENV_GET_SAVE_DIRECTORY: c_uint = 31;
pub const ENV_SET_SYSTEM_AV_INFO: c_uint = 32;
pub const ENV_SET_GEOMETRY: c_uint = 37;
pub const ENV_GET_HW_RENDER_INTERFACE: c_uint = 41 | EXPERIMENTAL;
pub const ENV_SET_HW_RENDER_CONTEXT_NEGOTIATION_INTERFACE: c_uint = 43 | EXPERIMENTAL;
pub const ENV_SET_SERIALIZATION_QUIRKS: c_uint = 44;
pub const ENV_SET_HW_SHARED_CONTEXT: c_uint = 44 | EXPERIMENTAL;
pub const ENV_GET_AUDIO_VIDEO_ENABLE: c_uint = 47 | EXPERIMENTAL;
pub const ENV_GET_INPUT_BITMASKS: c_uint = 51 | EXPERIMENTAL;
pub const ENV_GET_CORE_OPTIONS_VERSION: c_uint = 52;
pub const ENV_GET_PREFERRED_HW_RENDER: c_uint = 56;
pub const ENV_GET_MESSAGE_INTERFACE_VERSION: c_uint = 59;
pub const ENV_SET_MESSAGE_EXT: c_uint = 60;
pub const ENV_GET_CAN_DUPE: c_uint = 3;

pub const DEVICE_JOYPAD: c_uint = 1;
pub const DEVICE_MOUSE: c_uint = 2;
pub const DEVICE_ANALOG: c_uint = 5;
pub const DEVICE_POINTER: c_uint = 6;

pub const JOYPAD_B: c_uint = 0;
pub const JOYPAD_Y: c_uint = 1;
pub const JOYPAD_SELECT: c_uint = 2;
pub const JOYPAD_START: c_uint = 3;
pub const JOYPAD_UP: c_uint = 4;
pub const JOYPAD_DOWN: c_uint = 5;
pub const JOYPAD_LEFT: c_uint = 6;
pub const JOYPAD_RIGHT: c_uint = 7;
pub const JOYPAD_A: c_uint = 8;
pub const JOYPAD_X: c_uint = 9;
pub const JOYPAD_L: c_uint = 10;
pub const JOYPAD_R: c_uint = 11;
pub const JOYPAD_L2: c_uint = 12;
pub const JOYPAD_R2: c_uint = 13;
pub const JOYPAD_MASK: c_uint = 256;

pub const ANALOG_LEFT: c_uint = 0;
pub const ANALOG_RIGHT: c_uint = 1;
pub const ANALOG_X: c_uint = 0;
pub const ANALOG_Y: c_uint = 1;

pub const POINTER_X: c_uint = 0;
pub const POINTER_Y: c_uint = 1;
pub const POINTER_PRESSED: c_uint = 2;
pub const POINTER_COUNT: c_uint = 3;

pub const PIXEL_FORMAT_XRGB8888: c_uint = 1;

pub const HW_CONTEXT_VULKAN: c_uint = 6;

pub const HW_RENDER_INTERFACE_VULKAN: c_uint = 0;
pub const HW_RENDER_INTERFACE_VULKAN_VERSION: c_uint = 5;

/// What `video_refresh` is given when the frame is in the hardware
/// renderer's image.
pub const HW_FRAME_BUFFER_VALID: *const c_void = usize::MAX as *const c_void;

pub type EnvironmentFn = unsafe extern "C" fn(cmd: c_uint, data: *mut c_void) -> bool;
pub type VideoRefreshFn =
    unsafe extern "C" fn(data: *const c_void, width: c_uint, height: c_uint, pitch: usize);
pub type AudioSampleFn = unsafe extern "C" fn(left: i16, right: i16);
pub type AudioSampleBatchFn = unsafe extern "C" fn(data: *const i16, frames: usize) -> usize;
pub type InputPollFn = unsafe extern "C" fn();
pub type InputStateFn =
    unsafe extern "C" fn(port: c_uint, device: c_uint, index: c_uint, id: c_uint) -> i16;
pub type LogFn = unsafe extern "C" fn(level: c_uint, fmt: *const c_char, ...);
pub type HwContextResetFn = unsafe extern "C" fn();

#[repr(C)]
pub struct SystemInfo {
    pub library_name: *const c_char,
    pub library_version: *const c_char,
    pub valid_extensions: *const c_char,
    pub need_fullpath: bool,
    pub block_extract: bool,
}

#[repr(C)]
#[derive(Default, Clone, Copy)]
pub struct GameGeometry {
    pub base_width: c_uint,
    pub base_height: c_uint,
    pub max_width: c_uint,
    pub max_height: c_uint,
    pub aspect_ratio: f32,
}

#[repr(C)]
#[derive(Default, Clone, Copy)]
pub struct SystemTiming {
    pub fps: f64,
    pub sample_rate: f64,
}

#[repr(C)]
#[derive(Default, Clone, Copy)]
pub struct SystemAvInfo {
    pub geometry: GameGeometry,
    pub timing: SystemTiming,
}

#[repr(C)]
pub struct GameInfo {
    pub path: *const c_char,
    pub data: *const c_void,
    pub size: usize,
    pub meta: *const c_char,
}

#[repr(C)]
pub struct Variable {
    pub key: *const c_char,
    pub value: *const c_char,
}

#[repr(C)]
pub struct LogCallback {
    pub log: LogFn,
}

#[repr(C)]
pub struct Message {
    pub msg: *const c_char,
    pub frames: c_uint,
}

#[repr(C)]
pub struct MessageExt {
    pub msg: *const c_char,
    pub duration: c_uint,
    pub priority: c_uint,
    pub level: c_uint,
    pub target: c_uint,
    pub kind: c_uint,
    pub progress: i8,
}

#[repr(C)]
pub struct HwRenderCallback {
    pub context_type: c_uint,
    pub context_reset: Option<HwContextResetFn>,
    pub get_current_framebuffer: Option<unsafe extern "C" fn() -> usize>,
    pub get_proc_address: Option<unsafe extern "C" fn(sym: *const c_char) -> *const c_void>,
    pub depth: bool,
    pub stencil: bool,
    pub bottom_left_origin: bool,
    pub version_major: c_uint,
    pub version_minor: c_uint,
    pub cache_context: bool,
    pub context_destroy: Option<HwContextResetFn>,
    pub debug_context: bool,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct VulkanImage {
    pub image_view: vk::ImageView,
    pub image_layout: vk::ImageLayout,
    pub create_info: vk::ImageViewCreateInfo<'static>,
}

#[repr(C)]
#[derive(Default)]
pub struct VulkanContext {
    pub gpu: vk::PhysicalDevice,
    pub device: vk::Device,
    pub queue: vk::Queue,
    pub queue_family_index: u32,
    pub presentation_queue: vk::Queue,
    pub presentation_queue_family_index: u32,
}

pub type VulkanCreateDeviceFn = unsafe extern "C" fn(
    context: *mut VulkanContext,
    instance: vk::Instance,
    gpu: vk::PhysicalDevice,
    surface: vk::SurfaceKHR,
    get_instance_proc_addr: vk::PFN_vkGetInstanceProcAddr,
    required_device_extensions: *const *const c_char,
    num_required_device_extensions: c_uint,
    required_device_layers: *const *const c_char,
    num_required_device_layers: c_uint,
    required_features: *const vk::PhysicalDeviceFeatures,
) -> bool;

#[repr(C)]
pub struct VulkanNegotiation {
    pub interface_type: c_uint,
    pub interface_version: c_uint,
    pub get_application_info: Option<unsafe extern "C" fn() -> *const vk::ApplicationInfo<'static>>,
    pub create_device: Option<VulkanCreateDeviceFn>,
    pub destroy_device: Option<unsafe extern "C" fn()>,
}

#[repr(C)]
pub struct VulkanInterface {
    pub interface_type: c_uint,
    pub interface_version: c_uint,
    pub handle: *mut c_void,
    pub instance: vk::Instance,
    pub gpu: vk::PhysicalDevice,
    pub device: vk::Device,
    pub get_device_proc_addr: vk::PFN_vkGetDeviceProcAddr,
    pub get_instance_proc_addr: vk::PFN_vkGetInstanceProcAddr,
    pub queue: vk::Queue,
    pub queue_index: c_uint,
    pub set_image: unsafe extern "C" fn(
        handle: *mut c_void,
        image: *const VulkanImage,
        num_semaphores: u32,
        semaphores: *const vk::Semaphore,
        src_queue_family: u32,
    ),
    pub get_sync_index: unsafe extern "C" fn(handle: *mut c_void) -> u32,
    pub get_sync_index_mask: unsafe extern "C" fn(handle: *mut c_void) -> u32,
    pub set_command_buffers:
        unsafe extern "C" fn(handle: *mut c_void, num: u32, cmd: *const vk::CommandBuffer),
    pub wait_sync_index: unsafe extern "C" fn(handle: *mut c_void),
    pub lock_queue: unsafe extern "C" fn(handle: *mut c_void),
    pub unlock_queue: unsafe extern "C" fn(handle: *mut c_void),
    pub set_signal_semaphore: unsafe extern "C" fn(handle: *mut c_void, semaphore: vk::Semaphore),
}

/// The core's entry points.
pub struct Api {
    pub set_environment: unsafe extern "C" fn(EnvironmentFn),
    pub set_video_refresh: unsafe extern "C" fn(VideoRefreshFn),
    pub set_audio_sample: unsafe extern "C" fn(AudioSampleFn),
    pub set_audio_sample_batch: unsafe extern "C" fn(AudioSampleBatchFn),
    pub set_input_poll: unsafe extern "C" fn(InputPollFn),
    pub set_input_state: unsafe extern "C" fn(InputStateFn),
    pub api_version: unsafe extern "C" fn() -> c_uint,
    pub init: unsafe extern "C" fn(),
    pub deinit: unsafe extern "C" fn(),
    pub get_system_info: unsafe extern "C" fn(*mut SystemInfo),
    pub get_system_av_info: unsafe extern "C" fn(*mut SystemAvInfo),
    pub load_game: unsafe extern "C" fn(*const GameInfo) -> bool,
    pub unload_game: unsafe extern "C" fn(),
    pub run: unsafe extern "C" fn(),
    pub reset: unsafe extern "C" fn(),
    pub serialize_size: unsafe extern "C" fn() -> usize,
    pub serialize: unsafe extern "C" fn(*mut c_void, usize) -> bool,
    pub unserialize: unsafe extern "C" fn(*const c_void, usize) -> bool,
    /// Keeps the core's code loaded for as long as the entry points are
    /// used.
    _library: libloading::Library,
}

impl Api {
    /// Opens the core at `path` and finds its entry points.
    ///
    /// # Safety
    ///
    /// `path` must be a libretro core: opening it runs its initialisers,
    /// and each entry point is taken to have libretro's signature.
    pub unsafe fn open(path: &std::path::Path) -> Result<Self, libloading::Error> {
        // SAFETY: the caller vouches that `path` is a libretro core.
        let library = unsafe { libloading::Library::new(path)? };
        macro_rules! entry {
            ($name:literal) => {
                // SAFETY: libretro gives every entry point this name and
                // the signature of the field it fills.
                *unsafe { library.get(concat!($name, "\0").as_bytes())? }
            };
        }
        Ok(Self {
            set_environment: entry!("retro_set_environment"),
            set_video_refresh: entry!("retro_set_video_refresh"),
            set_audio_sample: entry!("retro_set_audio_sample"),
            set_audio_sample_batch: entry!("retro_set_audio_sample_batch"),
            set_input_poll: entry!("retro_set_input_poll"),
            set_input_state: entry!("retro_set_input_state"),
            api_version: entry!("retro_api_version"),
            init: entry!("retro_init"),
            deinit: entry!("retro_deinit"),
            get_system_info: entry!("retro_get_system_info"),
            get_system_av_info: entry!("retro_get_system_av_info"),
            load_game: entry!("retro_load_game"),
            unload_game: entry!("retro_unload_game"),
            run: entry!("retro_run"),
            reset: entry!("retro_reset"),
            serialize_size: entry!("retro_serialize_size"),
            serialize: entry!("retro_serialize"),
            unserialize: entry!("retro_unserialize"),
            _library: library,
        })
    }
}
