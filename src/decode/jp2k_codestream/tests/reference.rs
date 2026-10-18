//! Independent SIZ/COD metadata oracle for the tracked codestream fixtures.
//! Codec acceptance and WSI sample limits are exercised through the real adapter.

#[derive(Debug)]
pub(super) struct FixtureMetadata {
    pub(super) image_width: u32,
    pub(super) image_height: u32,
    pub(super) multiple_component_transform: bool,
    pub(super) components: Vec<(u8, bool, u8, u8)>,
}

pub(super) fn fixture_metadata(bytes: &[u8]) -> FixtureMetadata {
    assert_eq!(&bytes[..2], &[0xff, 0x4f], "fixture SOC");
    let mut offset = 2;
    let mut dimensions = None;
    let mut components = None;
    let mut transform = None;
    while offset + 4 <= bytes.len() {
        let marker = u16::from_be_bytes(bytes[offset..offset + 2].try_into().unwrap());
        if marker == 0xff90 || marker == 0xff93 {
            break;
        }
        let len = usize::from(u16::from_be_bytes(
            bytes[offset + 2..offset + 4].try_into().unwrap(),
        ));
        assert!(len >= 2, "fixture marker length");
        let payload = &bytes[offset + 4..offset + 2 + len];
        match marker {
            0xff51 => {
                let value =
                    |start| u32::from_be_bytes(payload[start..start + 4].try_into().unwrap());
                dimensions = Some((value(2) - value(10), value(6) - value(14)));
                let count = usize::from(u16::from_be_bytes(payload[34..36].try_into().unwrap()));
                components = Some(
                    payload[36..36 + 3 * count]
                        .as_chunks::<3>()
                        .0
                        .iter()
                        .map(|component| {
                            (
                                (component[0] & 0x7f) + 1,
                                component[0] & 0x80 != 0,
                                component[1],
                                component[2],
                            )
                        })
                        .collect(),
                );
            }
            0xff52 => transform = Some(payload[4] != 0),
            _ => {}
        }
        offset += 2 + len;
    }
    let (image_width, image_height) = dimensions.expect("fixture SIZ");
    FixtureMetadata {
        image_width,
        image_height,
        multiple_component_transform: transform.expect("fixture COD"),
        components: components.expect("fixture components"),
    }
}
