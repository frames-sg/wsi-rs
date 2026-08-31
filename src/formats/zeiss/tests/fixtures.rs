use std::io::Write;

const FILE_HEADER_BYTES: usize = 32 + 512;
const DIRECTORY_FIXED_BYTES: usize = 128;
const METADATA_FIXED_BYTES: usize = 256;
const ATTACHMENT_DIRECTORY_FIXED_BYTES: usize = 256;
const SUBBLOCK_FIXED_BYTES: usize = 256;
const ATTACHMENT_FIXED_BYTES: usize = 256;

#[derive(Clone)]
pub(super) struct SubblockSpec {
    pub(super) x: i32,
    pub(super) y: i32,
    pub(super) width: i32,
    pub(super) height: i32,
    pub(super) stored_width: u32,
    pub(super) stored_height: u32,
    pub(super) scene: Option<i32>,
    pub(super) m_index: Option<i32>,
    pub(super) pixel_type: i32,
    pub(super) compression: i32,
    pub(super) pyramid_type: u8,
    pub(super) data: Vec<u8>,
}

impl SubblockSpec {
    pub(super) fn bgr24(x: i32, y: i32, width: u32, height: u32, data: Vec<u8>) -> Self {
        Self {
            x,
            y,
            width: width as i32,
            height: height as i32,
            stored_width: width,
            stored_height: height,
            scene: Some(0),
            m_index: None,
            pixel_type: 3,
            compression: 0,
            pyramid_type: 0,
            data,
        }
    }

    pub(super) fn downsampled(mut self, stored_width: u32, stored_height: u32) -> Self {
        self.stored_width = stored_width;
        self.stored_height = stored_height;
        self.pyramid_type = 1;
        self
    }
}

#[derive(Clone)]
pub(super) struct AttachmentSpec {
    pub(super) name: &'static str,
    pub(super) file_type: &'static str,
    pub(super) data: Vec<u8>,
}

pub(super) fn metadata_xml(width: u32, height: u32) -> String {
    format!(
        r#"<ImageDocument><Metadata><Information><Image><PixelType>Bgr24</PixelType><SizeX>{width}</SizeX><SizeY>{height}</SizeY><SizeS>1</SizeS><Dimensions><Channels><Channel Id="Channel:0" Name="Brightfield"><PixelType>Bgr24</PixelType><Color>#FF112233</Color></Channel></Channels></Dimensions></Image><Document><UserName>Fixture User</UserName><CreationDate>2026-08-15</CreationDate></Document><Application><Name>Fixture Writer</Name><Version>1.0</Version></Application></Information><Scaling><Items><Distance Id="X"><Value>0.00000025</Value><DefaultUnitFormat>m</DefaultUnitFormat></Distance><Distance Id="Y"><Value>0.0000005</Value><DefaultUnitFormat>m</DefaultUnitFormat></Distance></Items></Scaling><ObjectiveRef Id="Objective:0"/><Objective Id="Objective:0"><NominalMagnification>40</NominalMagnification></Objective></Metadata></ImageDocument>"#
    )
}

pub(super) fn main_fixture() -> tempfile::NamedTempFile {
    let embedded = build_czi_bytes(
        &[SubblockSpec::bgr24(0, 0, 1, 1, vec![7, 8, 9])],
        &[],
        &metadata_xml(1, 1),
    );
    let jpeg = jpeg_rgb(2, 1, &[255, 0, 0, 0, 255, 0]);
    let attachments = [
        AttachmentSpec {
            name: "Label",
            file_type: "JPG",
            data: jpeg,
        },
        AttachmentSpec {
            name: "SlidePreview",
            file_type: "CZI",
            data: embedded,
        },
        AttachmentSpec {
            name: "Ignored",
            file_type: "TXT",
            data: b"ignored".to_vec(),
        },
    ];
    let subblocks = [
        SubblockSpec::bgr24(10, 20, 2, 2, vec![3, 2, 1, 6, 5, 4, 9, 8, 7, 12, 11, 10]),
        SubblockSpec::bgr24(
            12,
            20,
            2,
            2,
            vec![15, 14, 13, 18, 17, 16, 21, 20, 19, 24, 23, 22],
        ),
        SubblockSpec::bgr24(10, 20, 4, 2, vec![30, 29, 28, 33, 32, 31]).downsampled(2, 1),
    ];
    write_fixture(&subblocks, &attachments, &metadata_xml(4, 2))
}

