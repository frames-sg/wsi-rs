use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use libloading::{Library, Symbol};

#[repr(C)]
struct OpenSlideHandle {
    _private: [u8; 0],
}

#[repr(C)]
struct OpenSlideCacheHandle {
    _private: [u8; 0],
}

type Open = unsafe extern "C" fn(*const c_char) -> *mut OpenSlideHandle;
type Close = unsafe extern "C" fn(*mut OpenSlideHandle);
type GetError = unsafe extern "C" fn(*mut OpenSlideHandle) -> *const c_char;
type GetVersion = unsafe extern "C" fn() -> *const c_char;
type GetLevelCount = unsafe extern "C" fn(*mut OpenSlideHandle) -> c_int;
type GetLevelDimensions = unsafe extern "C" fn(*mut OpenSlideHandle, c_int, *mut i64, *mut i64);
type GetLevelDownsample = unsafe extern "C" fn(*mut OpenSlideHandle, c_int) -> f64;
type GetPropertyValue = unsafe extern "C" fn(*mut OpenSlideHandle, *const c_char) -> *const c_char;
type ReadRegion = unsafe extern "C" fn(*mut OpenSlideHandle, *mut u32, i64, i64, c_int, i64, i64);
type CacheCreate = unsafe extern "C" fn(usize) -> *mut OpenSlideCacheHandle;
type SetCache = unsafe extern "C" fn(*mut OpenSlideHandle, *mut OpenSlideCacheHandle);
type CacheRelease = unsafe extern "C" fn(*mut OpenSlideCacheHandle);
type GetAssociatedImageNames = unsafe extern "C" fn(*mut OpenSlideHandle) -> *const *const c_char;
type GetAssociatedImageDimensions =
    unsafe extern "C" fn(*mut OpenSlideHandle, *const c_char, *mut i64, *mut i64);

struct Api {
    _library: Library,
    open: Open,
    close: Close,
    get_error: GetError,
    get_version: GetVersion,
    get_level_count: GetLevelCount,
    get_level_dimensions: GetLevelDimensions,
    get_level_downsample: GetLevelDownsample,
    get_property_value: GetPropertyValue,
    read_region: ReadRegion,
    cache: Option<(CacheCreate, SetCache, CacheRelease)>,
    associated_images: Option<(GetAssociatedImageNames, GetAssociatedImageDimensions)>,
}

#[derive(Clone)]
pub struct OpenSlideApi(Arc<Api>);

impl OpenSlideApi {
    pub fn load(path: &Path) -> Result<Self, String> {
        // SAFETY: Callers explicitly select an OpenSlide-compatible library.
        // The library remains owned by `Api` for every copied function pointer.
        let library = unsafe { Library::new(path) }
            .map_err(|error| format!("failed to load {}: {error}", path.display()))?;
        // SAFETY: These types match the corresponding OpenSlide C declarations,
        // and the returned `Api` owns `library` for their full lifetime.
        let api = unsafe {
            let cache = optional_symbol_set3(
                &library,
                b"openslide_cache_create\0",
                b"openslide_set_cache\0",
                b"openslide_cache_release\0",
            )?;
            let associated_images = optional_symbol_set2(
                &library,
                b"openslide_get_associated_image_names\0",
                b"openslide_get_associated_image_dimensions\0",
            )?;
            Api {
                open: load_symbol(&library, b"openslide_open\0")?,
                close: load_symbol(&library, b"openslide_close\0")?,
                get_error: load_symbol(&library, b"openslide_get_error\0")?,
                get_version: load_symbol(&library, b"openslide_get_version\0")?,
                get_level_count: load_symbol(&library, b"openslide_get_level_count\0")?,
                get_level_dimensions: load_symbol(&library, b"openslide_get_level_dimensions\0")?,
                get_level_downsample: load_symbol(&library, b"openslide_get_level_downsample\0")?,
                get_property_value: load_symbol(&library, b"openslide_get_property_value\0")?,
                read_region: load_symbol(&library, b"openslide_read_region\0")?,
                cache,
                associated_images,
                _library: library,
            }
        };
        Ok(Self(Arc::new(api)))
    }

