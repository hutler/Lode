//! A small element tree over `quick-xml`, enough for the XML that `svn --xml` prints.

use std::collections::BTreeMap;

use quick_xml::events::Event;
use quick_xml::Reader;

use crate::ImportError;

/// One element: its name, attributes, text and children.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Element {
    pub(crate) name: String,
    pub(crate) attrs: BTreeMap<String, String>,
    pub(crate) text: String,
    pub(crate) children: Vec<Element>,
}

impl Element {
    /// The first child named `name`.
    pub(crate) fn child(&self, name: &str) -> Option<&Element> {
        self.children.iter().find(|c| c.name == name)
    }

    /// Every child named `name`.
    pub(crate) fn all<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Element> + 'a {
        self.children.iter().filter(move |c| c.name == name)
    }

    /// An attribute.
    pub(crate) fn attr(&self, name: &str) -> Option<&str> {
        self.attrs.get(name).map(String::as_str)
    }

    /// The text of the first child named `name`.
    pub(crate) fn child_text(&self, name: &str) -> Option<&str> {
        self.child(name).map(|c| c.text.as_str())
    }
}

fn bad(e: impl std::fmt::Display) -> ImportError {
    ImportError::Protocol(format!("unreadable XML from svn: {e}"))
}

/// Parses a document into its root element.
pub(crate) fn parse(input: &[u8]) -> Result<Element, ImportError> {
    let mut reader = Reader::from_reader(input);
    let mut stack: Vec<Element> = vec![Element::default()];
    let mut buf = Vec::new();
    loop {
        let ev = reader.read_event_into(&mut buf).map_err(bad)?;
        match ev {
            Event::Start(e) => {
                stack.push(start(&e)?);
            }
            Event::Empty(e) => {
                let el = start(&e)?;
                stack
                    .last_mut()
                    .ok_or_else(|| bad("unbalanced"))?
                    .children
                    .push(el);
            }
            Event::End(_) => {
                let el = stack.pop().ok_or_else(|| bad("unbalanced"))?;
                stack
                    .last_mut()
                    .ok_or_else(|| bad("unbalanced"))?
                    .children
                    .push(el);
            }
            Event::Text(t) => {
                let s = t.xml_content().map_err(bad)?;
                if let Some(top) = stack.last_mut() {
                    top.text.push_str(&s);
                }
            }
            Event::CData(t) => {
                let s = t.decode().map_err(bad)?;
                if let Some(top) = stack.last_mut() {
                    top.text.push_str(&s);
                }
            }
            Event::GeneralRef(r) => {
                let s = if r.is_char_ref() {
                    r.resolve_char_ref()
                        .map_err(bad)?
                        .map(String::from)
                        .ok_or_else(|| bad("bad character reference"))?
                } else {
                    let name = r.decode().map_err(bad)?;
                    quick_xml::escape::resolve_predefined_entity(&name)
                        .ok_or_else(|| bad(format!("unknown entity &{name};")))?
                        .to_owned()
                };
                if let Some(top) = stack.last_mut() {
                    top.text.push_str(&s);
                }
            }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    let mut doc = stack.pop().ok_or_else(|| bad("empty"))?;
    if !stack.is_empty() {
        return Err(bad("unclosed element"));
    }
    doc.children
        .pop()
        .filter(|_| doc.children.is_empty())
        .ok_or_else(|| bad("no single root element"))
}

fn start(e: &quick_xml::events::BytesStart<'_>) -> Result<Element, ImportError> {
    let name = String::from_utf8_lossy(e.name().as_ref()).into_owned();
    let mut attrs = BTreeMap::new();
    for a in e.attributes() {
        let a = a.map_err(bad)?;
        let key = String::from_utf8_lossy(a.key.as_ref()).into_owned();
        let value = a.unescape_value().map_err(bad)?.into_owned();
        attrs.insert(key, value);
    }
    Ok(Element {
        name,
        attrs,
        ..Element::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn elements_attributes_and_entities() {
        let doc = parse(
            br#"<?xml version="1.0"?>
<log><logentry revision="3"><msg>a &amp; b &lt;c&gt; &#233;</msg><x a="1 &quot;q&quot;"/></logentry></log>"#,
        )
        .unwrap();
        assert_eq!(doc.name, "log");
        let e = doc.child("logentry").unwrap();
        assert_eq!(e.attr("revision"), Some("3"));
        assert_eq!(e.child_text("msg"), Some("a & b <c> é"));
        assert_eq!(e.child("x").unwrap().attr("a"), Some("1 \"q\""));
        assert!(parse(b"<a><b></a>").is_err());
    }
}
