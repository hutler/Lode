//! Building HTML safely: every value is escaped unless it is already [`Html`].
//!
//! Pages are written with [`html!`](crate::html), which takes a literal format string and
//! escapes each argument. The format string must be a literal and cannot capture variables
//! by name (the macro builds it with `concat!`, where Rust refuses implicit captures), so the
//! only way to put text into a page unescaped is to make it `Html` here, from markup this
//! module built.

use std::fmt;

/// Markup that is safe to put into a page as it is.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Html(String);

impl Html {
    /// Empty markup.
    pub fn new() -> Self {
        Self(String::new())
    }

    /// Markup from a literal: text the program itself wrote.
    pub fn lit(s: &'static str) -> Self {
        Self(s.to_owned())
    }

    /// Markup from a string `html!` formatted. Not public: anything else must go through
    /// the macro or `lit`.
    #[doc(hidden)]
    pub fn from_formatted(s: String) -> Self {
        Self(s)
    }

    /// Appends more markup.
    pub fn push(&mut self, more: Html) {
        self.0.push_str(&more.0);
    }

    /// The markup as text.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The markup as an owned string.
    pub fn into_string(self) -> String {
        self.0
    }

    /// Whether there is no markup.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl FromIterator<Html> for Html {
    fn from_iter<I: IntoIterator<Item = Html>>(iter: I) -> Self {
        let mut out = Html::new();
        for h in iter {
            out.push(h);
        }
        out
    }
}

/// Something that can go into a page: markup as it is, everything else escaped.
pub trait Render {
    /// Writes `self` into `out`.
    fn render(&self, out: &mut String);
}

impl Render for Html {
    fn render(&self, out: &mut String) {
        out.push_str(&self.0);
    }
}

impl Render for str {
    fn render(&self, out: &mut String) {
        escape_into(self, out);
    }
}

impl Render for String {
    fn render(&self, out: &mut String) {
        escape_into(self, out);
    }
}

impl<T: Render + ?Sized> Render for &T {
    fn render(&self, out: &mut String) {
        (**self).render(out);
    }
}

impl<T: Render> Render for Option<T> {
    fn render(&self, out: &mut String) {
        if let Some(v) = self {
            v.render(out);
        }
    }
}

macro_rules! render_display {
    ($($t:ty),*) => {$(
        impl Render for $t {
            fn render(&self, out: &mut String) {
                escape_into(&self.to_string(), out);
            }
        }
    )*};
}
render_display!(
    u8,
    u16,
    u32,
    u64,
    usize,
    i32,
    i64,
    char,
    std::net::SocketAddr
);

/// Displays a value the way [`Render`] writes it, for `format!`.
#[doc(hidden)]
pub struct Shown<'a, T: Render + ?Sized>(pub &'a T);

impl<T: Render + ?Sized> fmt::Display for Shown<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut s = String::new();
        self.0.render(&mut s);
        f.write_str(&s)
    }
}

/// Escapes text for HTML content and quoted attribute values.
pub fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    escape_into(text, &mut out);
    out
}

fn escape_into(text: &str, out: &mut String) {
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            // Kept through attribute values, where a parser may fold a raw line break.
            '\n' => out.push_str("&#10;"),
            '\r' => out.push_str("&#13;"),
            // Control characters other than tab and line breaks are not allowed in HTML.
            c if c.is_control() && c != '\t' => out.push('\u{fffd}'),
            c => out.push(c),
        }
    }
}

/// Formats markup: the format string is literal markup, every argument is escaped unless it
/// is [`Html`].
///
/// ```ignore
/// let row = html!("<td>{}</td><td>{}</td>", depot.name, revisions);
/// ```
#[macro_export]
#[doc(hidden)]
macro_rules! html {
    ($fmt:literal $(, $arg:expr)* $(,)?) => {
        $crate::admin::html::Html::from_formatted(format!(
            concat!($fmt) $(, $crate::admin::html::Shown(&$arg))*
        ))
    };
}

/// Percent-encodes one path segment or query value.
pub fn encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for b in text.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_are_escaped_and_markup_is_not() {
        let name = "<script>alert('x')</script>&\"";
        let inner = crate::html!("<b>{}</b>", "a<b");
        let page = crate::html!("<p title=\"{}\">{} {} {}</p>", name, name, inner, 42u64);
        assert_eq!(
            page.as_str(),
            "<p title=\"&lt;script&gt;alert(&#39;x&#39;)&lt;/script&gt;&amp;&quot;\">\
             &lt;script&gt;alert(&#39;x&#39;)&lt;/script&gt;&amp;&quot; <b>a&lt;b</b> 42</p>"
        );
    }

    #[test]
    fn options_and_lists() {
        let none: Option<String> = None;
        assert_eq!(crate::html!("[{}]", none).as_str(), "[]");
        assert_eq!(crate::html!("[{}]", Some("<")).as_str(), "[&lt;]");
        let list: Html = ["a", "<"]
            .iter()
            .map(|s| crate::html!("<li>{}</li>", s))
            .collect();
        assert_eq!(list.as_str(), "<li>a</li><li>&lt;</li>");
        assert_eq!(escape("\u{1}x"), "\u{fffd}x");
    }

    #[test]
    fn encoding() {
        assert_eq!(encode("main"), "main");
        assert_eq!(encode("feature/a b"), "feature%2Fa%20b");
        assert_eq!(encode("é"), "%C3%A9");
    }
}