    pub fn discover() -> Result<Self, String> {
        let mut errors = Vec::new();
        for path in library_candidates() {
            match Self::load(&path) {
                Ok(api) => return Ok(api),
                Err(error) => errors.push(error),
            }
        }
        Err(format!(
            "failed to load libopenslide; tried: {}",
            errors.join(" | ")
        ))
    }

    pub fn version(&self) -> Result<String, String> {
        // SAFETY: The function pointer has the OpenSlide ABI and its library is live.
        let raw = unsafe { (self.0.get_version)() };
        if raw.is_null() {
            return Err("openslide_get_version returned NULL".into());
        }
        // SAFETY: OpenSlide returns a library-owned NUL-terminated string.
        Ok(unsafe { CStr::from_ptr(raw) }
            .to_string_lossy()
            .into_owned())
    }

    pub fn create_cache(&self, capacity: usize) -> Result<OpenSlideCache, String> {
        let (create, _, release) = self
            .0
            .cache
            .ok_or_else(|| "OpenSlide library does not expose cache functions".to_string())?;
        // SAFETY: Capacity is an ordinary value and a non-null returned handle
        // transfers ownership to `CacheInner`.
        let raw = unsafe { create(capacity) };
        if raw.is_null() {
            return Err(format!(
                "openslide_cache_create returned NULL for {capacity} bytes"
            ));
        }
        Ok(OpenSlideCache(Arc::new(CacheInner {
            raw,
            release,
            api: Arc::clone(&self.0),
        })))
    }

    pub fn open(&self, path: &Path) -> Result<OpenSlide, String> {
        open_handle(Arc::clone(&self.0), path, None)
    }

    pub fn open_with_cache(
        &self,
        path: &Path,
        cache: &OpenSlideCache,
    ) -> Result<OpenSlide, String> {
        if !Arc::ptr_eq(&self.0, &cache.0.api) {
            return Err("cache and slide libraries differ".into());
        }
        let mut slide = open_handle(Arc::clone(&self.0), path, Some(Arc::clone(&cache.0)))?;
        let (_, set, _) = self
            .0
            .cache
            .expect("cache handle exists only when all cache functions loaded");
        // SAFETY: Both opaque handles were created by this API and remain live.
        unsafe { set(slide.raw, cache.0.raw) };
        slide.check_error()?;
        slide._cache = Some(Arc::clone(&cache.0));
        Ok(slide)
    }
}

pub fn try_load() -> Option<OpenSlideApi> {
    OpenSlideApi::discover().ok()
}

#[derive(Clone)]
pub struct OpenSlideCache(Arc<CacheInner>);

struct CacheInner {
    raw: *mut OpenSlideCacheHandle,
    release: CacheRelease,
    api: Arc<Api>,
}

// SAFETY: OpenSlide cache handles are documented as thread-safe and may be
// shared by multiple slide handles. `CacheInner` keeps the defining API live
// and releases the opaque handle only after the final `Arc` is dropped.
unsafe impl Send for CacheInner {}
// SAFETY: Cache operations occur inside OpenSlide; Rust code only shares the
// opaque handle and does not dereference or mutate its storage directly.
unsafe impl Sync for CacheInner {}

impl Drop for CacheInner {
    fn drop(&mut self) {
        // SAFETY: This object uniquely owns the live cache handle.
        unsafe { (self.release)(self.raw) };
    }
}

pub struct OpenSlide {
    raw: *mut OpenSlideHandle,
    api: Arc<Api>,
    _cache: Option<Arc<CacheInner>>,
}

// SAFETY: OpenSlide documents handle operations as thread-safe. The wrapper
// owns the handle and keeps the defining dynamic library and cache alive.
unsafe impl Send for OpenSlide {}
// SAFETY: Shared calls use OpenSlide's thread-safe C entry points, while Rust's
// ownership prevents the handle from being closed during a live borrow.
unsafe impl Sync for OpenSlide {}

impl OpenSlide {
    pub fn open(path: &Path) -> Result<Self, String> {
        OpenSlideApi::discover()?.open(path)
    }

