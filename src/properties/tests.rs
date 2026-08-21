use super::*;

#[test]
fn insert_and_get() {
    let mut props = Properties::new();
    props.insert("openslide.vendor", "aperio");
    assert_eq!(props.vendor(), Some("aperio"));
    assert_eq!(props.get("missing"), None);
}

#[test]
fn mpp_parsing() {
    let mut props = Properties::new();
    props.insert("openslide.mpp-x", "0.2528");
    props.insert("openslide.mpp-y", "0.2528");
    let (x, y) = props.mpp().unwrap();
    assert!((x - 0.2528).abs() < 1e-6);
    assert!((y - 0.2528).abs() < 1e-6);
}

#[test]
fn background_color_default_white() {
    let props = Properties::new();
    assert_eq!(props.background_color(), [255, 255, 255]);
}

#[test]
fn background_color_hex() {
    let mut props = Properties::new();
    props.insert("openslide.background-color", "#FF0000");
    assert_eq!(props.background_color(), [255, 0, 0]);
}

#[test]
fn names_sorted() {
    let mut props = Properties::new();
    props.insert("z.last", "1");
    props.insert("a.first", "2");
    let names = props.names();
    assert_eq!(names, vec!["a.first", "z.last"]);
}
