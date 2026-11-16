//! rTorrent's XML-RPC (<https://xmlrpc.com/spec.md>): a `methodCall` written
//! and a `methodResponse` read with quick-xml (motd spec, Tech Stack;
//! decision 10), in place of sb-go's go-rtorrent. Only what rTorrent's
//! answers to motd's four calls hold is read: `int`, `i4`, `i8`,
//! `string` (and an untyped value, a string by the spec), `array`, and a
//! `fault`'s `struct`; any other type is an error, where go-rtorrent's
//! type assertions panicked (Difference 13).
//!
//! Library-backed: quick-xml's `Writer` escapes the text it writes
//! (<https://docs.rs/quick-xml/0.42.0/quick_xml/writer/struct.Writer.html>),
//! and its `Reader` yields the events
//! (<https://docs.rs/quick-xml/0.42.0/quick_xml/reader/struct.Reader.html>),
//! references resolved with its `resolve_xml_entity` and
//! `BytesRef::resolve_char_ref`, an empty element expanded to its start
//! and end by the reader's `expand_empty_elements`. Why not serde through
//! quick-xml's `serialize` feature: its deserializer trims text that
//! stands alone (quick-xml 0.42.0 `src/de/mod.rs`), and XML-RPC's strings
//! keep their spaces; nor can the reader's `trim_text` stand in for the
//! blank-text rule here, since a reference (`&amp;`) is an event of its
//! own and trimming would eat the spaces beside it. Why not dxr or
//! another XML-RPC crate: map
//! policy 3 (thin clients); none is in the tree.

use std::collections::VecDeque;

use quick_xml::Writer;
use quick_xml::escape::resolve_xml_entity;
use quick_xml::events::{BytesDecl, BytesText, Event};
use quick_xml::reader::Reader;

/// How deep an answer may nest values: rTorrent's deepest is a list of
/// lists; a deeper one is refused rather than walked.
const MAX_DEPTH: usize = 16;

/// One XML-RPC value, as far as motd reads them.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Int(i64),
    Str(String),
    Array(Vec<Value>),
    Struct(Vec<(String, Value)>),
}

impl Value {
    /// The value as an integer, if it is one.
    #[must_use]
    pub fn int(&self) -> Option<i64> {
        match self {
            Value::Int(number) => Some(*number),
            _ => None,
        }
    }

    /// The value as a string, if it is one.
    #[must_use]
    pub fn str(&self) -> Option<&str> {
        match self {
            Value::Str(text) => Some(text),
            _ => None,
        }
    }
}

/// Why an answer gave no values.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    /// rTorrent's own fault.
    #[error("fault {code}: {message}")]
    Fault { code: i64, message: String },
    /// Not an XML-RPC answer motd can read.
    #[error("unexpected answer: {0}")]
    Unexpected(String),
}

/// A `methodCall` of `method` with `params`, each a string.
///
/// # Errors
///
/// Only an I/O error of the writer, which writes to memory.
pub fn method_call(method: &str, params: &[&str]) -> std::io::Result<Vec<u8>> {
    let mut writer = Writer::new(Vec::new());
    writer.write_event(Event::Decl(BytesDecl::new("1.0", None, None)))?;
    writer
        .create_element("methodCall")
        .write_inner_content(|writer| {
            writer
                .create_element("methodName")
                .write_text_content(BytesText::new(method))?;
            writer
                .create_element("params")
                .write_inner_content(|writer| {
                    for param in params {
                        writer
                            .create_element("param")
                            .write_inner_content(|writer| {
                                writer
                                    .create_element("value")
                                    .write_inner_content(|writer| {
                                        writer
                                            .create_element("string")
                                            .write_text_content(BytesText::new(param))?;
                                        Ok(())
                                    })?;
                                Ok(())
                            })?;
                    }
                    Ok(())
                })?;
            Ok(())
        })?;
    Ok(writer.into_inner())
}

