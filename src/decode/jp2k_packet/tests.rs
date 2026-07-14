use super::*;
use crate::decode::jp2k_codestream::{
    Jp2kCodestreamInfo, Jp2kCodingStyleInfo, Jp2kComponentInfo, Jp2kQuantStep,
    Jp2kQuantizationInfo, Jp2kTilePartHeader, Jp2kWaveletTransform,
};

struct PacketHeaderWriter {
    bytes: Vec<u8>,
    buf: u32,
    bit_count: u32,
}

impl PacketHeaderWriter {
    fn new() -> Self {
        Self {
            bytes: Vec::new(),
            buf: 0,
            bit_count: 8,
        }
    }

    fn write_bit(&mut self, bit: bool) {
        if self.bit_count == 0 {
            self.byte_out();
        }
        self.bit_count -= 1;
        if bit {
            self.buf |= 1 << self.bit_count;
        }
    }

    fn write_bits(&mut self, value: u32, bit_count: u32) {
        for shift in (0..bit_count).rev() {
            self.write_bit(((value >> shift) & 1) != 0);
        }
    }

    fn write_num_passes(&mut self, passes: u32) {
        match passes {
            1 => self.write_bit(false),
            2 => self.write_bits(0b10, 2),
            3..=5 => self.write_bits(0b1100 | (passes - 3), 4),
            6..=36 => self.write_bits(0b111100000 | (passes - 6), 9),
            37..=164 => self.write_bits(0b1111111110000000 | (passes - 37), 16),
            _ => panic!("unsupported test pass count"),
        }
    }

    fn write_comma_code(&mut self, value: u32) {
        for _ in 0..value {
            self.write_bit(true);
        }
        self.write_bit(false);
    }

    fn finish(mut self) -> Vec<u8> {
        self.in_align();
        self.bytes
    }

    fn in_align(&mut self) {
        self.byte_out();
        if self.bit_count == 7 {
            self.byte_out();
        }
    }

    fn byte_out(&mut self) {
        self.buf = (self.buf << 8) & 0xFFFF;
        self.bit_count = if self.buf == 0xFF00 { 7 } else { 8 };
        self.bytes.push((self.buf >> 8) as u8);
    }
}

fn test_info(
    order: Jp2kProgressionOrder,
    layers: u16,
    width: u32,
    height: u32,
    decomposition_levels: u8,
) -> Jp2kCodestreamInfo {
    let mut steps = Vec::new();
    for _ in 0..(3 * decomposition_levels as usize + 1) {
        steps.push(Jp2kQuantStep {
            exponent: 8,
            mantissa: 0,
        });
    }
    Jp2kCodestreamInfo {
        image_origin_x: 0,
        image_origin_y: 0,
        image_width: width,
        image_height: height,
        tile_width: width,
        tile_height: height,
        tile_origin_x: 0,
        tile_origin_y: 0,
        tile_count_x: 1,
        tile_count_y: 1,
        components: vec![
            Jp2kComponentInfo {
                precision_bits: 8,
                is_signed: false,
                horizontal_sample_separation: 1,
                vertical_sample_separation: 1,
            };
            3
        ],
        coding_style: Jp2kCodingStyleInfo {
            progression_order: order,
            layers,
            multiple_component_transform: false,
            decomposition_levels,
            code_block_width_exponent: 4,
            code_block_height_exponent: 4,
            code_block_style: 0,
            transform: Jp2kWaveletTransform::Irreversible9x7,
            custom_precincts: false,
            sop_markers: false,
            eph_markers: false,
        },
        quantization: Jp2kQuantizationInfo {
            style: Jp2kQuantizationStyle::ScalarExpounded,
            guard_bits: 2,
            steps,
        },
        main_header_length: 0,
        tile_parts: vec![Jp2kTilePartInfo {
            header: Jp2kTilePartHeader {
                tile_index: 0,
                tile_part_length: 0,
                tile_part_index: 0,
                tile_part_count: 1,
            },
            data_offset: 0,
            data_length: 0,
        }],
        seen_markers: vec![],
    }
}

fn empty_packet(eph_markers: bool) -> Vec<u8> {
    let mut writer = PacketHeaderWriter::new();
    writer.write_bit(false);
    let mut packet = writer.finish();
    if eph_markers {
        packet.extend_from_slice(&MARKER_EPH);
    }
    packet
}