pub(super) fn write_fixture(
    subblocks: &[SubblockSpec],
    attachments: &[AttachmentSpec],
    xml: &str,
) -> tempfile::NamedTempFile {
    let bytes = build_czi_bytes(subblocks, attachments, xml);
    let mut file = tempfile::Builder::new()
        .prefix("wsi-rs-czi-")
        .suffix(".czi")
        .tempfile()
        .expect("create synthetic CZI");
    file.write_all(&bytes).expect("write synthetic CZI");
    file.flush().expect("flush synthetic CZI");
    file
}

pub(super) fn build_czi_bytes(
    subblocks: &[SubblockSpec],
    attachments: &[AttachmentSpec],
    xml: &str,
) -> Vec<u8> {
    let entry_sizes: Vec<_> = subblocks
        .iter()
        .map(|spec| 32 + dimensions(spec).len() * 20)
        .collect();
    let directory_offset = FILE_HEADER_BYTES;
    let directory_bytes = 32 + DIRECTORY_FIXED_BYTES + entry_sizes.iter().sum::<usize>();
    let metadata_offset = directory_offset + directory_bytes;
    let metadata_bytes = 32 + METADATA_FIXED_BYTES + xml.len();
    let attachment_directory_offset = if attachments.is_empty() {
        0
    } else {
        metadata_offset + metadata_bytes
    };
    let attachment_directory_bytes = if attachments.is_empty() {
        0
    } else {
        32 + ATTACHMENT_DIRECTORY_FIXED_BYTES + attachments.len() * 128
    };

    let mut cursor = metadata_offset + metadata_bytes + attachment_directory_bytes;
    let mut subblock_offsets = Vec::with_capacity(subblocks.len());
    for spec in subblocks {
        subblock_offsets.push(cursor);
        cursor += 32 + SUBBLOCK_FIXED_BYTES + spec.data.len();
    }
    let mut attachment_offsets = Vec::with_capacity(attachments.len());
    for attachment in attachments {
        attachment_offsets.push(cursor);
        cursor += 32 + ATTACHMENT_FIXED_BYTES + attachment.data.len();
    }

    let mut bytes = vec![0; cursor];
    write_segment_header(&mut bytes, 0, b"ZISRAWFILE\0\0\0\0\0\0", 512);
    write_i32(&mut bytes, 32, 1);
    write_i32(&mut bytes, 36, 0);
    for (index, byte) in bytes[48..80].iter_mut().enumerate() {
        *byte = index as u8;
    }
    write_u64(&mut bytes, 32 + 52, directory_offset as u64);
    write_u64(&mut bytes, 32 + 60, metadata_offset as u64);
    write_u64(&mut bytes, 32 + 72, attachment_directory_offset as u64);

    let directory_used = DIRECTORY_FIXED_BYTES + entry_sizes.iter().sum::<usize>();
    write_segment_header(
        &mut bytes,
        directory_offset,
        b"ZISRAWDIRECTORY\0",
        directory_used,
    );
    write_i32(&mut bytes, directory_offset + 32, subblocks.len() as i32);
    let mut entry_cursor = directory_offset + 32 + DIRECTORY_FIXED_BYTES;
    for ((spec, &file_position), &entry_size) in
        subblocks.iter().zip(&subblock_offsets).zip(&entry_sizes)
    {
        write_directory_entry(
            &mut bytes[entry_cursor..entry_cursor + entry_size],
            spec,
            file_position,
        );
        entry_cursor += entry_size;
    }

    write_segment_header(
        &mut bytes,
        metadata_offset,
        b"ZISRAWMETADATA\0\0",
        METADATA_FIXED_BYTES + xml.len(),
    );
    write_u32(&mut bytes, metadata_offset + 32, xml.len() as u32);
    let xml_offset = metadata_offset + 32 + METADATA_FIXED_BYTES;
    bytes[xml_offset..xml_offset + xml.len()].copy_from_slice(xml.as_bytes());

    if let Some(directory_offset) =
        (attachment_directory_offset != 0).then_some(attachment_directory_offset)
    {
        write_segment_header(
            &mut bytes,
            directory_offset,
            b"ZISRAWATTDIR\0\0\0\0",
            ATTACHMENT_DIRECTORY_FIXED_BYTES + attachments.len() * 128,
        );
        write_i32(&mut bytes, directory_offset + 32, attachments.len() as i32);
        for (index, (attachment, &file_position)) in
            attachments.iter().zip(&attachment_offsets).enumerate()
        {
            let start = directory_offset + 32 + ATTACHMENT_DIRECTORY_FIXED_BYTES + index * 128;
            let entry = &mut bytes[start..start + 128];
            entry[..2].copy_from_slice(b"A1");
            write_u64(entry, 12, file_position as u64);
            entry[24..40].copy_from_slice(&[index as u8; 16]);
            write_fixed(&mut entry[40..48], attachment.file_type);
            write_fixed(&mut entry[48..128], attachment.name);
        }
    }

    for ((spec, &file_position), &entry_size) in
        subblocks.iter().zip(&subblock_offsets).zip(&entry_sizes)
    {
        write_segment_header(
            &mut bytes,
            file_position,
            b"ZISRAWSUBBLOCK\0\0",
            SUBBLOCK_FIXED_BYTES + spec.data.len(),
        );
        write_u64(&mut bytes, file_position + 32 + 8, spec.data.len() as u64);
        let entry_start = file_position + 32 + 16;
        write_directory_entry(
            &mut bytes[entry_start..entry_start + entry_size],
            spec,
            file_position,
        );
        let data_start = file_position + 32 + SUBBLOCK_FIXED_BYTES;
        bytes[data_start..data_start + spec.data.len()].copy_from_slice(&spec.data);
    }

    for (attachment, &file_position) in attachments.iter().zip(&attachment_offsets) {
        write_segment_header(
            &mut bytes,
            file_position,
            b"ZISRAWATTACH\0\0\0\0",
            ATTACHMENT_FIXED_BYTES + attachment.data.len(),
        );
        write_u64(&mut bytes, file_position + 32, attachment.data.len() as u64);
        let data_start = file_position + 32 + ATTACHMENT_FIXED_BYTES;
        bytes[data_start..data_start + attachment.data.len()].copy_from_slice(&attachment.data);
    }
    bytes
}

