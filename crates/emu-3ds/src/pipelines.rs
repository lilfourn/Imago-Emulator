//! The core's pipeline caches, split one to a thread.
//!
//! A game builds a pipeline the first time it draws something new, and the
//! frame waits for it. Azahar builds them on the game's thread and on nine
//! workers of its own, all through one Vulkan pipeline cache, and MoltenVK
//! holds that cache's lock while it translates and compiles each shader,
//! so every compile in the helper waited for the one before. A new scene
//! in Pokémon Omega Ruby held its frame up to two seconds, and more than
//! half of the game thread's wait was for that lock.
//!
//! So the helper hands the core its own lookups for Vulkan's functions
//! ([`lookups`]), which give it MoltenVK's except for a few that stand in
//! for them. Each thread that builds pipelines through one of the core's
//! caches builds them through a cache of its own instead, made from the
//! same saved data, so the threads compile at once. When the core reads
//! its cache to save it, the threads' caches are merged into it first, so
//! the shaders each translated are saved too, and the next start finds
//! them all. `DS_EMULATOR_3DS_PIPELINE_CACHE=shared` turns this off, to
//! compare the two.
//!
//! On Linux and Windows the functions called MoltenVK's here are the
//! system's Vulkan loader's, which pass each call on to the GPU's driver.
//! The split works the same there; whether those drivers hold a shared
//! cache's lock across each compile, as MoltenVK does, was not measured.

use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::{CStr, c_char, c_void};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard, OnceLock, PoisonError};

use ash::vk;

/// MoltenVK's own `vkGetInstanceProcAddr` and `vkGetDeviceProcAddr`.
static LOOKUPS: OnceLock<(vk::PFN_vkGetInstanceProcAddr, vk::PFN_vkGetDeviceProcAddr)> =
    OnceLock::new();

/// MoltenVK's own device functions, once the core has a device.
static MOLTENVK: OnceLock<ash::DeviceFnV1_0> = OnceLock::new();

/// A thread's cache standing in for one of the core's: `None` once the
/// core's is destroyed. Locked while the thread builds through it, and
/// while it is merged.
type ThreadCache = Arc<Mutex<Option<vk::PipelineCache>>>;

/// One of the core's caches.
struct Split {
    /// The saved data it was made from, to make each thread's from.
    initial: Vec<u8>,
    /// The threads' caches standing in for it.
    threads: Vec<ThreadCache>,
}

/// The core's caches, by handle.
static CACHES: LazyLock<Mutex<HashMap<vk::PipelineCache, Split>>> = LazyLock::new(Mutex::default);

thread_local! {
    /// This thread's caches, by the core's cache each stands in for.
    static MINE: RefCell<HashMap<vk::PipelineCache, ThreadCache>> = RefCell::default();
}

fn caches() -> MutexGuard<'static, HashMap<vk::PipelineCache, Split>> {
    CACHES.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The functions to hand the core for looking up Vulkan's, given
/// MoltenVK's: these, unless the caches are shared
/// (`DS_EMULATOR_3DS_PIPELINE_CACHE=shared`), and MoltenVK's own then.
pub(crate) fn lookups(
    moltenvk: (vk::PFN_vkGetInstanceProcAddr, vk::PFN_vkGetDeviceProcAddr),
) -> (vk::PFN_vkGetInstanceProcAddr, vk::PFN_vkGetDeviceProcAddr) {
    let shared = std::env::var("DS_EMULATOR_3DS_PIPELINE_CACHE").as_deref() == Ok("shared");
    // The helper makes one device, so this is set once.
    if shared || LOOKUPS.set(moltenvk).is_err() {
        return moltenvk;
    }
    (get_instance_proc_addr, get_device_proc_addr)
}

/// The core's `vkGetInstanceProcAddr`.
unsafe extern "system" fn get_instance_proc_addr(
    instance: vk::Instance,
    name: *const c_char,
) -> vk::PFN_vkVoidFunction {
    // SAFETY: the core passes a function's name, as Vulkan defines.
    if let Some(ours) = unsafe { standing_in(name) } {
        return Some(ours);
    }
    let (get, _) = LOOKUPS.get()?;
    // SAFETY: what the core asked, asked of MoltenVK.
    unsafe { get(instance, name) }
}

/// The core's `vkGetDeviceProcAddr`.
unsafe extern "system" fn get_device_proc_addr(
    device: vk::Device,
    name: *const c_char,
) -> vk::PFN_vkVoidFunction {
    // SAFETY: the core passes a function's name, as Vulkan defines.
    if let Some(ours) = unsafe { standing_in(name) } {
        return Some(ours);
    }
    let (_, get) = LOOKUPS.get()?;
    // SAFETY: what the core asked, asked of MoltenVK.
    unsafe { get(device, name) }
}