fn single_contribution_packet(eph_markers: bool, body: &[u8]) -> Vec<u8> {
    let mut writer = PacketHeaderWriter::new();
    writer.write_bit(true);
    writer.write_bit(true);
    writer.write_bit(true);
    writer.write_num_passes(1);
    writer.write_comma_code(0);
    writer.write_bits(body.len() as u32, 3);
    let mut packet = writer.finish();
    if eph_markers {
        packet.extend_from_slice(&MARKER_EPH);
    }
    packet.extend_from_slice(body);
    packet
}

#[test]
fn enumerate_lrcp_packets() {
    let packets = enumerate_packet_order(&test_info(Jp2kProgressionOrder::Lrcp, 2, 512, 256, 2));
    assert_eq!(packets.len(), 18);
    assert_eq!(
        packets[0],
        Jp2kPacketCoordinate {
            layer: 0,
            resolution: 0,
            component: 0,
            precinct: 0,
        }
    );
    assert_eq!(
        packets.last().copied().unwrap(),
        Jp2kPacketCoordinate {
            layer: 1,
            resolution: 2,
            component: 2,
            precinct: 0,
        }
    );
}

#[test]
fn enumerate_multiple_precincts_for_large_resolutions() {
    let packets =
        enumerate_packet_order(&test_info(Jp2kProgressionOrder::Rlcp, 1, 70000, 40000, 0));
    assert!(packets.iter().any(|packet| packet.precinct > 0));
}

#[test]
fn tile_part_packet_coordinates_split_round_robin() {
    let info = test_info(Jp2kProgressionOrder::Lrcp, 2, 512, 256, 2);
    let tile_part = Jp2kTilePartInfo {
        header: Jp2kTilePartHeader {
            tile_index: 0,
            tile_part_length: 0,
            tile_part_index: 1,
            tile_part_count: 2,
        },
        data_offset: 0,
        data_length: 0,
    };

    let packets = tile_part_packet_coordinates(&info, &tile_part);
    assert_eq!(packets.len(), 9);
    assert_eq!(
        packets[0],
        Jp2kPacketCoordinate {
            layer: 0,
            resolution: 0,
            component: 1,
            precinct: 0,
        }
    );
}

#[test]
fn parse_single_packet_header_and_body_ranges() {
    let info = test_info(Jp2kProgressionOrder::Lrcp, 1, 8, 8, 0);
    let mut codestream = Vec::new();
    codestream.extend_from_slice(&single_contribution_packet(
        false,
        &[0x11, 0x22, 0x33, 0x44],
    ));
    codestream.extend_from_slice(&empty_packet(false));
    codestream.extend_from_slice(&empty_packet(false));

    let tile_part = Jp2kTilePartInfo {
        header: Jp2kTilePartHeader {
            tile_index: 0,
            tile_part_length: 0,
            tile_part_index: 0,
            tile_part_count: 1,
        },
        data_offset: 0,
        data_length: codestream.len(),
    };

    let packets = parse_tile_part_packets(&codestream, &info, &tile_part).unwrap();
    assert_eq!(packets.len(), 3);
    let packet = &packets[0];
    assert_eq!(packet.coordinate.component, 0);
    assert_eq!(packet.header_offset, 0);
    assert_eq!(packet.body_length, 4);
    assert_eq!(packet.contributions.len(), 1);
    assert_eq!(packet.contributions[0].length, 4);
    assert_eq!(packet.contributions[0].body_offset, packet.body_offset);
    assert_eq!(packets[1].body_length, 0);
    assert_eq!(packets[2].body_length, 0);
}

#[test]
fn parse_empty_packet_with_eph_marker() {
    let mut info = test_info(Jp2kProgressionOrder::Lrcp, 1, 8, 8, 0);
    info.coding_style.eph_markers = true;
    let mut codestream = Vec::new();
    codestream.extend_from_slice(&empty_packet(true));
    codestream.extend_from_slice(&empty_packet(true));
    codestream.extend_from_slice(&empty_packet(true));

    let tile_part = Jp2kTilePartInfo {
        header: Jp2kTilePartHeader {
            tile_index: 0,
            tile_part_length: 0,
            tile_part_index: 0,
            tile_part_count: 1,
        },
        data_offset: 0,
        data_length: codestream.len(),
    };

    let packets = parse_tile_part_packets(&codestream, &info, &tile_part).unwrap();
    assert_eq!(packets[0].body_length, 0);
    assert_eq!(packets[0].header_length, empty_packet(true).len());
    assert_eq!(packets[1].body_length, 0);
    assert_eq!(packets[2].body_length, 0);
}
