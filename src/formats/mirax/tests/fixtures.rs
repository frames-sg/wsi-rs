use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};

use image::{DynamicImage, ImageFormat, RgbImage};
use tempfile::TempDir;

const SLIDE_ID: &str = "SYNTHETIC";
const IMAGE_SIDE: u32 = 16;

#[derive(Clone, Copy)]
struct DataRecord {
    offset: i32,
    len: i32,
    file: i32,
}

pub(super) struct MiraxFixture {
    _temp: TempDir,
    pub(super) path: PathBuf,
    pub(super) slide_dir: PathBuf,
    pub(super) slidedat_path: PathBuf,
    pub(super) index_path: PathBuf,
    pub(super) data_path: PathBuf,
}

impl MiraxFixture {
    pub(super) fn complete() -> Self {
        let temp = tempfile::tempdir().expect("temporary MIRAX fixture directory");
        let path = temp.path().join("synthetic.mrxs");
        let slide_dir = temp.path().join("synthetic");
        let slidedat_path = slide_dir.join("Slidedat.ini");
        let index_path = slide_dir.join("Index.dat");
        let data_path = slide_dir.join("Data0000.dat");
        fs::create_dir(&slide_dir).expect("create MIRAX companion directory");
        fs::write(&path, b"synthetic MIRAX entry").expect("write MIRAX entry");

        let mut data = Vec::new();
        let positions = append_record(&mut data, &position_buffer(false));

        let mut hierarchy = Vec::new();
        let mut level0 = Vec::new();
        for y in 0..4 {
            for x in 0..4 {
                let image_index = y * 4 + x;
                let jpeg = encode_jpeg(IMAGE_SIDE, IMAGE_SIDE, 5 + image_index as u8);
                level0.push((image_index, append_record(&mut data, &jpeg)));
            }
        }
        hierarchy.push(level0);

        let mut level1 = Vec::new();
        for (x, y, bias) in [(0, 0, 60), (2, 0, 80), (0, 2, 100), (2, 2, 120)] {
            let png = encode_raster(ImageFormat::Png, IMAGE_SIDE, IMAGE_SIDE, bias);
            level1.push((y * 4 + x, append_record(&mut data, &png)));
        }
        hierarchy.push(level1);

        let bmp = encode_raster(ImageFormat::Bmp, IMAGE_SIDE, IMAGE_SIDE, 180);
        hierarchy.push(vec![(0, append_record(&mut data, &bmp))]);

        let macro_record = append_record(&mut data, &encode_jpeg(12, 8, 200));
        let label_record = append_record(&mut data, &encode_jpeg(10, 6, 210));
        let thumbnail_record = append_record(&mut data, &encode_jpeg(8, 4, 220));
        fs::write(&data_path, data).expect("write MIRAX data file");

        let nonhier = [positions, macro_record, label_record, thumbnail_record];
        fs::write(&index_path, build_index(&hierarchy, &nonhier)).expect("write MIRAX index");

        let fixture = Self {
            _temp: temp,
            path,
            slide_dir,
            slidedat_path,
            index_path,
            data_path,
        };
        fixture.write_slidedat(&fixture.complete_slidedat());
        fixture
    }