fn dimensions(spec: &SubblockSpec) -> Vec<([u8; 4], i32, i32, i32)> {
    let mut dimensions = vec![
        (*b"X\0\0\0", spec.x, spec.width, spec.stored_width as i32),
        (*b"Y\0\0\0", spec.y, spec.height, spec.stored_height as i32),
    ];
    if let Some(scene) = spec.scene {
        dimensions.push((*b"S\0\0\0", scene, 1, 1));
    }
    if let Some(m_index) = spec.m_index {
        dimensions.push((*b"M\0\0\0", m_index, 1, 1));
    }
    dimensions
}

fn write_directory_entry(entry: &mut [u8], spec: &SubblockSpec, file_position: usize) {
    entry[..2].copy_from_slice(b"DV");
    write_i32(entry, 2, spec.pixel_type);
    write_u64(entry, 6, file_position as u64);
    write_i32(entry, 18, spec.compression);
    entry[22] = spec.pyramid_type;
    let dimensions = dimensions(spec);
    write_i32(entry, 28, dimensions.len() as i32);
    for (index, (code, start, size, stored)) in dimensions.into_iter().enumerate() {
        let offset = 32 + index * 20;
        entry[offset..offset + 4].copy_from_slice(&code);
        write_i32(entry, offset + 4, start);
        write_i32(entry, offset + 8, size);
        write_i32(entry, offset + 16, stored);
    }
}

fn write_segment_header(bytes: &mut [u8], offset: usize, magic: &[u8; 16], used: usize) {
    bytes[offset..offset + 16].copy_from_slice(magic);
    write_u64(bytes, offset + 16, used as u64);
    write_u64(bytes, offset + 24, used as u64);
}

fn write_fixed(destination: &mut [u8], value: &str) {
    let len = destination.len().min(value.len());
    destination[..len].copy_from_slice(&value.as_bytes()[..len]);
}

pub(super) fn write_u32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

pub(super) fn write_i32(bytes: &mut [u8], offset: usize, value: i32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

pub(super) fn write_u64(bytes: &mut [u8], offset: usize, value: u64) {
    bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

pub(super) fn jpeg_rgb(width: u16, height: u16, rgb: &[u8]) -> Vec<u8> {
    let mut encoded = Vec::new();
    jpeg_encoder::Encoder::new(&mut encoded, 95)
        .encode(rgb, width, height, jpeg_encoder::ColorType::Rgb)
        .expect("encode deterministic associated JPEG");
    encoded
}