/// The helper's own function for the Vulkan function `name`, if it stands
/// in for it: the two lookups, so that whatever the core looks up comes
/// through them, and those that make, use, read and destroy caches.
///
/// # Safety
///
/// `name` is null or a NUL-terminated string.
unsafe fn standing_in(name: *const c_char) -> Option<unsafe extern "system" fn()> {
    if name.is_null() {
        return None;
    }
    // SAFETY: as the caller vouches.
    let name = unsafe { CStr::from_ptr(name) };
    let get_instance: vk::PFN_vkGetInstanceProcAddr = get_instance_proc_addr;
    let get_device: vk::PFN_vkGetDeviceProcAddr = get_device_proc_addr;
    let create_cache: vk::PFN_vkCreatePipelineCache = create_pipeline_cache;
    let destroy_cache: vk::PFN_vkDestroyPipelineCache = destroy_pipeline_cache;
    let cache_data: vk::PFN_vkGetPipelineCacheData = get_pipeline_cache_data;
    let graphics: vk::PFN_vkCreateGraphicsPipelines = create_graphics_pipelines;
    let compute: vk::PFN_vkCreateComputePipelines = create_compute_pipelines;
    let address = match name.to_bytes() {
        b"vkGetInstanceProcAddr" => get_instance as *const (),
        b"vkGetDeviceProcAddr" => get_device as *const (),
        b"vkCreatePipelineCache" => create_cache as *const (),
        b"vkDestroyPipelineCache" => destroy_cache as *const (),
        b"vkGetPipelineCacheData" => cache_data as *const (),
        b"vkCreateGraphicsPipelines" => graphics as *const (),
        b"vkCreateComputePipelines" => compute as *const (),
        _ => return None,
    };
    // SAFETY: the address is one of the functions above. Vulkan hands every
    // function back as this one type, and the caller, who asked for it by
    // name, casts it back to its own.
    Some(unsafe { std::mem::transmute::<*const (), unsafe extern "system" fn()>(address) })
}

/// MoltenVK's device functions, found on `device`, the core's.
fn moltenvk(device: vk::Device) -> &'static ash::DeviceFnV1_0 {
    MOLTENVK.get_or_init(|| {
        let get = LOOKUPS.get().map(|(_, get)| *get);
        ash::DeviceFnV1_0::load(|name| match get {
            // SAFETY: the device is MoltenVK's; a function it lacks comes
            // back null, which ash's table turns into a panic if called,
            // and the 1.0 functions are all there.
            Some(get) => unsafe { get(device, name.as_ptr()) }
                .map_or(std::ptr::null(), |function| function as *const c_void),
            None => std::ptr::null(),
        })
    })
}

/// The cache this thread builds through in place of the core's `shared`:
/// made from the same saved data the first time, or `None` if `shared` is
/// none of the core's or a cache cannot be made.
fn thread_cache(device: vk::Device, shared: vk::PipelineCache) -> Option<ThreadCache> {
    MINE.with_borrow_mut(|mine| {
        let alive = |cache: &ThreadCache| {
            cache
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .is_some()
        };
        if let Some(cache) = mine.get(&shared).filter(|cache| alive(cache)) {
            return Some(Arc::clone(cache));
        }
        let initial = caches().get(&shared)?.initial.clone();
        let info = vk::PipelineCacheCreateInfo::default().initial_data(&initial);
        let mut made = vk::PipelineCache::null();
        // SAFETY: `info` and `initial` outlive the call, which writes the
        // new cache to `made`.
        let result = unsafe {
            (moltenvk(device).create_pipeline_cache)(device, &info, std::ptr::null(), &mut made)
        };
        if result != vk::Result::SUCCESS {
            return None;
        }
        let cache = Arc::new(Mutex::new(Some(made)));
        let mut caches = caches();
        let Some(split) = caches.get_mut(&shared) else {
            // The core's went meanwhile.
            // SAFETY: nothing else has the cache just made.
            unsafe { (moltenvk(device).destroy_pipeline_cache)(device, made, std::ptr::null()) };
            return None;
        };
        split.threads.push(Arc::clone(&cache));
        mine.insert(shared, Arc::clone(&cache));
        Some(cache)
    })
}

/// Runs `build` with this thread's cache standing in for the core's
/// `cache`, or with `cache` itself if it has none.
fn through_own_cache(
    device: vk::Device,
    cache: vk::PipelineCache,
    build: impl FnOnce(vk::PipelineCache) -> vk::Result,
) -> vk::Result {
    if cache != vk::PipelineCache::null()
        && let Some(own) = thread_cache(device, cache)
    {
        let own = own.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(own) = *own {
            return build(own);
        }
    }
    build(cache)
}