/// A `methodResponse`'s parameters, or its fault.
///
/// # Errors
///
/// [`Error::Fault`] for a fault, [`Error::Unexpected`] for anything that
/// isn't an answer of the types above.
pub fn response(body: &str) -> Result<Vec<Value>, Error> {
    let mut walk = Walk {
        reader: Reader::from_str(body),
        queue: VecDeque::new(),
    };
    walk.reader.config_mut().expand_empty_elements = true;
    walk.open("methodResponse")?;
    match walk.next_start()?.as_str() {
        "params" => {
            let mut params = Vec::new();
            loop {
                match walk.significant()? {
                    Tag::Start(name) if name == "param" => {
                        walk.open("value")?;
                        params.push(walk.value(0)?);
                        walk.close("param")?;
                    }
                    Tag::End(name) if name == "params" => break,
                    other => return Err(unexpected(&other)),
                }
            }
            walk.close("methodResponse")?;
            Ok(params)
        }
        "fault" => {
            walk.open("value")?;
            let fault = walk.value(0)?;
            Err(fault_of(&fault))
        }
        other => Err(Error::Unexpected(format!("<{other}> in the answer"))),
    }
}

/// A fault's code and message, from its struct.
fn fault_of(fault: &Value) -> Error {
    let Value::Struct(members) = fault else {
        return Error::Unexpected("a fault that isn't a struct".to_string());
    };
    let member = |name: &str| members.iter().find(|(key, _)| key == name).map(|(_, v)| v);
    match (
        member("faultCode").and_then(Value::int),
        member("faultString").and_then(Value::str),
    ) {
        (Some(code), Some(message)) => Error::Fault {
            code,
            message: message.to_string(),
        },
        _ => Error::Unexpected("a fault without its code and string".to_string()),
    }
}

/// An element boundary or a run of text in the answer.
#[derive(Debug, PartialEq)]
enum Tag {
    Start(String),
    End(String),
    Text(String),
}

fn unexpected(tag: &Tag) -> Error {
    Error::Unexpected(match tag {
        Tag::Start(name) => format!("<{name}>"),
        Tag::End(name) => format!("</{name}>"),
        Tag::Text(_) => "text out of place".to_string(),
    })
}

/// Whether `tag` is text of spaces and line ends only, which XML-RPC's
/// layout may put between elements.
fn blank(tag: &Tag) -> bool {
    matches!(tag, Tag::Text(text) if text.trim().is_empty())
}

/// The reader, walked tag by tag, with the tags read ahead.
struct Walk<'a> {
    reader: Reader<&'a [u8]>,
    queue: VecDeque<Tag>,
}