    pub fn level_count(&self) -> u32 {
        // SAFETY: `raw` is a live OpenSlide handle.
        let count = unsafe { (self.api.get_level_count)(self.raw) };
        count.max(0) as u32
    }

    pub fn level_dimensions(&self, level: u32) -> (u64, u64) {
        let mut width = 0i64;
        let mut height = 0i64;
        // SAFETY: `raw` is live and both output pointers are writable.
        unsafe {
            (self.api.get_level_dimensions)(self.raw, level as c_int, &mut width, &mut height);
        }
        (width.max(0) as u64, height.max(0) as u64)
    }

    pub fn levels(&self) -> Result<Vec<OpenSlideLevel>, String> {
        // SAFETY: `raw` is a live OpenSlide handle.
        let count = unsafe { (self.api.get_level_count)(self.raw) };
        self.check_error()?;
        let count = usize::try_from(count).map_err(|_| format!("invalid level count {count}"))?;
        if count == 0 {
            return Err("slide reports no levels".into());
        }
        (0..count)
            .map(|index| {
                let level = c_int::try_from(index)
                    .map_err(|_| format!("level index {index} exceeds c_int"))?;
                let mut width = -1i64;
                let mut height = -1i64;
                // SAFETY: Output pointers are writable and the index is in range.
                unsafe {
                    (self.api.get_level_dimensions)(self.raw, level, &mut width, &mut height);
                }
                self.check_error()?;
                // SAFETY: The same live handle and valid level are used.
                let downsample = unsafe { (self.api.get_level_downsample)(self.raw, level) };
                self.check_error()?;
                Ok(OpenSlideLevel {
                    width: u64::try_from(width)
                        .map_err(|_| format!("level {index} has invalid width {width}"))?,
                    height: u64::try_from(height)
                        .map_err(|_| format!("level {index} has invalid height {height}"))?,
                    downsample,
                })
            })
            .collect()
    }

    pub fn property(&self, name: &str) -> Option<String> {
        let name = CString::new(name).ok()?;
        self.property_value(&name).ok().flatten()
    }

    pub fn level0_bounds(&self) -> Result<Option<OpenSlideBounds>, String> {
        let x = self.property_value(c"openslide.bounds-x")?;
        let y = self.property_value(c"openslide.bounds-y")?;
        let width = self.property_value(c"openslide.bounds-width")?;
        let height = self.property_value(c"openslide.bounds-height")?;
        let present = [x.is_some(), y.is_some(), width.is_some(), height.is_some()];
        if present.iter().all(|value| !value) {
            return Ok(None);
        }
        if present.iter().any(|value| !value) {
            return Err(
                "OpenSlide tissue bounds are partial; expected x, y, width, and height".into(),
            );
        }
        let bounds = parse_bounds_from_properties(|name| match name {
            "openslide.bounds-x" => x.clone(),
            "openslide.bounds-y" => y.clone(),
            "openslide.bounds-width" => width.clone(),
            "openslide.bounds-height" => height.clone(),
            _ => None,
        })
        .ok_or_else(|| "OpenSlide tissue bounds are invalid or empty".to_string())?;
        Ok(Some(bounds))
    }

    pub fn bounds(&self) -> Option<OpenSlideBounds> {
        self.level0_bounds().ok().flatten()
    }

    pub fn read_region_argb_into(
        &self,
        x: i64,
        y: i64,
        level: u32,
        width: u32,
        height: u32,
        buffer: &mut Vec<u32>,
    ) -> Result<(), String> {
        let len = usize::try_from(width)
            .ok()
            .and_then(|width| {
                usize::try_from(height)
                    .ok()
                    .and_then(|height| width.checked_mul(height))
            })
            .ok_or_else(|| format!("region dimensions overflow: {width}x{height}"))?;
        buffer.resize(len, 0);
        let level = c_int::try_from(level).map_err(|_| format!("level {level} exceeds c_int"))?;
        // SAFETY: The destination contains the checked number of writable pixels.
        unsafe {
            (self.api.read_region)(
                self.raw,
                buffer.as_mut_ptr(),
                x,
                y,
                level,
                i64::from(width),
                i64::from(height),
            );
        }
        self.check_error()
    }