unsafe extern "system" fn create_pipeline_cache(
    device: vk::Device,
    info: *const vk::PipelineCacheCreateInfo<'_>,
    allocator: *const vk::AllocationCallbacks<'_>,
    made: *mut vk::PipelineCache,
) -> vk::Result {
    // SAFETY: the core's arguments, passed on.
    let result = unsafe { (moltenvk(device).create_pipeline_cache)(device, info, allocator, made) };
    if result == vk::Result::SUCCESS && !info.is_null() && !made.is_null() {
        // SAFETY: Vulkan defines both as valid for the call, the initial
        // data as `initial_data_size` bytes when it is not null.
        let (info, made) = unsafe { (&*info, *made) };
        let initial = if info.p_initial_data.is_null() {
            Vec::new()
        } else {
            // SAFETY: as above.
            unsafe {
                std::slice::from_raw_parts(info.p_initial_data.cast::<u8>(), info.initial_data_size)
            }
            .to_vec()
        };
        caches().insert(
            made,
            Split {
                initial,
                threads: Vec::new(),
            },
        );
    }
    result
}

unsafe extern "system" fn destroy_pipeline_cache(
    device: vk::Device,
    cache: vk::PipelineCache,
    allocator: *const vk::AllocationCallbacks<'_>,
) {
    let split = caches().remove(&cache);
    for thread in split.into_iter().flat_map(|split| split.threads) {
        let own = thread.lock().unwrap_or_else(PoisonError::into_inner).take();
        if let Some(own) = own {
            // SAFETY: the thread's cache is unused once taken.
            unsafe { (moltenvk(device).destroy_pipeline_cache)(device, own, std::ptr::null()) };
        }
    }
    // SAFETY: the core's arguments, passed on.
    unsafe { (moltenvk(device).destroy_pipeline_cache)(device, cache, allocator) };
}

unsafe extern "system" fn get_pipeline_cache_data(
    device: vk::Device,
    cache: vk::PipelineCache,
    size: *mut usize,
    data: *mut c_void,
) -> vk::Result {
    // The core asks for the size first, then the data: merging before the
    // first gives the size for both.
    if data.is_null()
        && let Some(split) = caches().get(&cache)
    {
        let guards: Vec<_> = split
            .threads
            .iter()
            .map(|thread| thread.lock().unwrap_or_else(PoisonError::into_inner))
            .collect();
        let sources: Vec<vk::PipelineCache> = guards.iter().filter_map(|own| **own).collect();
        if !sources.is_empty() {
            // A merge that fails leaves the core's cache as it was, which
            // is saved as it would have been without the threads'.
            // SAFETY: every thread's cache is held, so none is in use, and
            // the core's own is used by nothing else: its pipelines are
            // built through the threads' caches.
            let _ = unsafe {
                (moltenvk(device).merge_pipeline_caches)(
                    device,
                    cache,
                    u32::try_from(sources.len()).unwrap_or(u32::MAX),
                    sources.as_ptr(),
                )
            };
        }
    }
    // SAFETY: the core's arguments, passed on.
    unsafe { (moltenvk(device).get_pipeline_cache_data)(device, cache, size, data) }
}

unsafe extern "system" fn create_graphics_pipelines(
    device: vk::Device,
    cache: vk::PipelineCache,
    count: u32,
    infos: *const vk::GraphicsPipelineCreateInfo<'_>,
    allocator: *const vk::AllocationCallbacks<'_>,
    pipelines: *mut vk::Pipeline,
) -> vk::Result {
    through_own_cache(device, cache, |cache| {
        // SAFETY: the core's arguments, but for a cache made from the
        // same data.
        unsafe {
            (moltenvk(device).create_graphics_pipelines)(
                device, cache, count, infos, allocator, pipelines,
            )
        }
    })
}

unsafe extern "system" fn create_compute_pipelines(
    device: vk::Device,
    cache: vk::PipelineCache,
    count: u32,
    infos: *const vk::ComputePipelineCreateInfo<'_>,
    allocator: *const vk::AllocationCallbacks<'_>,
    pipelines: *mut vk::Pipeline,
) -> vk::Result {
    through_own_cache(device, cache, |cache| {
        // SAFETY: as for graphics pipelines.
        unsafe {
            (moltenvk(device).create_compute_pipelines)(
                device, cache, count, infos, allocator, pipelines,
            )
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_stands_in_for_the_cache_functions_and_the_lookups_only() {
        for name in [
            c"vkGetInstanceProcAddr",
            c"vkGetDeviceProcAddr",
            c"vkCreatePipelineCache",
            c"vkDestroyPipelineCache",
            c"vkGetPipelineCacheData",
            c"vkCreateGraphicsPipelines",
            c"vkCreateComputePipelines",
        ] {
            // SAFETY: a NUL-terminated name.
            assert!(unsafe { standing_in(name.as_ptr()) }.is_some(), "{name:?}");
        }
        for name in [
            c"vkQueueSubmit",
            c"vkMergePipelineCaches",
            c"vkCreatePipelineCacheX",
        ] {
            // SAFETY: as above.
            assert!(unsafe { standing_in(name.as_ptr()) }.is_none(), "{name:?}");
        }
        // SAFETY: null is allowed.
        assert!(unsafe { standing_in(std::ptr::null()) }.is_none());
    }
}