impl Walk<'_> {
    /// The next tag: a run of text and references joined into one text,
    /// comments and the declaration passed over.
    fn next_tag(&mut self) -> Result<Tag, Error> {
        if let Some(tag) = self.queue.pop_front() {
            return Ok(tag);
        }
        let mut text: Option<String> = None;
        loop {
            let event = self
                .reader
                .read_event()
                .map_err(|err| Error::Unexpected(format!("not XML: {err}")))?;
            let tags = match event {
                Event::Text(t) => {
                    text.get_or_insert_with(String::new)
                        .push_str(&t.xml10_content());
                    continue;
                }
                Event::CData(t) => {
                    text.get_or_insert_with(String::new)
                        .push_str(&t.xml10_content());
                    continue;
                }
                Event::GeneralRef(r) => {
                    let resolved = match r.resolve_char_ref() {
                        Ok(Some(c)) => c.to_string(),
                        Ok(None) => resolve_xml_entity(&r)
                            .ok_or_else(|| Error::Unexpected(format!("&{};", &*r)))?
                            .to_string(),
                        Err(err) => return Err(Error::Unexpected(format!("not XML: {err}"))),
                    };
                    text.get_or_insert_with(String::new).push_str(&resolved);
                    continue;
                }
                Event::Start(start) => vec![Tag::Start(name(start.local_name().as_ref()))],
                Event::End(end) => vec![Tag::End(name(end.local_name().as_ref()))],
                Event::Eof => return Err(Error::Unexpected("the answer ends early".to_string())),
                // `expand_empty_elements` makes an empty element a start
                // and an end, so none comes.
                Event::Empty(_)
                | Event::Comment(_)
                | Event::Decl(_)
                | Event::PI(_)
                | Event::DocType(_) => continue,
            };
            if let Some(text) = text {
                self.queue.push_back(Tag::Text(text));
            }
            self.queue.extend(tags);
            return self
                .queue
                .pop_front()
                .ok_or_else(|| Error::Unexpected("no tag".to_string()));
        }
    }

    /// The next tag that isn't blank text.
    fn significant(&mut self) -> Result<Tag, Error> {
        loop {
            let tag = self.next_tag()?;
            if !blank(&tag) {
                return Ok(tag);
            }
        }
    }

    /// The next significant tag, which must start an element; its name.
    fn next_start(&mut self) -> Result<String, Error> {
        match self.significant()? {
            Tag::Start(name) => Ok(name),
            other => Err(unexpected(&other)),
        }
    }

    /// The next significant tag, which must start `element`.
    fn open(&mut self, element: &str) -> Result<(), Error> {
        match self.significant()? {
            Tag::Start(name) if name == element => Ok(()),
            other => Err(unexpected(&other)),
        }
    }

    /// The next significant tag, which must end `element`.
    fn close(&mut self, element: &str) -> Result<(), Error> {
        match self.significant()? {
            Tag::End(name) if name == element => Ok(()),
            other => Err(unexpected(&other)),
        }
    }

    /// The text up to the end of `element` (none: empty), kept as it is.
    fn text_of(&mut self, element: &str) -> Result<String, Error> {
        match self.next_tag()? {
            Tag::Text(text) => {
                self.close(element)?;
                Ok(text)
            }
            Tag::End(name) if name == element => Ok(String::new()),
            other => Err(unexpected(&other)),
        }
    }

    /// The value whose `<value>` was just read, and its `</value>`,
    /// nested at most [`MAX_DEPTH`] deep.
    fn value(&mut self, depth: usize) -> Result<Value, Error> {
        if depth > MAX_DEPTH {
            return Err(Error::Unexpected("values nested too deep".to_string()));
        }
        let first = self.next_tag()?;
        let typed = match first {
            // An untyped value is a string (the spec).
            Tag::Text(text) => match self.next_tag()? {
                Tag::End(name) if name == "value" => return Ok(Value::Str(text)),
                Tag::Start(name) if text.trim().is_empty() => name,
                other => return Err(unexpected(&other)),
            },
            Tag::End(name) if name == "value" => return Ok(Value::Str(String::new())),
            Tag::Start(name) => name,
            other @ Tag::End(_) => return Err(unexpected(&other)),
        };
        let value = match typed.as_str() {
            "int" | "i4" | "i8" => {
                let text = self.text_of(&typed)?;
                Value::Int(text.trim().parse().map_err(|_| {
                    Error::Unexpected(format!("<{typed}> that isn't a whole number"))
                })?)
            }
            "string" => Value::Str(self.text_of("string")?),
            "array" => {
                self.open("data")?;
                let mut items = Vec::new();
                loop {
                    match self.significant()? {
                        Tag::Start(name) if name == "value" => items.push(self.value(depth + 1)?),
                        Tag::End(name) if name == "data" => break,
                        other => return Err(unexpected(&other)),
                    }
                }
                self.close("array")?;
                Value::Array(items)
            }
            "struct" => {
                let mut members = Vec::new();
                loop {
                    match self.significant()? {
                        Tag::Start(name) if name == "member" => {
                            self.open("name")?;
                            let key = self.text_of("name")?;
                            self.open("value")?;
                            members.push((key, self.value(depth + 1)?));
                            self.close("member")?;
                        }
                        Tag::End(name) if name == "struct" => break,
                        other => return Err(unexpected(&other)),
                    }
                }
                Value::Struct(members)
            }
            other => return Err(Error::Unexpected(format!("a <{other}> value"))),
        };
        self.close("value")?;
        Ok(value)
    }
}