    pub fn read_region(
        &self,
        x: i64,
        y: i64,
        level: u32,
        width: u32,
        height: u32,
    ) -> Result<Vec<u8>, String> {
        let mut argb = Vec::new();
        self.read_region_argb_into(x, y, level, width, height, &mut argb)?;
        Ok(argb_to_rgba(argb))
    }

    pub fn read_region_rgba(
        &self,
        x: i64,
        y: i64,
        level: i32,
        width: u32,
        height: u32,
    ) -> Result<Vec<u8>, String> {
        let level = u32::try_from(level).map_err(|_| format!("level {level} is negative"))?;
        self.read_region(x, y, level, width, height)
    }

    pub fn associated_names(&self) -> Vec<String> {
        let Some((names, _)) = self.api.associated_images else {
            return Vec::new();
        };
        // SAFETY: `raw` is live and the returned array is null terminated.
        let names = unsafe { names(self.raw) };
        if names.is_null() {
            return Vec::new();
        }
        let mut output = Vec::new();
        for index in 0.. {
            // SAFETY: OpenSlide guarantees a null-terminated pointer array.
            let name = unsafe { *names.add(index) };
            if name.is_null() {
                break;
            }
            // SAFETY: Each non-null entry is a library-owned C string.
            output.push(
                unsafe { CStr::from_ptr(name) }
                    .to_string_lossy()
                    .into_owned(),
            );
        }
        output
    }

    pub fn associated_dimensions(&self, name: &str) -> Result<(u32, u32), String> {
        let (_, dimensions) = self.api.associated_images.ok_or_else(|| {
            "OpenSlide library does not expose associated-image functions".to_string()
        })?;
        let name = CString::new(name).map_err(|error| error.to_string())?;
        let mut width = 0i64;
        let mut height = 0i64;
        // SAFETY: The handle and C string are live and outputs are writable.
        unsafe { dimensions(self.raw, name.as_ptr(), &mut width, &mut height) };
        self.check_error()?;
        Ok((
            u32::try_from(width)
                .map_err(|_| format!("associated image width out of range: {width}"))?,
            u32::try_from(height)
                .map_err(|_| format!("associated image height out of range: {height}"))?,
        ))
    }

    fn property_value(&self, name: &CStr) -> Result<Option<String>, String> {
        // SAFETY: The handle is live and `name` is NUL terminated.
        let value = unsafe { (self.api.get_property_value)(self.raw, name.as_ptr()) };
        self.check_error()?;
        if value.is_null() {
            return Ok(None);
        }
        // SAFETY: A non-null property value is a library-owned C string.
        Ok(Some(
            unsafe { CStr::from_ptr(value) }
                .to_string_lossy()
                .into_owned(),
        ))
    }

    fn check_error(&self) -> Result<(), String> {
        // SAFETY: `raw` is a live OpenSlide handle.
        let error = unsafe { (self.api.get_error)(self.raw) };
        if error.is_null() {
            return Ok(());
        }
        // SAFETY: OpenSlide error pointers are NUL-terminated and library owned.
        Err(unsafe { CStr::from_ptr(error) }
            .to_string_lossy()
            .into_owned())
    }
}

