use super::model::{
    Dimension, DocumentInfo, ImageInfo, MetadataChannelInfo, MetadataSummary, ScalingInfo,
};
use super::pixel::PixelType;
use crate::decode::xml::{parse_xml, XmlNode};
use crate::error::WsiError;

pub(super) fn parse_metadata_xml(xml: &str) -> Result<MetadataSummary, WsiError> {
    if xml.trim().is_empty() {
        return Ok(MetadataSummary::default());
    }
    let root = parse_xml(xml)?;
    let metadata = if root.tag == "Metadata" {
        Some(&root)
    } else {
        root.find("Metadata")
    };
    let information = metadata.and_then(|node| node.find("Information"));
    let image_node = information.and_then(|node| node.find("Image"));
    let document_node = information.and_then(|node| node.find("Document"));
    let application_node = information.and_then(|node| node.find("Application"));
    let scaling_node = metadata.and_then(|node| node.find("Scaling"));

    Ok(MetadataSummary {
        document: parse_document(document_node, application_node),
        image: parse_image(image_node),
        scaling: parse_scaling(scaling_node),
        channels: parse_channels(image_node),
    })
}

fn parse_document(
    document_node: Option<&XmlNode>,
    application_node: Option<&XmlNode>,
) -> DocumentInfo {
    let mut document = DocumentInfo::default();
    if let Some(node) = document_node {
        document.name = child_text(node, "Name");
        document.title = child_text(node, "Title");
        document.comment = child_text(node, "Comment");
        document.author = child_text(node, "Author");
        document.user_name = child_text(node, "UserName");
        document.creation_date = child_text(node, "CreationDate");
        document.description = child_text(node, "Description");
    }
    if let Some(node) = application_node {
        document.application_name = child_text(node, "Name");
        document.application_version = child_text(node, "Version");
    }
    document
}

fn parse_image(image_node: Option<&XmlNode>) -> ImageInfo {
    let mut image = ImageInfo::default();
    let Some(node) = image_node else {
        return image;
    };
    image.pixel_type = child_text(node, "PixelType").and_then(|text| PixelType::from_name(&text));
    for child in &node.children {
        let Some(dimension_name) = child.tag.strip_prefix("Size") else {
            continue;
        };
        let Some(dimension) = Dimension::from_code(dimension_name) else {
            continue;
        };
        if let Some(value) = node_text(child).and_then(|text| text.parse::<usize>().ok()) {
            image.sizes.insert(dimension, value);
        }
    }
    image
}

fn parse_channels(image_node: Option<&XmlNode>) -> Vec<MetadataChannelInfo> {
    let Some(channels) = image_node
        .and_then(|node| node.find("Dimensions"))
        .and_then(|node| node.find("Channels"))
    else {
        return Vec::new();
    };
    channels
        .children
        .iter()
        .filter(|node| node.tag == "Channel")
        .enumerate()
        .map(|(index, channel)| MetadataChannelInfo {
            index,
            id: channel.attr("Id").map(str::to_owned),
            name: channel.attr("Name").map(str::to_owned),
            pixel_type: child_text(channel, "PixelType")
                .and_then(|text| PixelType::from_name(&text)),
            color: child_text(channel, "Color"),
        })
        .collect()
}

fn parse_scaling(scaling_node: Option<&XmlNode>) -> ScalingInfo {
    let Some(scaling_node) = scaling_node else {
        return ScalingInfo::default();
    };
    let items = scaling_node.find("Items").unwrap_or(scaling_node);
    let mut scaling = ScalingInfo::default();
    for distance in items.children.iter().filter(|node| node.tag == "Distance") {
        let value = child_text(distance, "Value").and_then(|text| text.parse::<f64>().ok());
        match distance
            .attr("Id")
            .map(str::trim)
            .map(str::to_ascii_uppercase)
            .as_deref()
        {
            Some("X") => scaling.x = value,
            Some("Y") => scaling.y = value,
            Some("Z") => scaling.z = value,
            _ => {}
        }
        if scaling.unit.is_none() {
            scaling.unit = child_text(distance, "DefaultUnitFormat")
                .or_else(|| child_text(distance, "Unit"))
                .or_else(|| child_text(distance, "UnitFormat"));
        }
    }
    scaling
}

fn child_text(node: &XmlNode, name: &str) -> Option<String> {
    node.find(name).and_then(node_text).map(str::to_owned)
}

fn node_text(node: &XmlNode) -> Option<&str> {
    node.text
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_the_metadata_used_by_the_zeiss_backend() {
        let xml = r##"<ImageDocument><Metadata><Information><Image><PixelType>Bgr24</PixelType><SizeX>12</SizeX><SizeY>7</SizeY><Dimensions><Channels><Channel Id="c0" Name="RGB"><PixelType>Bgr24</PixelType><Color>#FF112233</Color></Channel></Channels></Dimensions></Image><Document><UserName>Ada</UserName></Document><Application><Name>ZEN</Name><Version>4</Version></Application></Information><Scaling><Items><Distance Id="X"><Value>2.5e-7</Value><DefaultUnitFormat>m</DefaultUnitFormat></Distance></Items></Scaling></Metadata></ImageDocument>"##;
        let summary = parse_metadata_xml(xml).expect("parse metadata");
        assert_eq!(summary.image.pixel_type, Some(PixelType::Bgr24));
        assert_eq!(summary.image.sizes.get(&Dimension::X), Some(&12));
        assert_eq!(summary.document.user_name.as_deref(), Some("Ada"));
        assert_eq!(summary.document.application_name.as_deref(), Some("ZEN"));
        assert_eq!(summary.scaling.x, Some(2.5e-7));
        assert_eq!(summary.channels[0].name.as_deref(), Some("RGB"));
        assert_eq!(summary.channels[0].color.as_deref(), Some("#FF112233"));
    }
}