    pub(super) fn complete_slidedat(&self) -> String {
        format!(
            "[GENERAL]\n\
             SLIDE_ID={SLIDE_ID}\n\
             IMAGENUMBER_X=4\n\
             IMAGENUMBER_Y=4\n\
             OBJECTIVE_MAGNIFICATION=20\n\
             CameraImageDivisionsPerSide=1\n\
             [HIERARCHICAL]\n\
             INDEXFILE=Index.dat\n\
             HIER_COUNT=1\n\
             NONHIER_COUNT=2\n\
             HIER_0_NAME=Slide zoom level\n\
             HIER_0_COUNT=3\n\
             HIER_0_VAL_0_SECTION=LEVEL_0\n\
             HIER_0_VAL_1_SECTION=LEVEL_1\n\
             HIER_0_VAL_2_SECTION=LEVEL_2\n\
             NONHIER_0_NAME=VIMSLIDE_POSITION_BUFFER\n\
             NONHIER_0_COUNT=1\n\
             NONHIER_0_VAL_0=default\n\
             NONHIER_0_VAL_0_SECTION=POSITIONS\n\
             NONHIER_1_NAME=Scan data layer\n\
             NONHIER_1_COUNT=3\n\
             NONHIER_1_VAL_0=ScanDataLayer_SlideThumbnail\n\
             NONHIER_1_VAL_0_SECTION=MACRO\n\
             NONHIER_1_VAL_1=ScanDataLayer_SlideBarcode\n\
             NONHIER_1_VAL_1_SECTION=LABEL\n\
             NONHIER_1_VAL_2=ScanDataLayer_SlidePreview\n\
             NONHIER_1_VAL_2_SECTION=THUMBNAIL\n\
             [DATAFILE]\n\
             FILE_COUNT=1\n\
             FILE_0=Data0000.dat\n\
             [LEVEL_0]\n\
             IMAGE_FILL_COLOR_BGR=1122867\n\
             MICROMETER_PER_PIXEL_X=0.25\n\
             MICROMETER_PER_PIXEL_Y=0.5\n\
             DIGITIZER_WIDTH=16\n\
             DIGITIZER_HEIGHT=16\n\
             OVERLAP_X=0\n\
             OVERLAP_Y=0\n\
             IMAGE_CONCAT_FACTOR=0\n\
             IMAGE_FORMAT=JPEG\n\
             [LEVEL_1]\n\
             IMAGE_FILL_COLOR_BGR=1122867\n\
             MICROMETER_PER_PIXEL_X=0.5\n\
             MICROMETER_PER_PIXEL_Y=1\n\
             DIGITIZER_WIDTH=16\n\
             DIGITIZER_HEIGHT=16\n\
             OVERLAP_X=0\n\
             OVERLAP_Y=0\n\
             IMAGE_CONCAT_FACTOR=1\n\
             IMAGE_FORMAT=PNG\n\
             [LEVEL_2]\n\
             IMAGE_FILL_COLOR_BGR=1122867\n\
             MICROMETER_PER_PIXEL_X=1\n\
             MICROMETER_PER_PIXEL_Y=2\n\
             DIGITIZER_WIDTH=16\n\
             DIGITIZER_HEIGHT=16\n\
             OVERLAP_X=0\n\
             OVERLAP_Y=0\n\
             IMAGE_CONCAT_FACTOR=1\n\
             IMAGE_FORMAT=BMP24\n\
             [POSITIONS]\n\
             [MACRO]\n\
             THUMBNAIL_IMAGE_TYPE=JPEG\n\
             [LABEL]\n\
             BARCODE_IMAGE_TYPE=JPEG\n\
             [THUMBNAIL]\n\
             PREVIEW_IMAGE_TYPE=JPEG\n"
        )
    }

    pub(super) fn write_slidedat(&self, contents: &str) {
        fs::write(&self.slidedat_path, contents).expect("write MIRAX Slidedat.ini");
    }

    pub(super) fn read_index(&self) -> Vec<u8> {
        fs::read(&self.index_path).expect("read synthetic MIRAX index")
    }

    pub(super) fn write_index(&self, bytes: &[u8]) {
        fs::write(&self.index_path, bytes).expect("write synthetic MIRAX index")
    }
}

pub(super) fn encode_jpeg(width: u32, height: u32, bias: u8) -> Vec<u8> {
    j2k_jpeg::encode_jpeg_baseline(
        j2k_jpeg::JpegSamples::Rgb8 {
            data: &patterned_pixels(width, height, bias),
            width,
            height,
        },
        j2k_jpeg::JpegEncodeOptions {
            quality: 95,
            subsampling: j2k_jpeg::JpegSubsampling::Ybr444,
            restart_interval: None,
            backend: j2k_jpeg::JpegBackend::Cpu,
        },
    )
    .expect("encode synthetic MIRAX JPEG")
    .data
}

pub(super) fn patterned_pixels(width: u32, height: u32, bias: u8) -> Vec<u8> {
    let mut pixels = vec![0; width as usize * height as usize * 3];
    for y in 0..height {
        for x in 0..width {
            let offset = (y as usize * width as usize + x as usize) * 3;
            pixels[offset] = bias.wrapping_add(x as u8);
            pixels[offset + 1] = bias.wrapping_add(y as u8);
            pixels[offset + 2] = bias.wrapping_add(x.wrapping_add(y) as u8);
        }
    }
    pixels
}

