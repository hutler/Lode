//! A small browser for tests of the admin web UI: cookies, forms and the parsed page.
//!
//! Pages are parsed as XML, which the admin UI's markup is (void elements are closed, every
//! value is escaped), so a page that is not well formed fails the test, and so does a value
//! that was not escaped.

use std::collections::BTreeMap;

use quick_xml::events::Event;
use quick_xml::Reader;
use reqwest::header;

use crate::must;

/// An element of a parsed page.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Node {
    /// Tag name.
    pub name: String,
    /// Attributes, unescaped.
    pub attrs: Vec<(String, String)>,
    /// Elements and text inside.
    pub children: Vec<Child>,
}

/// What an element holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Child {
    /// An element.
    Element(Node),
    /// Text, unescaped.
    Text(String),
}

impl Node {
    /// An attribute's value.
    pub fn attr(&self, name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }

    /// The text inside, with runs of white space made one space.
    pub fn text(&self) -> String {
        let mut out = String::new();
        self.collect_text(&mut out);
        out.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    fn collect_text(&self, out: &mut String) {
        for c in &self.children {
            match c {
                Child::Text(t) => out.push_str(t),
                Child::Element(e) => {
                    out.push(' ');
                    e.collect_text(out);
                    out.push(' ');
                }
            }
        }
    }

    /// Every element inside (not this one) for which `f` holds, in document order.
    pub fn find(&self, f: &dyn Fn(&Node) -> bool) -> Vec<&Node> {
        let mut out = Vec::new();
        self.walk(f, &mut out);
        out
    }

    fn walk<'a>(&'a self, f: &dyn Fn(&Node) -> bool, out: &mut Vec<&'a Node>) {
        for c in &self.children {
            if let Child::Element(e) = c {
                if f(e) {
                    out.push(e);
                }
                e.walk(f, out);
            }
        }
    }

    /// Every element with this tag name.
    pub fn all(&self, tag: &str) -> Vec<&Node> {
        self.find(&|n| n.name == tag)
    }

    /// The element with this id.
    pub fn by_id(&self, id: &str) -> Option<&Node> {
        self.find(&|n| n.attr("id") == Some(id)).into_iter().next()
    }

    /// Elements with this tag whose text contains `text`.
    pub fn containing(&self, tag: &str, text: &str) -> Vec<&Node> {
        self.find(&|n| n.name == tag && n.text().contains(text))
    }
}

/// Parses a page. Fails the test when it is not well formed.
pub fn parse(page: &str) -> Node {
    let mut reader = Reader::from_str(page);
    reader.config_mut().check_end_names = true;
    let mut stack = vec![Node {
        name: "#document".into(),
        ..Node::default()
    }];
    let element = |e: &quick_xml::events::BytesStart<'_>| -> Node {
        let name = String::from_utf8_lossy(e.name().as_ref()).into_owned();
        let attrs = e
            .attributes()
            .map(|a| {
                let a = must(a, "attribute");
                (
                    String::from_utf8_lossy(a.key.as_ref()).into_owned(),
                    must(a.unescape_value(), "attribute value").into_owned(),
                )
            })
            .collect();
        Node {
            name,
            attrs,
            children: Vec::new(),
        }
    };
    let push = |stack: &mut Vec<Node>, c: Child| {
        if let Some(top) = stack.last_mut() {
            top.children.push(c);
        }
    };
    loop {
        match must(reader.read_event(), "well-formed page") {
            Event::Start(e) => stack.push(element(&e)),
            Event::Empty(e) => push(&mut stack, Child::Element(element(&e))),
            Event::End(_) => {
                let done = must(stack.pop().ok_or("unbalanced"), "page structure");
                push(&mut stack, Child::Element(done));
            }
            Event::Text(t) => push(
                &mut stack,
                Child::Text(must(t.decode(), "text").into_owned()),
            ),
            Event::GeneralRef(r) => {
                let text = match must(r.resolve_char_ref(), "character reference") {
                    Some(c) => c.to_string(),
                    None => {
                        let name = must(r.decode(), "entity");
                        must(
                            quick_xml::escape::unescape(&format!("&{name};")),
                            "entity reference",
                        )
                        .into_owned()
                    }
                };
                push(&mut stack, Child::Text(text));
            }
            Event::CData(t) => push(
                &mut stack,
                Child::Text(String::from_utf8_lossy(&t).into_owned()),
            ),
            Event::Eof => break,
            _ => {}
        }
    }
    must(
        (stack.len() == 1).then_some(()).ok_or("unclosed elements"),
        "page structure",
    );
    stack.pop().unwrap_or_default()
}

/// One answer.
#[derive(Debug, Clone)]
pub struct Page {
    /// Status code.
    pub status: u16,
    /// Headers, lower-case names; repeated headers joined with a line break.
    pub headers: BTreeMap<String, String>,
    /// `Set-Cookie` headers.
    pub set_cookies: Vec<String>,
    /// Body.
    pub body: String,
}

impl Page {
    /// The parsed body.
    pub fn doc(&self) -> Node {
        parse(&self.body)
    }

    /// Where a redirect leads.
    pub fn location(&self) -> Option<&str> {
        self.headers.get("location").map(String::as_str)
    }

    /// The `Set-Cookie` header for cookie `name`.
    pub fn cookie(&self, name: &str) -> Option<&str> {
        self.set_cookies
            .iter()
            .find(|c| c.starts_with(&format!("{name}=")))
            .map(String::as_str)
    }
}

