//! A strict reader for the XML property lists xbps writes.
//!
//! General XML parsers bring features that only help an attacker here: entity expansion, processing
//! instructions, and recursion that a deeply nested file can overflow. A duplicate key is also a
//! risk in its own right, because this code and xbps might each believe a different copy. So this
//! accepts exactly the subset xbps emits and refuses everything else, and it does not recurse.

use std::collections::BTreeMap;

const MAX_DEPTH: usize = 16;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Plist {
    String(String),
    Integer(String),
    Bool(bool),
    Data(String),
    Array(Vec<Plist>),
    Dict(BTreeMap<String, Plist>),
}

impl Plist {
    pub fn as_dict(&self) -> Option<&BTreeMap<String, Plist>> {
        match self {
            Plist::Dict(map) => Some(map),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Plist::String(text) => Some(text),
            _ => None,
        }
    }

    pub fn as_array(&self) -> Option<&[Plist]> {
        match self {
            Plist::Array(items) => Some(items),
            _ => None,
        }
    }
}

enum Frame {
    Array(Vec<Plist>),
    Dict { map: BTreeMap<String, Plist>, key: Option<String> },
}

struct Reader<'a> {
    rest: &'a str,
}

impl<'a> Reader<'a> {
    fn skip_whitespace(&mut self) {
        self.rest = self.rest.trim_start_matches([' ', '\t', '\n', '\r']);
    }

    /// Reads `<...>` and returns what is between the brackets.
    fn tag(&mut self) -> Result<&'a str, String> {
        self.skip_whitespace();
        let rest = self.rest.strip_prefix('<').ok_or("expected a tag")?;
        let end = rest.find('>').ok_or("a tag is not closed")?;
        self.rest = &rest[end + 1..];
        Ok(&rest[..end])
    }

    /// Reads text up to the closing tag of a leaf element.
    fn leaf_text(&mut self, name: &str) -> Result<String, String> {
        let end = self.rest.find('<').ok_or("an element is not closed")?;
        let raw = &self.rest[..end];
        self.rest = &self.rest[end..];
        let closing = self.tag()?;
        if closing.strip_prefix('/') != Some(name) {
            return Err(format!("expected the closing tag of {name}"));
        }
        unescape(raw)
    }
}