fn encode_raster(format: ImageFormat, width: u32, height: u32, bias: u8) -> Vec<u8> {
    let rgb = RgbImage::from_raw(width, height, patterned_pixels(width, height, bias))
        .expect("valid synthetic MIRAX raster dimensions");
    let mut output = Cursor::new(Vec::new());
    DynamicImage::ImageRgb8(rgb)
        .write_to(&mut output, format)
        .expect("encode synthetic MIRAX raster");
    output.into_inner()
}

fn position_buffer(compressed_layout: bool) -> Vec<u8> {
    let mut positions = Vec::new();
    for y in 0..4i32 {
        for x in 0..4i32 {
            positions.push(u8::from(compressed_layout));
            let (position_x, position_y) = if x == 3 && y == 3 {
                (0, 0)
            } else {
                (
                    x * IMAGE_SIDE as i32 + i32::from(x > 0) * 2,
                    y * IMAGE_SIDE as i32,
                )
            };
            positions.extend_from_slice(&position_x.to_le_bytes());
            positions.extend_from_slice(&position_y.to_le_bytes());
        }
    }
    positions
}

fn append_record(data: &mut Vec<u8>, payload: &[u8]) -> DataRecord {
    let offset = i32::try_from(data.len()).expect("small synthetic MIRAX data offset");
    data.extend_from_slice(payload);
    DataRecord {
        offset,
        len: i32::try_from(payload.len()).expect("small synthetic MIRAX data record"),
        file: 0,
    }
}

struct IndexBytes {
    bytes: Vec<u8>,
}

impl IndexBytes {
    fn reserve(&mut self, len: usize) -> usize {
        let offset = self.bytes.len();
        self.bytes.resize(offset + len, 0);
        offset
    }

    fn set_i32(&mut self, offset: usize, value: i32) {
        self.bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn set_u32(&mut self, offset: usize, value: usize) {
        self.set_i32(
            offset,
            i32::try_from(value).expect("small synthetic MIRAX index pointer"),
        );
    }
}

fn build_index(hierarchy: &[Vec<(i32, DataRecord)>], nonhier: &[DataRecord]) -> Vec<u8> {
    let mut index = IndexBytes {
        bytes: format!("01.02{SLIDE_ID}").into_bytes(),
    };
    let hierarchy_root = index.reserve(4);
    let nonhier_root = index.reserve(4);
    let hierarchy_table = index.reserve(hierarchy.len() * 4);
    let nonhier_table = index.reserve(nonhier.len() * 4);
    index.set_u32(hierarchy_root, hierarchy_table);
    index.set_u32(nonhier_root, nonhier_table);

    for (level, records) in hierarchy.iter().enumerate() {
        let head = index.reserve(8);
        let page = index.reserve(8 + records.len() * 16);
        index.set_u32(hierarchy_table + level * 4, head);
        index.set_i32(head, 0);
        index.set_u32(head + 4, page);
        index.set_i32(
            page,
            i32::try_from(records.len()).expect("small synthetic MIRAX page"),
        );
        index.set_i32(page + 4, 0);
        for (record_index, (image_index, record)) in records.iter().enumerate() {
            let offset = page + 8 + record_index * 16;
            index.set_i32(offset, *image_index);
            index.set_i32(offset + 4, record.offset);
            index.set_i32(offset + 8, record.len);
            index.set_i32(offset + 12, record.file);
        }
    }

    for (record_index, record) in nonhier.iter().enumerate() {
        let head = index.reserve(8);
        let page = index.reserve(28);
        index.set_u32(nonhier_table + record_index * 4, head);
        index.set_i32(head, 0);
        index.set_u32(head + 4, page);
        index.set_i32(page, 1);
        index.set_i32(page + 4, 0);
        index.set_i32(page + 8, 0);
        index.set_i32(page + 12, 0);
        index.set_i32(page + 16, record.offset);
        index.set_i32(page + 20, record.len);
        index.set_i32(page + 24, record.file);
    }

    index.bytes
}

pub(super) fn write_bytes(path: &Path, bytes: &[u8]) {
    fs::write(path, bytes).expect("write synthetic MIRAX test bytes");
}
