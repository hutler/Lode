//! The two S3 answers this store reads: a ListObjectsV2 page and an error body.

use quick_xml::events::Event;
use quick_xml::Reader;

/// One page of ListObjectsV2.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Page {
    /// `(Key, LastModified)` of each object.
    pub(crate) objects: Vec<(String, String)>,
    /// `IsTruncated`.
    pub(crate) truncated: bool,
    /// `NextContinuationToken`.
    pub(crate) next_token: Option<String>,
}

/// Walks a document, calling `f(path, text)` for the text of every element, where `path`
/// is the element names from the root down.
fn walk(input: &[u8], mut f: impl FnMut(&[String], &str)) -> Result<(), String> {
    let mut reader = Reader::from_reader(input);
    let mut path: Vec<String> = Vec::new();
    let mut text = String::new();
    let mut buf = Vec::new();
    loop {
        let ev = reader
            .read_event_into(&mut buf)
            .map_err(|e| e.to_string())?;
        match ev {
            Event::Start(e) => {
                path.push(String::from_utf8_lossy(e.local_name().as_ref()).into_owned());
                text.clear();
            }
            Event::End(_) => {
                f(&path, &text);
                text.clear();
                if path.pop().is_none() {
                    return Err("unbalanced XML".into());
                }
            }
            Event::Text(t) => text.push_str(&t.xml_content().map_err(|e| e.to_string())?),
            Event::CData(t) => text.push_str(&t.decode().map_err(|e| e.to_string())?),
            Event::GeneralRef(r) => {
                let s = if r.is_char_ref() {
                    r.resolve_char_ref()
                        .map_err(|e| e.to_string())?
                        .map(String::from)
                        .ok_or("bad character reference")?
                } else {
                    let name = r.decode().map_err(|e| e.to_string())?;
                    quick_xml::escape::resolve_predefined_entity(&name)
                        .ok_or_else(|| format!("unknown entity &{name};"))?
                        .to_owned()
                };
                text.push_str(&s);
            }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    if path.is_empty() {
        Ok(())
    } else {
        Err("unclosed element".into())
    }
}

/// Parses a `ListBucketResult`.
pub(crate) fn parse_list(input: &[u8]) -> Result<Page, String> {
    let mut page = Page::default();
    let mut key: Option<String> = None;
    let mut modified: Option<String> = None;
    let mut root_ok = false;
    walk(input, |path, text| match path {
        [root] => root_ok = root == "ListBucketResult",
        [_, f] if f == "IsTruncated" => page.truncated = text.trim() == "true",
        [_, f] if f == "NextContinuationToken" => page.next_token = Some(text.to_owned()),
        [_, c, f] if c == "Contents" && f == "Key" => key = Some(text.to_owned()),
        [_, c, f] if c == "Contents" && f == "LastModified" => {
            modified = Some(text.trim().to_owned())
        }
        [_, c] if c == "Contents" => {
            if let (Some(k), Some(m)) = (key.take(), modified.take()) {
                page.objects.push((k, m));
            }
        }
        _ => {}
    })?;
    if root_ok {
        Ok(page)
    } else {
        Err("not a ListBucketResult".into())
    }
}

/// The `Code` of an S3 `Error` body, if it is one.
pub(crate) fn error_code(input: &[u8]) -> Option<String> {
    let mut code = None;
    let _ = walk(input, |path, text| {
        if let [root, f] = path {
            if root == "Error" && f == "Code" {
                code = Some(text.trim().to_owned());
            }
        }
    });
    code.filter(|c| {
        !c.is_empty() && c.len() <= 64 && c.chars().all(|ch| ch.is_ascii_alphanumeric())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_pages() {
        let page = parse_list(
            br#"<?xml version="1.0" encoding="UTF-8"?>
<ListBucketResult xmlns="http://s3.amazonaws.com/doc/2006-03-01/">
  <Name>b</Name><Prefix>p/</Prefix><KeyCount>2</KeyCount><MaxKeys>2</MaxKeys>
  <IsTruncated>true</IsTruncated>
  <Contents><Key>p/a&amp;b</Key><LastModified>2026-11-14T10:00:00.000Z</LastModified>
    <ETag>&quot;x&quot;</ETag><Size>5</Size><StorageClass>STANDARD</StorageClass></Contents>
  <Contents><Key>p/c</Key><LastModified>2026-11-14T10:00:01Z</LastModified></Contents>
  <NextContinuationToken>tok/+=</NextContinuationToken>
</ListBucketResult>"#,
        )
        .unwrap();
        assert_eq!(
            page.objects,
            vec![
                ("p/a&b".to_owned(), "2026-11-14T10:00:00.000Z".to_owned()),
                ("p/c".to_owned(), "2026-11-14T10:00:01Z".to_owned())
            ]
        );
        assert!(page.truncated);
        assert_eq!(page.next_token.as_deref(), Some("tok/+="));

        let last =
            parse_list(b"<ListBucketResult><IsTruncated>false</IsTruncated></ListBucketResult>")
                .unwrap();
        assert_eq!(last, Page::default());
        assert!(parse_list(b"<Error><Code>AccessDenied</Code></Error>").is_err());
        assert!(parse_list(b"<ListBucketResult>").is_err());
    }

    #[test]
    fn error_codes() {
        let body = b"<?xml version=\"1.0\"?><Error><Code>NoSuchBucket</Code><Message>gone</Message></Error>";
        assert_eq!(error_code(body).as_deref(), Some("NoSuchBucket"));
        assert_eq!(error_code(b"<html>bad gateway</html>"), None);
        assert_eq!(error_code(b""), None);
        assert_eq!(error_code(b"<Error><Code>a b</Code></Error>"), None);
    }
}
