use crate::error::WsiError;
use quick_xml::events::Event;
use quick_xml::{Reader, XmlVersion};
use std::collections::HashMap;

const MAX_XML_INPUT_BYTES: usize = 32 * 1024 * 1024;
const MAX_XML_DEPTH: u32 = 128;
const MAX_XML_NODES: usize = 100_000;
const MAX_XML_ATTRIBUTES: usize = 500_000;
const MAX_XML_ATTRIBUTES_PER_NODE: usize = 256;

#[derive(Default)]
struct XmlBudget {
    nodes: usize,
    attributes: usize,
}

impl XmlBudget {
    fn add_node(&mut self) -> Result<(), WsiError> {
        // The parser rejects the next node at 100,001, far below usize::MAX on
        // every supported target, so a separate arithmetic-overflow branch is
        // unreachable.
        self.nodes += 1;
        if self.nodes > MAX_XML_NODES {
            return Err(WsiError::Xml(format!(
                "XML node count exceeds maximum of {MAX_XML_NODES}"
            )));
        }
        Ok(())
    }

    fn add_attributes(&mut self, count: usize) -> Result<(), WsiError> {
        if count > MAX_XML_ATTRIBUTES_PER_NODE {
            return Err(WsiError::Xml(format!(
                "XML element attribute count exceeds maximum of {MAX_XML_ATTRIBUTES_PER_NODE}"
            )));
        }
        // Per-node and aggregate budgets bound this addition to at most 500,256.
        self.attributes += count;
        if self.attributes > MAX_XML_ATTRIBUTES {
            return Err(WsiError::Xml(format!(
                "XML attribute count exceeds maximum of {MAX_XML_ATTRIBUTES}"
            )));
        }
        Ok(())
    }
}

/// A simple tree representation of an XML document.
///
/// # Security note
///
/// The parser ignores document type events, resolves only predefined entities,
/// and enforces input, depth, node, and attribute budgets before constructing
/// the tree.
#[derive(Debug, Clone)]
pub(crate) struct XmlNode {
    pub tag: String,
    pub attributes: HashMap<String, String>,
    pub text: Option<String>,
    pub children: Vec<XmlNode>,
}

impl XmlNode {
    /// Find the first direct child with the given tag name.
    pub(crate) fn find(&self, tag: &str) -> Option<&XmlNode> {
        self.children.iter().find(|c| c.tag == tag)
    }

    /// Find all direct children with the given tag name.
    pub(crate) fn find_all(&self, tag: &str) -> Vec<&XmlNode> {
        self.children.iter().filter(|c| c.tag == tag).collect()
    }

    /// Get the value of an attribute by name.
    pub(crate) fn attr(&self, name: &str) -> Option<&str> {
        self.attributes.get(name).map(|s| s.as_str())
    }
}

/// Parse an XML string into a tree of `XmlNode`.
pub(crate) fn parse_xml(xml: &str) -> Result<XmlNode, WsiError> {
    if xml.len() > MAX_XML_INPUT_BYTES {
        return Err(WsiError::Xml(format!(
            "XML input exceeds maximum of {MAX_XML_INPUT_BYTES} bytes"
        )));
    }
    let mut reader = Reader::from_str(xml);
    let mut buf = Vec::new();
    let mut budget = XmlBudget::default();

    // Find the root element
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                let node = parse_node_recursive(&e, &mut reader, 0, &mut budget)?;
                return Ok(node);
            }
            Ok(Event::Empty(e)) => {
                return make_empty_node(&e, &mut budget);
            }
            Ok(Event::Eof) => {
                return Err(WsiError::Xml("empty document".into()));
            }
            Err(e) => {
                return Err(WsiError::Xml(e.to_string()));
            }
            _ => {}
        }
        buf.clear();
    }
}

fn make_empty_node(
    e: &quick_xml::events::BytesStart,
    budget: &mut XmlBudget,
) -> Result<XmlNode, WsiError> {
    budget.add_node()?;
    let tag = String::from_utf8_lossy(e.name().as_ref()).into_owned();
    let mut attributes = HashMap::new();
    let mut attribute_count = 0usize;
    for attr in e.attributes() {
        attribute_count += 1;
        if attribute_count > MAX_XML_ATTRIBUTES_PER_NODE {
            return Err(WsiError::Xml(format!(
                "XML element attribute count exceeds maximum of {MAX_XML_ATTRIBUTES_PER_NODE}"
            )));
        }
        budget.add_attributes(1)?;
        let attr = attr.map_err(|err| WsiError::Xml(err.to_string()))?;
        let key = String::from_utf8_lossy(attr.key.as_ref()).into_owned();
        let val = attr
            .normalized_value(XmlVersion::Implicit1_0)
            .map_err(|err| WsiError::Xml(err.to_string()))?
            .into_owned();
        attributes.insert(key, val);
    }
    Ok(XmlNode {
        tag,
        attributes,
        text: None,
        children: Vec::new(),
    })
}

fn parse_node_recursive(
    start: &quick_xml::events::BytesStart,
    reader: &mut Reader<&[u8]>,
    depth: u32,
    budget: &mut XmlBudget,
) -> Result<XmlNode, WsiError> {
    if depth > MAX_XML_DEPTH {
        return Err(WsiError::Xml(format!(
            "XML nesting depth exceeds maximum of {}",
            MAX_XML_DEPTH
        )));
    }
    budget.add_node()?;
    let tag = String::from_utf8_lossy(start.name().as_ref()).into_owned();
    let mut attributes = HashMap::new();
    let mut attribute_count = 0usize;
    for attr in start.attributes() {
        attribute_count += 1;
        if attribute_count > MAX_XML_ATTRIBUTES_PER_NODE {
            return Err(WsiError::Xml(format!(
                "XML element attribute count exceeds maximum of {MAX_XML_ATTRIBUTES_PER_NODE}"
            )));
        }
        budget.add_attributes(1)?;
        let attr = attr.map_err(|err| WsiError::Xml(err.to_string()))?;
        let key = String::from_utf8_lossy(attr.key.as_ref()).into_owned();
        let val = attr
            .normalized_value(XmlVersion::Implicit1_0)
            .map_err(|err| WsiError::Xml(err.to_string()))?
            .into_owned();
        attributes.insert(key, val);
    }
    let mut children = Vec::new();
    let mut text = None;
    let mut buf = Vec::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                let child = parse_node_recursive(&e, reader, depth + 1, budget)?;
                children.push(child);
            }
            Ok(Event::Empty(e)) => {
                children.push(make_empty_node(&e, budget)?);
            }
            Ok(Event::Text(e)) => {
                let t = e
                    .xml_content(XmlVersion::Implicit1_0)
                    .expect("Reader::from_str yields valid text encoding")
                    .into_owned();
                if !t.trim().is_empty() {
                    text = Some(t);
                }
            }
            Ok(Event::End(_)) => break,
            Ok(Event::Eof) => {
                return Err(WsiError::Xml(format!("unexpected EOF in <{}>", tag)));
            }
            Err(e) => {
                return Err(WsiError::Xml(e.to_string()));
            }
            _ => {}
        }
        buf.clear();
    }

    Ok(XmlNode {
        tag,
        attributes,
        text,
        children,
    })
}

#[cfg(test)]
mod tests;