/// Only the five predefined entities. A numeric reference could hide a `/` or `.` from a check on
/// the raw text, and xbps' own parser may read it differently.
fn unescape(raw: &str) -> Result<String, String> {
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        let after = &rest[at + 1..];
        let end = after.find(';').ok_or("an entity is not closed")?;
        out.push(match &after[..end] {
            "amp" => '&',
            "lt" => '<',
            "gt" => '>',
            "quot" => '"',
            "apos" => '\'',
            _ => return Err("unsupported entity".to_string()),
        });
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

fn attach(stack: &mut [Frame], root: &mut Option<Plist>, value: Plist) -> Result<(), String> {
    match stack.last_mut() {
        None => {
            if root.replace(value).is_some() {
                return Err("the plist has more than one root value".to_string());
            }
            Ok(())
        }
        Some(Frame::Array(items)) => {
            items.push(value);
            Ok(())
        }
        Some(Frame::Dict { map, key }) => {
            let key = key.take().ok_or("a dict value has no key")?;
            if map.contains_key(&key) {
                return Err("a dict has a duplicate key".to_string());
            }
            map.insert(key, value);
            Ok(())
        }
    }
}

/// Parses a property list, or explains in a short fixed message why it was refused.
pub fn parse(bytes: &[u8]) -> Result<Plist, String> {
    let text = std::str::from_utf8(bytes).map_err(|_| "is not valid UTF-8")?;
    let mut reader = Reader { rest: text };

    let mut tag = reader.tag()?;
    if tag.starts_with("?xml") && tag.ends_with('?') {
        tag = reader.tag()?;
    }
    if tag.starts_with("!DOCTYPE") {
        // An internal subset is where entity definitions live.
        if tag.contains('[') {
            return Err("has a DOCTYPE internal subset".to_string());
        }
        tag = reader.tag()?;
    }
    if tag != "plist" && !tag.starts_with("plist ") {
        return Err("the root element is not plist".to_string());
    }

    let mut stack: Vec<Frame> = Vec::new();
    let mut root = None;
    loop {
        let tag = reader.tag()?;
        match tag {
            "/plist" => {
                if !stack.is_empty() {
                    return Err("a collection is not closed".to_string());
                }
                break;
            }
            "array" | "dict" => {
                if stack.len() >= MAX_DEPTH {
                    return Err("is nested too deeply".to_string());
                }
                stack.push(if tag == "array" {
                    Frame::Array(Vec::new())
                } else {
                    Frame::Dict { map: BTreeMap::new(), key: None }
                });
            }
            "/array" | "/dict" => {
                let value = match (stack.pop(), tag) {
                    (Some(Frame::Array(items)), "/array") => Plist::Array(items),
                    (Some(Frame::Dict { map, key: None }), "/dict") => Plist::Dict(map),
                    _ => return Err("mismatched closing tag".to_string()),
                };
                attach(&mut stack, &mut root, value)?;
            }
            "key" => {
                let text = reader.leaf_text("key")?;
                match stack.last_mut() {
                    Some(Frame::Dict { key, .. }) if key.is_none() => *key = Some(text),
                    _ => return Err("a key is misplaced".to_string()),
                }
            }
            "string" => {
                let value = Plist::String(reader.leaf_text("string")?);
                attach(&mut stack, &mut root, value)?;
            }
            "integer" => {
                let text = reader.leaf_text("integer")?;
                if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
                    return Err("an integer is not a number".to_string());
                }
                attach(&mut stack, &mut root, Plist::Integer(text))?;
            }
            "data" => {
                let value = Plist::Data(reader.leaf_text("data")?);
                attach(&mut stack, &mut root, value)?;
            }
            "true/" => attach(&mut stack, &mut root, Plist::Bool(true))?,
            "false/" => attach(&mut stack, &mut root, Plist::Bool(false))?,
            "string/" => attach(&mut stack, &mut root, Plist::String(String::new()))?,
            "array/" => attach(&mut stack, &mut root, Plist::Array(Vec::new()))?,
            "dict/" => attach(&mut stack, &mut root, Plist::Dict(BTreeMap::new()))?,
            _ => return Err("has an unsupported element".to_string()),
        }
    }

    reader.skip_whitespace();
    if !reader.rest.is_empty() {
        return Err("has content after the end of the plist".to_string());
    }
    root.ok_or_else(|| "is empty".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEADER: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple Computer//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n";

    fn plist(body: &str) -> Vec<u8> {
        format!("{HEADER}<plist version=\"1.0\">\n{body}\n</plist>\n").into_bytes()
    }

    #[test]
    fn parses_what_xbps_writes() {
        let parsed = parse(&plist(
            "<dict>\n\t<key>a</key>\n\t<string>x &amp; y</string>\n\t<key>b</key>\n\t<array>\n\t\t<string>1</string>\n\t</array>\n\t<key>c</key>\n\t<integer>5</integer>\n\t<key>d</key>\n\t<true/>\n\t<key>e</key>\n\t<false/>\n\t<key>f</key>\n\t<data>AAAA</data>\n\t<key>g</key>\n\t<array/>\n\t<key>h</key>\n\t<dict/>\n\t<key>i</key>\n\t<string/>\n</dict>",
        ))
        .unwrap();
        let dict = parsed.as_dict().unwrap();
        assert_eq!(dict["a"].as_str(), Some("x & y"));
        assert_eq!(dict["b"].as_array().unwrap()[0].as_str(), Some("1"));
        assert_eq!(dict["c"], Plist::Integer("5".into()));
        assert_eq!(dict["d"], Plist::Bool(true));
        assert_eq!(dict["e"], Plist::Bool(false));
        assert_eq!(dict.len(), 9);
    }

    #[test]
    fn refuses_duplicate_keys() {
        let body = "<dict><key>pkgname</key><string>a</string><key>pkgname</key><string>b</string></dict>";
        assert!(parse(&plist(body)).unwrap_err().contains("duplicate"));
    }

    #[test]
    fn refuses_deep_nesting_without_recursing() {
        let deep = "<array>".repeat(2_000_000);
        assert!(parse(&plist(&deep)).unwrap_err().contains("deeply"));
    }

    #[test]
    fn refuses_malformed_and_unsupported_input() {
        for body in [
            "",
            "<dict>",
            "</dict>",
            "<dict></array>",
            "<array></dict>",
            "<dict><string>no key</string></dict>",
            "<dict><key>a</key></dict>",
            "<dict><key>a</key><key>b</key><string>x</string></dict>",
            "<array><key>a</key></array>",
            "<string>unclosed",
            "<string>a<b>c</b></string>",
            "<string>&custom;</string>",
            "<string>&#47;etc</string>",
            "<string>&amp</string>",
            "<integer>abc</integer>",
            "<date>2020</date>",
            "<!-- comment --><dict/>",
            "<![CDATA[x]]>",
            "<?pi x?><dict/>",
            "<string>a</string><string>b</string>",
            "<dict foo=\"bar\"/>",
        ] {
            assert!(parse(&plist(body)).is_err(), "{body:?} should be refused");
        }
    }

    #[test]
    fn refuses_non_plist_documents() {
        for document in [
            &b""[..],
            b"bplist00\x00\x01",
            b"\xff\xfe<plist>",
            b"<html></html>",
            b"<?xml version=\"1.0\"?><!DOCTYPE plist [<!ENTITY x \"y\">]><plist><string>&x;</string></plist>",
            b"<plist><string>a</string></plist>trailing",
        ] {
            assert!(parse(document).is_err(), "{document:?}");
        }
    }
}
