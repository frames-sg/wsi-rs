use super::*;

#[test]
fn parse_xml_tree() {
    let xml = r#"<root version="1.0">
            <meta key="vendor">Aperio</meta>
            <levels>
                <level id="0" width="4096" height="2048"/>
                <level id="1" width="2048" height="1024"/>
            </levels>
        </root>"#;

    let root = parse_xml(xml).unwrap();
    assert_eq!(root.tag, "root");
    assert_eq!(root.attr("version"), Some("1.0"));

    let meta = root.find("meta").unwrap();
    assert_eq!(meta.attr("key"), Some("vendor"));
    assert_eq!(meta.text.as_deref(), Some("Aperio"));

    let levels = root.find("levels").unwrap();
    let level_nodes = levels.find_all("level");
    assert_eq!(level_nodes.len(), 2);
    assert_eq!(level_nodes[0].attr("id"), Some("0"));
    assert_eq!(level_nodes[0].attr("width"), Some("4096"));
    assert_eq!(level_nodes[1].attr("id"), Some("1"));
    assert_eq!(level_nodes[1].attr("height"), Some("1024"));

    // children should be empty for self-closing tags
    assert!(level_nodes[0].children.is_empty());
}

#[test]
fn deeply_nested_xml_rejected() {
    let depth = MAX_XML_DEPTH + 10;
    let open_tags: String = (0..depth).map(|i| format!("<n{}>", i)).collect();
    let close_tags: String = (0..depth).rev().map(|i| format!("</n{}>", i)).collect();
    let xml = format!("{}{}", open_tags, close_tags);

    let result = parse_xml(&xml);
    assert!(result.is_err());
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("nesting depth"),
        "expected depth error, got: {err_msg}"
    );
}

#[test]
fn duplicate_attributes_are_rejected() {
    let err = parse_xml(r#"<root value="first" value="second"/>"#).unwrap_err();
    assert!(
        err.to_string().contains("duplicated attribute"),
        "unexpected duplicate-attribute error: {err}"
    );
}

#[test]
fn excessive_attributes_are_rejected() {
    let attributes = (0..=MAX_XML_ATTRIBUTES_PER_NODE)
        .map(|idx| format!(r#" a{idx}="value""#))
        .collect::<String>();
    let err = parse_xml(&format!("<root{attributes}/>")).unwrap_err();
    assert!(
        err.to_string().contains("attribute count"),
        "unexpected attribute-budget error: {err}"
    );
}

#[test]
fn document_type_entities_are_not_expanded() {
    let xml = r#"<!DOCTYPE root [<!ENTITY x "expanded">]><root>&x;</root>"#;
    let root = parse_xml(xml).unwrap();
    assert_ne!(root.text.as_deref(), Some("expanded"));
}

#[test]
fn budget_counters_accept_exact_limits_and_reject_the_next_item() {
    let mut node_budget = XmlBudget {
        nodes: MAX_XML_NODES - 1,
        attributes: 0,
    };
    node_budget.add_node().unwrap();
    assert_eq!(node_budget.nodes, MAX_XML_NODES);
    assert!(node_budget.add_node().is_err());

    let mut attribute_budget = XmlBudget {
        nodes: 0,
        attributes: MAX_XML_ATTRIBUTES - 1,
    };
    attribute_budget.add_attributes(1).unwrap();
    assert_eq!(attribute_budget.attributes, MAX_XML_ATTRIBUTES);
    assert!(attribute_budget.add_attributes(1).is_err());
}

#[test]
fn malformed_attribute_values_fail_for_empty_and_nonempty_elements() {
    for xml in [
        r#"<root value="&#x110000;"/>"#,
        r#"<root value="&#x110000;">text</root>"#,
    ] {
        let error = parse_xml(xml).expect_err("invalid Unicode entity must be rejected");
        assert!(error.to_string().contains("XML"));
    }

    let duplicate = parse_xml(r#"<root value="first" value="second">text</root>"#)
        .expect_err("duplicate recursive attribute must be rejected");
    assert!(duplicate.to_string().contains("duplicated attribute"));
}

#[test]
fn malformed_text_entity_is_not_expanded() {
    let root = parse_xml(r#"<root>&#x110000;</root>"#).expect("general references stay opaque");
    assert_eq!(root.text, None);
}
