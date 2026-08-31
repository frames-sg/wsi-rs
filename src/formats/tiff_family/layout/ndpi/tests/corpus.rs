use super::super::*;

#[test]
fn opens_legacy_wrapped_offset_ndpi_when_corpus_is_available() {
    use crate::core::registry::Slide;

    let workspace_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..");
    let path = workspace_root.join("downloads/openslide-testdata/Hamamatsu/Hamamatsu-1.ndpi");
    if !path.exists() {
        return;
    }

    let slide = Slide::open(&path).expect("open legacy NDPI");
    assert_eq!(
        slide.dataset().scenes[0].series[0].levels[0].dimensions,
        (188160, 101376)
    );
    let tile = slide
        .read_tile(&TileRequest {
            scene: 0usize.into(),
            series: 0usize.into(),
            level: 0u32.into(),
            plane: PlaneSelection::default().into(),
            col: 0,
            row: 0,
        })
        .expect("read legacy NDPI tile");
    assert!(tile.width() > 0);
    assert!(tile.height() > 0);
}