impl Drop for OpenSlide {
    fn drop(&mut self) {
        // SAFETY: This wrapper uniquely owns the live slide handle.
        unsafe { (self.api.close)(self.raw) };
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OpenSlideLevel {
    pub width: u64,
    pub height: u64,
    pub downsample: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpenSlideBounds {
    pub x: i64,
    pub y: i64,
    pub width: u64,
    pub height: u64,
}

pub fn parse_bounds_from_properties<F>(mut property: F) -> Option<OpenSlideBounds>
where
    F: FnMut(&str) -> Option<String>,
{
    let x = property("openslide.bounds-x")?.parse::<i64>().ok()?;
    let y = property("openslide.bounds-y")?.parse::<i64>().ok()?;
    let width = property("openslide.bounds-width")?.parse::<u64>().ok()?;
    let height = property("openslide.bounds-height")?.parse::<u64>().ok()?;
    (width > 0 && height > 0).then_some(OpenSlideBounds {
        x,
        y,
        width,
        height,
    })
}

fn open_handle(
    api: Arc<Api>,
    path: &Path,
    cache: Option<Arc<CacheInner>>,
) -> Result<OpenSlide, String> {
    let path_text = path
        .to_str()
        .ok_or_else(|| format!("slide path is not UTF-8: {}", path.display()))?;
    let path = CString::new(path_text.as_bytes())
        .map_err(|_| format!("slide path contains NUL: {path_text:?}"))?;
    // SAFETY: The C string is live for the call and a non-null handle transfers
    // ownership to the returned wrapper.
    let raw = unsafe { (api.open)(path.as_ptr()) };
    if raw.is_null() {
        return Err("openslide_open returned NULL".into());
    }
    let slide = OpenSlide {
        raw,
        api,
        _cache: cache,
    };
    slide.check_error()?;
    Ok(slide)
}

fn argb_to_rgba(argb: Vec<u32>) -> Vec<u8> {
    let mut rgba = Vec::with_capacity(argb.len() * 4);
    for pixel in argb {
        let a = ((pixel >> 24) & 0xff) as u8;
        let r = ((pixel >> 16) & 0xff) as u8;
        let g = ((pixel >> 8) & 0xff) as u8;
        let b = (pixel & 0xff) as u8;
        if a == 0 {
            rgba.extend_from_slice(&[0, 0, 0, 0]);
        } else {
            let unpremultiply = |channel: u8| {
                ((u16::from(channel) * 255 + u16::from(a) / 2) / u16::from(a)).min(255) as u8
            };
            rgba.extend_from_slice(&[unpremultiply(r), unpremultiply(g), unpremultiply(b), a]);
        }
    }
    rgba
}

fn library_candidates() -> Vec<PathBuf> {
    let mut candidates = ["OPENSLIDE_LIB_PATH", "WSI_RS_OPENSLIDE_LIBRARY"]
        .into_iter()
        .filter_map(std::env::var_os)
        .map(PathBuf::from)
        .collect::<Vec<_>>();
    candidates.extend(
        [
            "/opt/homebrew/lib/libopenslide.1.dylib",
            "/opt/homebrew/lib/libopenslide.dylib",
            "/usr/local/lib/libopenslide.1.dylib",
            "/usr/local/lib/libopenslide.dylib",
            "/usr/lib/x86_64-linux-gnu/libopenslide.so.0",
            "/usr/lib/x86_64-linux-gnu/libopenslide.so",
            "/usr/lib/libopenslide.so.0",
            "/usr/lib/libopenslide.so",
            "libopenslide.1.dylib",
            "libopenslide.dylib",
            "libopenslide.so.1",
            "libopenslide.so.0",
            "libopenslide.so",
            "libopenslide.dll",
            r"C:\Program Files\OpenSlide\bin\libopenslide.dll",
        ]
        .into_iter()
        .map(PathBuf::from),
    );
    candidates
}

/// # Safety
/// `name` must identify a symbol whose C ABI exactly matches `T`.
unsafe fn load_symbol<T: Copy>(library: &Library, name: &[u8]) -> Result<T, String> {
    // SAFETY: The caller supplies the exact ABI type and `library` remains live.
    let symbol: Symbol<'_, T> = unsafe { library.get(name) }.map_err(|error| {
        let name = CStr::from_bytes_with_nul(name)
            .map_or_else(|_| "<invalid>".into(), |name| name.to_string_lossy());
        format!("missing symbol {name}: {error}")
    })?;
    Ok(*symbol)
}

/// # Safety
/// The three symbol names must have C ABIs matching `A`, `B`, and `C`.
unsafe fn optional_symbol_set3<A: Copy, B: Copy, C: Copy>(
    library: &Library,
    first: &[u8],
    second: &[u8],
    third: &[u8],
) -> Result<Option<(A, B, C)>, String> {
    // SAFETY: The caller supplies the matching ABI types for every name.
    let values = unsafe {
        (
            library.get::<A>(first).ok().map(|symbol| *symbol),
            library.get::<B>(second).ok().map(|symbol| *symbol),
            library.get::<C>(third).ok().map(|symbol| *symbol),
        )
    };
    match values {
        (None, None, None) => Ok(None),
        (Some(first), Some(second), Some(third)) => Ok(Some((first, second, third))),
        _ => Err("OpenSlide library exposes an incomplete cache API".into()),
    }
}

/// # Safety
/// The two symbol names must have C ABIs matching `A` and `B`.
unsafe fn optional_symbol_set2<A: Copy, B: Copy>(
    library: &Library,
    first: &[u8],
    second: &[u8],
) -> Result<Option<(A, B)>, String> {
    // SAFETY: The caller supplies the matching ABI types for both names.
    let values = unsafe {
        (
            library.get::<A>(first).ok().map(|symbol| *symbol),
            library.get::<B>(second).ok().map(|symbol| *symbol),
        )
    };
    match values {
        (None, None) => Ok(None),
        (Some(first), Some(second)) => Ok(Some((first, second))),
        _ => Err("OpenSlide library exposes an incomplete associated-image API".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ptr;

    #[derive(Clone, Copy)]
    enum FakeMode {
        Normal,
        ZeroLevels,
        InvalidDimensions,
        PartialBounds,
        CacheError,
        ReadError,
    }

    struct FakeSlide {
        mode: FakeMode,
        error: Option<CString>,
    }

    unsafe extern "C" fn fake_open(path: *const c_char) -> *mut OpenSlideHandle {
        // SAFETY: The client supplies a live NUL-terminated path for this call.
        let path = unsafe { CStr::from_ptr(path) }.to_string_lossy();
        if path.contains("null-open") {
            return ptr::null_mut();
        }
        let mode = if path.contains("zero-levels") {
            FakeMode::ZeroLevels
        } else if path.contains("invalid-dimensions") {
            FakeMode::InvalidDimensions
        } else if path.contains("partial-bounds") {
            FakeMode::PartialBounds
        } else if path.contains("cache-error") {
            FakeMode::CacheError
        } else if path.contains("read-error") {
            FakeMode::ReadError
        } else {
            FakeMode::Normal
        };
        let error = path
            .contains("open-error")
            .then(|| CString::new("fake open failure").unwrap());
        Box::into_raw(Box::new(FakeSlide { mode, error })).cast()
    }

    unsafe extern "C" fn fake_close(slide: *mut OpenSlideHandle) {
        if !slide.is_null() {
            // SAFETY: `fake_open` allocated this handle and transfers it back once.
            drop(unsafe { Box::from_raw(slide.cast::<FakeSlide>()) });
        }
    }

    unsafe extern "C" fn fake_error(slide: *mut OpenSlideHandle) -> *const c_char {
        // SAFETY: Every handle comes from `fake_open` and remains live.
        let slide = unsafe { &*slide.cast::<FakeSlide>() };
        slide
            .error
            .as_ref()
            .map_or(ptr::null(), |error| error.as_ptr())
    }

    unsafe extern "C" fn fake_version() -> *const c_char {
        c"4.0.1-fake".as_ptr()
    }

    unsafe extern "C" fn null_version() -> *const c_char {
        ptr::null()
    }

    unsafe extern "C" fn fake_level_count(slide: *mut OpenSlideHandle) -> c_int {
        // SAFETY: Every handle comes from `fake_open`.
        let slide = unsafe { &*slide.cast::<FakeSlide>() };
        if matches!(slide.mode, FakeMode::ZeroLevels) {
            0
        } else {
            3
        }
    }

    unsafe extern "C" fn fake_level_dimensions(
        slide: *mut OpenSlideHandle,
        level: c_int,
        width: *mut i64,
        height: *mut i64,
    ) {
        // SAFETY: The handle and output pointers satisfy the fake ABI contract.
        let slide = unsafe { &*slide.cast::<FakeSlide>() };
        let side = if matches!(slide.mode, FakeMode::InvalidDimensions) {
            -1
        } else {
            4_096 >> level
        };
        unsafe {
            *width = side;
            *height = side;
        }
    }

    unsafe extern "C" fn fake_level_downsample(_slide: *mut OpenSlideHandle, level: c_int) -> f64 {
        f64::from(1 << level)
    }

    unsafe extern "C" fn fake_property_value(
        slide: *mut OpenSlideHandle,
        name: *const c_char,
    ) -> *const c_char {
        // SAFETY: The handle and name satisfy the fake ABI contract.
        let slide = unsafe { &*slide.cast::<FakeSlide>() };
        let name = unsafe { CStr::from_ptr(name) }.to_bytes();
        if matches!(slide.mode, FakeMode::ZeroLevels) {
            return ptr::null();
        }
        if matches!(slide.mode, FakeMode::PartialBounds) && name != b"openslide.bounds-x" {
            return ptr::null();
        }
        if matches!(slide.mode, FakeMode::InvalidDimensions) && name == b"openslide.bounds-width" {
            return c"invalid".as_ptr();
        }
        match name {
            b"openslide.bounds-x" => c"100".as_ptr(),
            b"openslide.bounds-y" => c"200".as_ptr(),
            b"openslide.bounds-width" => c"3000".as_ptr(),
            b"openslide.bounds-height" => c"2000".as_ptr(),
            _ => ptr::null(),
        }
    }

    unsafe extern "C" fn fake_read_region(
        slide: *mut OpenSlideHandle,
        destination: *mut u32,
        x: i64,
        y: i64,
        level: c_int,
        width: i64,
        height: i64,
    ) {
        let len = usize::try_from(width * height).unwrap();
        // SAFETY: The client allocates exactly this many output pixels.
        let pixels = unsafe { std::slice::from_raw_parts_mut(destination, len) };
        for (index, pixel) in pixels.iter_mut().enumerate() {
            *pixel = 0xff00_0000
                | (u32::try_from(x + y + i64::from(level)).unwrap_or(0) & 0xffff)
                | u32::try_from(index).unwrap_or(u32::MAX);
        }
        // SAFETY: Every handle comes from `fake_open`.
        let slide = unsafe { &mut *slide.cast::<FakeSlide>() };
        if matches!(slide.mode, FakeMode::ReadError) {
            slide.error = Some(CString::new("fake read failure").unwrap());
        }
    }

    unsafe extern "C" fn fake_cache_create(capacity: usize) -> *mut OpenSlideCacheHandle {
        if capacity == 13 {
            ptr::null_mut()
        } else {
            Box::into_raw(Box::new(capacity)).cast()
        }
    }

    unsafe extern "C" fn fake_set_cache(
        slide: *mut OpenSlideHandle,
        _cache: *mut OpenSlideCacheHandle,
    ) {
        // SAFETY: Every handle comes from `fake_open`.
        let slide = unsafe { &mut *slide.cast::<FakeSlide>() };
        if matches!(slide.mode, FakeMode::CacheError) {
            slide.error = Some(CString::new("fake cache failure").unwrap());
        }
    }

    unsafe extern "C" fn fake_cache_release(cache: *mut OpenSlideCacheHandle) {
        if !cache.is_null() {
            // SAFETY: `fake_cache_create` allocated this handle and transfers it back once.
            drop(unsafe { Box::from_raw(cache.cast::<usize>()) });
        }
    }

    #[cfg(unix)]
    fn current_process_library() -> Library {
        libloading::os::unix::Library::this().into()
    }

    #[cfg(windows)]
    fn current_process_library() -> Library {
        libloading::os::windows::Library::this().unwrap().into()
    }

    fn fake_api(version: GetVersion) -> OpenSlideApi {
        OpenSlideApi(Arc::new(Api {
            _library: current_process_library(),
            open: fake_open,
            close: fake_close,
            get_error: fake_error,
            get_version: version,
            get_level_count: fake_level_count,
            get_level_dimensions: fake_level_dimensions,
            get_level_downsample: fake_level_downsample,
            get_property_value: fake_property_value,
            read_region: fake_read_region,
            cache: Some((fake_cache_create, fake_set_cache, fake_cache_release)),
            associated_images: None,
        }))
    }

    fn error<T>(result: Result<T, String>) -> String {
        result.err().expect("operation should fail")
    }

    #[test]
    fn bounds_parser_rejects_partial_and_empty_bounds() {
        assert!(parse_bounds_from_properties(|_| None).is_none());
        let values = [("openslide.bounds-x", "1"), ("openslide.bounds-y", "2")];
        assert!(parse_bounds_from_properties(|name| values
            .iter()
            .find(|(key, _)| *key == name)
            .map(|(_, value)| (*value).to_string()))
        .is_none());
    }

    #[test]
    fn argb_conversion_unpremultiplies_and_preserves_transparency() {
        assert_eq!(
            argb_to_rgba(vec![0, 0x8010_2030]),
            vec![0, 0, 0, 0, 32, 64, 96, 128]
        );
    }

    #[test]
    fn fake_abi_exercises_cache_slide_levels_pixels_and_drops() {
        let api = fake_api(fake_version);
        assert_eq!(api.version().unwrap(), "4.0.1-fake");
        let cache = api.create_cache(1_024).unwrap();
        let slide = api
            .open_with_cache(Path::new("normal.svs"), &cache)
            .unwrap();
        assert_eq!(
            slide.levels().unwrap(),
            vec![
                OpenSlideLevel {
                    width: 4_096,
                    height: 4_096,
                    downsample: 1.0,
                },
                OpenSlideLevel {
                    width: 2_048,
                    height: 2_048,
                    downsample: 2.0,
                },
                OpenSlideLevel {
                    width: 1_024,
                    height: 1_024,
                    downsample: 4.0,
                },
            ]
        );
        assert_eq!(
            slide.level0_bounds().unwrap(),
            Some(OpenSlideBounds {
                x: 100,
                y: 200,
                width: 3_000,
                height: 2_000,
            })
        );
        let mut pixels = Vec::new();
        slide
            .read_region_argb_into(3, 5, 1, 2, 2, &mut pixels)
            .unwrap();
        assert_eq!(
            pixels,
            vec![0xff00_0009, 0xff00_0009, 0xff00_000b, 0xff00_000b]
        );
    }

    #[test]
    fn ffi_errors_are_explicit_and_release_owned_handles() {
        let api = fake_api(fake_version);
        let cache = api.create_cache(1_024).unwrap();
        assert!(error(api.create_cache(13)).contains("returned NULL"));
        assert!(error(api.open_with_cache(Path::new("null-open.svs"), &cache)).contains("NULL"));
        assert!(
            error(api.open_with_cache(Path::new("open-error.svs"), &cache)).contains("fake open")
        );
        assert!(
            error(api.open_with_cache(Path::new("cache-error.svs"), &cache)).contains("fake cache")
        );

        let zero = api
            .open_with_cache(Path::new("zero-levels.svs"), &cache)
            .unwrap();
        assert!(zero.levels().unwrap_err().contains("no levels"));
        assert_eq!(zero.level0_bounds().unwrap(), None);
        let invalid = api
            .open_with_cache(Path::new("invalid-dimensions.svs"), &cache)
            .unwrap();
        assert!(invalid.levels().unwrap_err().contains("invalid width"));
        assert!(invalid.level0_bounds().unwrap_err().contains("invalid"));
        let partial = api
            .open_with_cache(Path::new("partial-bounds.svs"), &cache)
            .unwrap();
        assert!(partial.level0_bounds().unwrap_err().contains("partial"));

        let read_error = api
            .open_with_cache(Path::new("read-error.svs"), &cache)
            .unwrap();
        assert!(read_error
            .read_region_argb_into(0, 0, 0, 1, 1, &mut Vec::new())
            .unwrap_err()
            .contains("fake read"));
        let other_api = fake_api(fake_version);
        assert!(
            error(other_api.open_with_cache(Path::new("normal.svs"), &cache))
                .contains("libraries differ")
        );
        assert!(error(fake_api(null_version).version()).contains("returned NULL"));
    }

    #[test]
    fn dynamic_loading_reports_library_and_symbol_errors() {
        assert!(OpenSlideApi::load(Path::new("definitely-missing-openslide-library")).is_err());
        let library = current_process_library();
        // SAFETY: The requested pointer is never returned or called.
        let error = unsafe {
            load_symbol::<GetVersion>(&library, b"definitely_missing_openslide_symbol\0")
        }
        .unwrap_err();
        assert!(error.contains("missing symbol"));
    }
}
