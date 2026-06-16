use super::jpeg::read_vms_jpeg_header;
use super::model::invalid_slide;
use super::*;

pub(super) const GROUP_VMS: &str = "Virtual Microscope Specimen";
pub(super) const KEY_MAP_FILE: &str = "MapFile";
pub(super) const KEY_IMAGE_FILE: &str = "ImageFile";
pub(super) const KEY_NUM_JPEG_COLS: &str = "NoJpegColumns";
pub(super) const KEY_NUM_JPEG_ROWS: &str = "NoJpegRows";
pub(super) const KEY_OPTIMISATION_FILE: &str = "OptimisationFile";
pub(super) const KEY_MACRO_IMAGE: &str = "MacroImage";
pub(super) const KEY_PHYSICAL_WIDTH: &str = "PhysicalWidth";
pub(super) const KEY_PHYSICAL_HEIGHT: &str = "PhysicalHeight";
pub(super) const KEY_SOURCE_LENS: &str = "SourceLens";
const KEY_FILE_MAX_SIZE: u64 = 64 << 10;

#[derive(Default)]
pub(super) struct ParsedIni {
    pub(super) groups: HashMap<String, HashMap<String, String>>,
}

pub(super) fn parse_vms_ini(path: &Path) -> Result<ParsedIni, WsiError> {
    let metadata = std::fs::metadata(path).map_err(|source| WsiError::IoWithPath {
        source: Arc::new(source),
        path: path.to_path_buf(),
    })?;
    if metadata.len() > KEY_FILE_MAX_SIZE {
        return Err(invalid_slide(path, "VMS key file too large"));
    }
    let text = std::fs::read_to_string(path).map_err(|source| WsiError::IoWithPath {
        source: Arc::new(source),
        path: path.to_path_buf(),
    })?;
    let mut parsed = ParsedIni::default();
    let mut current_group: Option<String> = None;
    for raw_line in text.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with(';') || line.starts_with('#') {
            continue;
        }
        if let Some(group) = line
            .strip_prefix('[')
            .and_then(|line| line.strip_suffix(']'))
        {
            current_group = Some(group.to_string());
            parsed.groups.entry(group.to_string()).or_default();
            continue;
        }
        let Some(group) = current_group.as_ref() else {
            continue;
        };
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        parsed
            .groups
            .entry(group.clone())
            .or_default()
            .insert(key.trim().to_string(), value.trim().to_string());
    }
    Ok(parsed)
}

pub(super) fn parse_u32(
    path: &Path,
    group: &HashMap<String, String>,
    key: &str,
) -> Result<u32, WsiError> {
    group
        .get(key)
        .ok_or_else(|| invalid_slide(path, format!("missing {key}")))?
        .parse::<u32>()
        .map_err(|_| invalid_slide(path, format!("invalid integer for {key}")))
}

pub(super) struct ImageDims {
    pub(super) layer: u32,
    pub(super) col: u32,
    pub(super) row: u32,
}

pub(super) fn parse_image_key_suffix(path: &Path, key: &str) -> Result<ImageDims, WsiError> {
    let suffix = &key[KEY_IMAGE_FILE.len()..];
    if suffix.is_empty() {
        return Ok(ImageDims {
            layer: 0,
            col: 0,
            row: 0,
        });
    }
    let trimmed = suffix
        .strip_prefix('(')
        .and_then(|value| value.strip_suffix(')'))
        .ok_or_else(|| invalid_slide(path, format!("invalid VMS image key suffix: {suffix}")))?;
    let parts: Vec<&str> = trimmed.split(',').map(str::trim).collect();
    match parts.as_slice() {
        [col, row] => Ok(ImageDims {
            layer: 0,
            col: col
                .parse()
                .map_err(|_| invalid_slide(path, format!("invalid VMS col in {key}")))?,
            row: row
                .parse()
                .map_err(|_| invalid_slide(path, format!("invalid VMS row in {key}")))?,
        }),
        [layer, col, row] => Ok(ImageDims {
            layer: layer
                .parse()
                .map_err(|_| invalid_slide(path, format!("invalid VMS layer in {key}")))?,
            col: col
                .parse()
                .map_err(|_| invalid_slide(path, format!("invalid VMS col in {key}")))?,
            row: row
                .parse()
                .map_err(|_| invalid_slide(path, format!("invalid VMS row in {key}")))?,
        }),
        _ => Err(invalid_slide(
            path,
            format!("unknown VMS image coordinate arity in {key}"),
        )),
    }
}

pub(super) fn parse_vms_opt_offsets(
    opt_path: Option<&Path>,
    image_paths: &[PathBuf],
) -> Result<Vec<Vec<Option<u64>>>, WsiError> {
    let Some(opt_path) = opt_path.filter(|path| path.is_file()) else {
        return Ok(vec![Vec::new(); image_paths.len()]);
    };

    let mut file = File::open(opt_path).map_err(|source| WsiError::IoWithPath {
        source: Arc::new(source),
        path: opt_path.to_path_buf(),
    })?;
    let mut per_image = Vec::with_capacity(image_paths.len());
    for image_path in image_paths {
        let geometry = jpeg_geometry_from_file(image_path)?;
        let tiles_down = geometry.height / geometry.tile_height;
        let mut row_starts = Vec::with_capacity(tiles_down as usize);
        let mut block = [0u8; 40];
        for _ in 0..tiles_down {
            match file.read_exact(&mut block) {
                Ok(()) => {
                    let offset = u64::from_le_bytes(block[..8].try_into().unwrap());
                    row_starts.push((offset > 0).then_some(offset));
                }
                Err(_) => {
                    return Ok(vec![Vec::new(); image_paths.len()]);
                }
            }
        }
        per_image.push(row_starts);
    }
    Ok(per_image)
}

fn jpeg_geometry_from_file(path: &Path) -> Result<JpegTileGeometry, WsiError> {
    Ok(read_vms_jpeg_header(path)?.geometry)
}