/// An element's local name as text.
fn name(local: &str) -> String {
    local.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_method_call_is_written_and_escaped() {
        let call = String::from_utf8(
            method_call("d.multicall2", &["", "main", "d.message=", "<&>"]).unwrap(),
        )
        .unwrap();
        assert_eq!(
            call,
            "<?xml version=\"1.0\"?><methodCall><methodName>d.multicall2</methodName><params>\
             <param><value><string></string></value></param>\
             <param><value><string>main</string></value></param>\
             <param><value><string>d.message=</string></value></param>\
             <param><value><string>&lt;&amp;&gt;</string></value></param>\
             </params></methodCall>"
        );
        assert_eq!(
            method_call("system.hostname", &[]).unwrap(),
            b"<?xml version=\"1.0\"?><methodCall><methodName>system.hostname</methodName><params></params></methodCall>"
        );
    }

    /// int, i4, i8, string (typed, untyped, empty, escaped), and an array
    /// of arrays, laid out as rTorrent writes them.
    #[test]
    fn a_response_reads_every_type_rtorrent_sends() {
        let body = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<methodResponse>\n<params>\n\
            <param><value><array><data>\n\
            <value><array><data><value><i8>1</i8></value><value><i4>0</i4></value><value><string>Tracker: [a &amp; b]</string></value></data></array></value>\n\
            <value><array><data><value><int> 7 </int></value><value><i8>-9000000000</i8></value><value><string/></value></data></array></value>\n\
            </data></array></value></param>\n\
            <param><value>untyped &#x41;&#66;</value></param>\n\
            <param><value></value></param>\n\
            <param><value><string>  kept  </string></value></param>\n\
            <!-- a comment -->\
            </params>\n</methodResponse>\n";
        let params = response(body).unwrap();
        assert_eq!(
            params,
            [
                Value::Array(vec![
                    Value::Array(vec![
                        Value::Int(1),
                        Value::Int(0),
                        Value::Str("Tracker: [a & b]".into()),
                    ]),
                    Value::Array(vec![
                        Value::Int(7),
                        Value::Int(-9_000_000_000),
                        Value::Str(String::new()),
                    ]),
                ]),
                Value::Str("untyped AB".into()),
                Value::Str(String::new()),
                Value::Str("  kept  ".into()),
            ]
        );
        assert_eq!(params[1].str(), Some("untyped AB"));
        assert_eq!(params[1].int(), None);
        assert_eq!(Value::Int(3).int(), Some(3));
        assert_eq!(Value::Int(3).str(), None);
    }

    #[test]
    fn a_fault_is_its_code_and_message() {
        let body = "<methodResponse><fault><value><struct>\
            <member><name>faultCode</name><value><int>-506</int></value></member>\
            <member><name>faultString</name><value><string>Method 'x' not defined</string></value></member>\
            </struct></value></fault></methodResponse>";
        let err = response(body).unwrap_err();
        assert_eq!(
            err,
            Error::Fault {
                code: -506,
                message: "Method 'x' not defined".into()
            }
        );
        assert_eq!(err.to_string(), "fault -506: Method 'x' not defined");
        let no_code = "<methodResponse><fault><value><struct><member><name>faultString</name><value>x</value></member></struct></value></fault></methodResponse>";
        assert!(matches!(response(no_code), Err(Error::Unexpected(_))));
        let not_struct =
            "<methodResponse><fault><value><int>1</int></value></fault></methodResponse>";
        assert!(matches!(response(not_struct), Err(Error::Unexpected(_))));
    }

    /// What isn't an answer of these types is an error, never a panic.
    #[test]
    fn anything_else_is_an_unexpected_answer() {
        let deep = format!(
            "<methodResponse><params><param><value>{}<int>1</int>{}</value></param></params></methodResponse>",
            "<array><data><value>".repeat(40),
            "</value></data></array>".repeat(40)
        );
        for body in [
            "",
            "not xml <",
            "<html><body>401</body></html>",
            "<methodResponse><params><param><value><double>1.5</double></value></param></params></methodResponse>",
            "<methodResponse><params><param><value><int>x</int></value></param></params></methodResponse>",
            "<methodResponse><params><param><value><int>1</string></value></param></params></methodResponse>",
            "<methodResponse><params><param><value><string>a</string>",
            "<methodResponse><params><param><value>a<int>1</int></value></param></params></methodResponse>",
            "<methodResponse><params><param><value><string>&bogus;</string></value></param></params></methodResponse>",
            "<methodResponse><params><other/></params></methodResponse>",
            "<methodResponse><other/></methodResponse>",
            "<methodResponse><params><param><value><struct><nope/></struct></value></param></params></methodResponse>",
            "<methodResponse><params><param><value><array><data><x/></data></array></value></param></params></methodResponse>",
            "<methodResponse><params>text</params></methodResponse>",
            &deep,
        ] {
            assert!(
                matches!(response(body), Err(Error::Unexpected(_))),
                "{body}"
            );
        }
        assert_eq!(
            response(&deep).unwrap_err().to_string(),
            "unexpected answer: values nested too deep"
        );
    }
}