/// A browser: keeps cookies, does not follow redirects by itself.
pub struct Browser {
    http: reqwest::Client,
    base: String,
    /// Cookies by name.
    pub cookies: BTreeMap<String, String>,
}

impl Browser {
    /// A browser for the server at `base` (such as `http://127.0.0.1:41234`).
    pub fn new(base: &str) -> Self {
        Self::with_client(
            base,
            must(
                reqwest::Client::builder()
                    .redirect(reqwest::redirect::Policy::none())
                    .no_proxy()
                    .build(),
                "HTTP client",
            ),
        )
    }

    /// A browser using this client, which must not follow redirects.
    pub fn with_client(base: &str, http: reqwest::Client) -> Self {
        Self {
            http,
            base: base.trim_end_matches('/').to_owned(),
            cookies: BTreeMap::new(),
        }
    }

    fn url(&self, path: &str) -> String {
        if path.starts_with("http://") || path.starts_with("https://") {
            path.to_owned()
        } else {
            format!("{}{path}", self.base)
        }
    }

    async fn send(&mut self, req: reqwest::RequestBuilder) -> Page {
        let req = if self.cookies.is_empty() {
            req
        } else {
            let jar = self
                .cookies
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join("; ");
            req.header(header::COOKIE, jar)
        };
        let resp = must(req.send().await, "request");
        let status = resp.status().as_u16();
        let mut headers = BTreeMap::new();
        let mut set_cookies = Vec::new();
        for (k, v) in resp.headers() {
            let v = v.to_str().unwrap_or("").to_owned();
            if k == header::SET_COOKIE {
                set_cookies.push(v.clone());
            }
            headers
                .entry(k.as_str().to_owned())
                .and_modify(|e: &mut String| {
                    e.push('\n');
                    e.push_str(&v);
                })
                .or_insert(v);
        }
        for c in &set_cookies {
            let first = c.split(';').next().unwrap_or("");
            if let Some((name, value)) = first.split_once('=') {
                let gone = c.contains("Max-Age=0") || value.is_empty();
                if gone {
                    self.cookies.remove(name);
                } else {
                    self.cookies.insert(name.to_owned(), value.to_owned());
                }
            }
        }
        let body = must(resp.text().await, "body");
        Page {
            status,
            headers,
            set_cookies,
            body,
        }
    }

    /// `GET path`.
    pub async fn get(&mut self, path: &str) -> Page {
        let req = self.http.get(self.url(path));
        self.send(req).await
    }

    /// `POST path` with form fields.
    pub async fn post(&mut self, path: &str, fields: &[(&str, &str)]) -> Page {
        let req = self.http.post(self.url(path)).form(fields);
        self.send(req).await
    }

    /// `GET path`, following redirects (at most 10) to the last page.
    pub async fn follow(&mut self, path: &str) -> Page {
        let mut url = self.url(path);
        for _ in 0..10 {
            let page = self.get(&url).await;
            match page.location() {
                Some(loc) if (300..400).contains(&page.status) => {
                    let base = must(reqwest::Url::parse(&url), "address");
                    url = must(base.join(loc), "redirect").to_string();
                }
                _ => return page,
            }
        }
        must(Err("more than 10 redirects"), "follow")
    }

    /// Submits the first form of `page` whose `action` is `action`, with the values it holds
    /// and `fields` in their place.
    pub async fn submit(&mut self, page: &Page, action: &str, fields: &[(&str, &str)]) -> Page {
        let doc = page.doc();
        let form = must(
            doc.find(&|n| n.name == "form" && n.attr("action") == Some(action))
                .into_iter()
                .next()
                .cloned()
                .ok_or_else(|| format!("no form for {action}")),
            "form",
        );
        // What the form would send as it stands: inputs with values, ticked boxes and
        // text areas; `fields` replace those.
        let mut all: Vec<(String, String)> = form
            .find(&|n| {
                (n.name == "input"
                    && match n.attr("type").unwrap_or("text") {
                        "checkbox" | "radio" => n.attr("checked").is_some(),
                        "submit" | "button" => false,
                        _ => true,
                    })
                    || n.name == "textarea"
            })
            .iter()
            .filter_map(|n| {
                let value = if n.name == "textarea" {
                    n.children
                        .iter()
                        .map(|c| match c {
                            Child::Text(t) => t.clone(),
                            Child::Element(_) => String::new(),
                        })
                        .collect()
                } else {
                    n.attr("value").unwrap_or("on").to_owned()
                };
                Some((n.attr("name")?.to_owned(), value))
            })
            .filter(|(k, _)| !fields.iter().any(|(f, _)| f == k))
            .collect();
        all.extend(
            fields
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned())),
        );
        let pairs: Vec<(&str, &str)> = all.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        self.post(action, &pairs).await
    }
}

#[cfg(test)]
#[allow(clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn parses_markup_and_unescapes() {
        let doc = parse(
            "<!DOCTYPE html>\n<html><body><p id=\"x\" title=\"a&amp;b\">1 &lt;script&gt; &#39;q&#39;\
             <br/><b>bold</b></p></body></html>",
        );
        let p = doc.by_id("x").unwrap_or_else(|| panic!("no p"));
        assert_eq!(p.attr("title"), Some("a&b"));
        assert_eq!(p.text(), "1 <script> 'q' bold");
        assert!(doc.all("script").is_empty());
        assert_eq!(doc.containing("b", "bold").len(), 1);
    }
}
