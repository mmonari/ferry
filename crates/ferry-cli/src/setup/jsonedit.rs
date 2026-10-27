//! Edits to the text of a JSON file that leave every other byte alone.
//!
//! `ferry setup` adds one element to an array of the user's `shell.json` and takes it out again on
//! `--undo`. The shell writes that file itself, in the order its keys were written, so a parse and
//! a re-serialize would reorder them and show the user a diff they did not ask for. These
//! functions find the array in the text and edit the text around one element, nothing else.
//!
//! The callers parse the text with serde_json first, so this module may trust that it is valid
//! JSON, but it never panics on anything else: a text it cannot follow gives `None`. And the
//! callers parse the result again and compare it with the value they expect, so a mistake here
//! is caught before anything is written.

use std::ops::Range;

/// An array in the text: where its brackets are and where each element is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Array {
    /// The byte offset of `[`.
    pub open: usize,
    /// The byte offset of `]`.
    pub close: usize,
    /// The span of each element, without the whitespace around it.
    pub elements: Vec<Range<usize>>,
}

/// What [`insert_id`] put in: a string where the array holds only strings, else an object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Inserted {
    pub text: String,
    /// The new element is the bare string `"<id>"`, not `{ "id": "<id>" }`.
    pub bare_string: bool,
}

/// Finds the array at `path` (object keys, from the root). `None` when the text is not followed
/// that far, when a key on the way appears twice (which of them a reader takes is a guess), or
/// when the value there is not an array.
pub fn array_at(text: &str, path: &[&str]) -> Option<Array> {
    let mut at = 0;
    for wanted in path {
        let members = Scan::at(text, at).members()?;
        let mut hits = members
            .into_iter()
            .filter(|(key, _)| key_is(text, key, wanted));
        let (_, value) = hits.next()?;
        if hits.next().is_some() {
            return None;
        }
        at = value.start;
    }
    let mut scan = Scan::at(text, at);
    let elements = scan.elements()?;
    Some(Array {
        open: at,
        close: scan.i.checked_sub(1)?,
        elements,
    })
}

/// `text` with the element `id` added at `index` (past the end appends), written the way the
/// neighbours are: on its own lines in a file that has lines, on the same line in one that does
/// not, as a bare string among strings, and with the indentation the file already uses.
pub fn insert_id(text: &str, array: &Array, index: usize, id: &str) -> Inserted {
    let index = index.min(array.elements.len());
    let nl = newline(text);
    let unit = indent_unit(text);
    let quoted = serde_json::to_string(id).unwrap_or_else(|_| format!("\"{id}\""));
    let bare_string = !array.elements.is_empty()
        && array
            .elements
            .iter()
            .all(|e| text.get(e.clone()).is_some_and(|t| t.starts_with('"')));
    let multiline = match array.elements.first() {
        Some(first) => text
            .get(array.open + 1..first.start)
            .is_some_and(|gap| gap.contains('\n')),
        // nothing inside to look at: a file that has lines has them here too
        None => text
            .get(..array.open)
            .is_some_and(|before| before.contains('\n')),
    };
    let element = |indent: &str| -> String {
        if bare_string {
            quoted.clone()
        } else if multiline {
            format!("{{{nl}{indent}{unit}\"id\": {quoted}{nl}{indent}}}")
        } else {
            format!("{{ \"id\": {quoted} }}")
        }
    };

    let edited = match (array.elements.get(index), array.elements.last()) {
        // an empty array
        (None, None) => {
            if multiline {
                let base = indent_of_line(text, array.open);
                let inner = format!("{base}{unit}");
                let body = format!("{nl}{inner}{}{nl}{base}", element(&inner));
                splice(text, array.open + 1..array.close, &body)
            } else {
                splice(text, array.open + 1..array.close, &element(""))
            }
        }
        // before an element
        (Some(before), _) => {
            if multiline {
                let indent = indent_of_line(text, before.start);
                let added = format!("{},{nl}{indent}", element(&indent));
                splice(text, before.start..before.start, &added)
            } else {
                let added = format!("{}{}", element(""), separator(text, array));
                splice(text, before.start..before.start, &added)
            }
        }
        // after the last one
        (None, Some(last)) => {
            if multiline {
                let indent = indent_of_line(text, last.start);
                let added = format!(",{nl}{indent}{}", element(&indent));
                splice(text, last.end..last.end, &added)
            } else {
                let added = format!("{}{}", separator(text, array), element(""));
                splice(text, last.end..last.end, &added)
            }
        }
    };
    Inserted {
        text: edited,
        bare_string,
    }
}

/// `text` without element `index`: the element and one separator go, so that the neighbours keep
/// the whitespace they had. The only element empties the brackets.
pub fn remove(text: &str, array: &Array, index: usize) -> Option<String> {
    let n = array.elements.len();
    let element = array.elements.get(index)?;
    let gone = if n == 1 {
        array.open + 1..array.close
    } else if let Some(next) = array.elements.get(index + 1) {
        element.start..next.start
    } else {
        array.elements.get(index.checked_sub(1)?)?.end..element.end
    };
    Some(splice(text, gone, ""))
}

fn splice(text: &str, range: Range<usize>, with: &str) -> String {
    let mut out = String::with_capacity(text.len() + with.len());
    out.push_str(text.get(..range.start).unwrap_or_default());
    out.push_str(with);
    out.push_str(text.get(range.end..).unwrap_or_default());
    out
}

/// The line ending the file uses.
fn newline(text: &str) -> &'static str {
    if text.contains("\r\n") { "\r\n" } else { "\n" }
}

/// One level of indentation, as the file shows it: the first indented line. Two spaces when
/// there is none (the shell's own).
fn indent_unit(text: &str) -> String {
    for line in text.lines().skip(1) {
        let indent: String = line
            .chars()
            .take_while(|c| *c == ' ' || *c == '\t')
            .collect();
        if indent.is_empty() || indent.len() == line.len() {
            continue;
        }
        return if indent.starts_with('\t') {
            "\t".into()
        } else {
            indent
        };
    }
    "  ".into()
}

/// The whitespace the line holding byte `at` starts with.
fn indent_of_line(text: &str, at: usize) -> String {
    let before = text.get(..at).unwrap_or_default();
    let start = before.rfind('\n').map_or(0, |i| i + 1);
    text.get(start..)
        .unwrap_or_default()
        .chars()
        .take_while(|c| *c == ' ' || *c == '\t')
        .collect()
}

/// What sits between two elements of a one-line array (`, ` when there is only one).
fn separator(text: &str, array: &Array) -> String {
    match (array.elements.first(), array.elements.get(1)) {
        (Some(a), Some(b)) => text.get(a.end..b.start).unwrap_or(", ").to_string(),
        _ => ", ".to_string(),
    }
}

/// The key at `span` (quotes included) is `wanted`, escapes decoded.
fn key_is(text: &str, span: &Range<usize>, wanted: &str) -> bool {
    text.get(span.clone())
        .and_then(|raw| serde_json::from_str::<String>(raw).ok())
        .is_some_and(|key| key == wanted)
}

/// A cursor over the bytes of the text that knows where JSON values start and end. It follows
/// the structure and never decodes a value, and it nests without recursion, so no input makes
/// it overflow the stack.
struct Scan<'a> {
    b: &'a [u8],
    i: usize,
}

impl<'a> Scan<'a> {
    fn at(text: &'a str, i: usize) -> Self {
        Self {
            b: text.as_bytes(),
            i,
        }
    }

    fn peek(&self) -> Option<u8> {
        self.b.get(self.i).copied()
    }

    fn whitespace(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.i += 1;
        }
    }

    /// Past the closing quote of the string that starts here.
    fn string(&mut self) -> Option<()> {
        if self.peek()? != b'"' {
            return None;
        }
        self.i += 1;
        loop {
            match self.peek()? {
                b'\\' => self.i += 2,
                b'"' => {
                    self.i += 1;
                    return Some(());
                }
                _ => self.i += 1,
            }
        }
    }

    /// The span of the value that starts at the next non-blank byte, and past it.
    fn value(&mut self) -> Option<Range<usize>> {
        self.whitespace();
        let start = self.i;
        match self.peek()? {
            b'"' => self.string()?,
            b'{' | b'[' => {
                let mut depth = 0usize;
                loop {
                    match self.peek()? {
                        b'"' => {
                            self.string()?;
                            continue;
                        }
                        b'{' | b'[' => depth += 1,
                        b'}' | b']' => depth = depth.checked_sub(1)?,
                        _ => {}
                    }
                    self.i += 1;
                    if depth == 0 {
                        break;
                    }
                }
            }
            _ => {
                // a number, true, false or null: up to whatever ends it
                while let Some(c) = self.peek() {
                    if matches!(c, b',' | b']' | b'}' | b' ' | b'\t' | b'\n' | b'\r') {
                        break;
                    }
                    self.i += 1;
                }
                if self.i == start {
                    return None;
                }
            }
        }
        Some(start..self.i)
    }

    /// The members of the object that starts here: each key (quotes included) and value span.
    fn members(&mut self) -> Option<Vec<(Range<usize>, Range<usize>)>> {
        self.whitespace();
        if self.peek()? != b'{' {
            return None;
        }
        self.i += 1;
        let mut out = Vec::new();
        loop {
            self.whitespace();
            match self.peek()? {
                b'}' => {
                    self.i += 1;
                    return Some(out);
                }
                b'"' => {
                    let from = self.i;
                    self.string()?;
                    let key = from..self.i;
                    self.whitespace();
                    if self.peek()? != b':' {
                        return None;
                    }
                    self.i += 1;
                    out.push((key, self.value()?));
                    self.whitespace();
                    match self.peek()? {
                        b',' => self.i += 1,
                        b'}' => {}
                        _ => return None,
                    }
                }
                _ => return None,
            }
        }
    }

    /// The elements of the array that starts here.
    fn elements(&mut self) -> Option<Vec<Range<usize>>> {
        self.whitespace();
        if self.peek()? != b'[' {
            return None;
        }
        self.i += 1;
        let mut out = Vec::new();
        loop {
            self.whitespace();
            if self.peek()? == b']' {
                self.i += 1;
                return Some(out);
            }
            out.push(self.value()?);
            self.whitespace();
            match self.peek()? {
                b',' => self.i += 1,
                b']' => {}
                _ => return None,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use serde_json::{Value, json};

    const PATH: [&str; 3] = ["bar", "layout", "right"];

    fn array(text: &str) -> Array {
        array_at(text, &PATH).expect("the array is found")
    }

    fn parse(text: &str) -> Value {
        serde_json::from_str(text).unwrap_or_else(|e| panic!("{e}: {text}"))
    }

    const PRETTY: &str = r#"{
  "version": 1,
  "bar": {
    "id": "my.bar",
    "layout": {
      "left": [],
      "right": [
        {
          "id": "a"
        },
        {
          "id": "b",
          "n": [1, 2]
        }
      ]
    }
  }
}
"#;

    #[test]
    fn the_array_is_found_by_its_path_with_the_span_of_each_element() {
        let a = array(PRETTY);
        assert_eq!(a.elements.len(), 2);
        assert_eq!(&PRETTY[a.open..=a.open], "[");
        assert_eq!(&PRETTY[a.close..=a.close], "]");
        assert_eq!(
            &PRETTY[a.elements[0].clone()],
            "{\n          \"id\": \"a\"\n        }"
        );
        let left = array_at(PRETTY, &["bar", "layout", "left"]).unwrap();
        assert!(left.elements.is_empty());
        assert_eq!(&PRETTY[left.open..=left.close], "[]");
    }

    #[test]
    fn a_path_that_leads_nowhere_is_none() {
        assert!(array_at(PRETTY, &["bar", "layout", "tray"]).is_none());
        assert!(array_at(PRETTY, &["bar", "id"]).is_none(), "a string");
        assert!(array_at(PRETTY, &["version"]).is_none(), "a number");
        assert!(array_at("[]", &PATH).is_none(), "the root is no object");
        assert!(array_at("", &PATH).is_none());
        assert!(array_at("{\"bar\": {\"layout\": {\"right\": [", &PATH).is_none());
    }

    #[test]
    fn a_key_that_appears_twice_is_ambiguous() {
        let t = r#"{"bar":{"layout":{"right":["a"],"right":["b"]}}}"#;
        assert!(array_at(t, &PATH).is_none());
    }

    #[test]
    fn brackets_and_quotes_inside_strings_do_not_confuse_the_scan() {
        let t = r#"{"bar":{"layout":{"right":["]\"[", {"id":"}{","x":"\\"}, "a,b"]}}}"#;
        let a = array(t);
        assert_eq!(a.elements.len(), 3);
        assert_eq!(&t[a.elements[0].clone()], r#""]\"[""#);
        assert_eq!(&t[a.elements[2].clone()], r#""a,b""#);
        parse(&remove(t, &a, 1).unwrap());
    }

    #[test]
    fn a_key_with_an_escape_is_found_by_what_it_says() {
        let t = r#"{"bar":{"layout":{"right":[1]}}}"#;
        assert_eq!(array(t).elements.len(), 1);
    }

    fn inserted(text: &str, at: usize) -> String {
        insert_id(text, &array(text), at, "ferry.bar").text
    }

    #[test]
    fn an_object_is_added_on_its_own_lines_in_a_file_that_has_them() {
        let t = inserted(PRETTY, 1);
        assert_eq!(
            t,
            PRETTY.replace(
                "        {\n          \"id\": \"b\",",
                "        {\n          \"id\": \"ferry.bar\"\n        },\n        {\n          \"id\": \"b\","
            )
        );
        let appended = inserted(PRETTY, 2);
        assert!(
            appended.contains(
                "        },\n        {\n          \"id\": \"ferry.bar\"\n        }\n      ]"
            ),
            "{appended}"
        );
        assert_eq!(
            parse(&appended)["bar"]["layout"]["right"][2]["id"],
            "ferry.bar"
        );
        let first = inserted(PRETTY, 0);
        assert_eq!(
            parse(&first)["bar"]["layout"]["right"][0]["id"],
            "ferry.bar"
        );
        assert_eq!(parse(&first)["bar"]["layout"]["right"][1]["id"], "a");
    }

    #[test]
    fn an_empty_array_becomes_a_multi_line_array_in_a_file_with_lines() {
        let empty = PRETTY.replace(
            "\"right\": [\n        {\n          \"id\": \"a\"\n        },\n        {\n          \"id\": \"b\",\n          \"n\": [1, 2]\n        }\n      ]",
            "\"right\": []",
        );
        let a = array(&empty);
        assert!(a.elements.is_empty());
        let t = insert_id(&empty, &a, 0, "ferry.bar").text;
        assert!(
            t.contains(
                "\"right\": [\n        {\n          \"id\": \"ferry.bar\"\n        }\n      ]"
            ),
            "{t}"
        );
        assert_eq!(
            parse(&t)["bar"]["layout"]["right"],
            json!([{"id": "ferry.bar"}])
        );
    }

    #[test]
    fn a_one_line_file_gets_a_one_line_element() {
        let t = r#"{"bar":{"layout":{"right":[{"id":"a"},{"id":"b"}]}}}"#;
        let out = inserted(t, 1);
        assert_eq!(
            out,
            r#"{"bar":{"layout":{"right":[{"id":"a"},{ "id": "ferry.bar" },{"id":"b"}]}}}"#
        );
        let spaced = r#"{"bar":{"layout":{"right":["a", "b"]}}}"#;
        assert_eq!(
            inserted(spaced, 2),
            r#"{"bar":{"layout":{"right":["a", "b", "ferry.bar"]}}}"#,
            "strings among strings, and the separator the file uses"
        );
        let empty = r#"{"bar":{"layout":{"right":[]}}}"#;
        assert_eq!(
            inserted(empty, 0),
            r#"{"bar":{"layout":{"right":[{ "id": "ferry.bar" }]}}}"#
        );
    }

    #[test]
    fn strings_among_strings_stay_strings_in_a_file_with_lines() {
        let t = "{\n  \"bar\": {\n    \"layout\": {\n      \"right\": [\n        \"a\",\n        \"b\"\n      ]\n    }\n  }\n}\n";
        let out = insert_id(t, &array(t), 1, "ferry.bar");
        assert!(out.bare_string);
        assert_eq!(
            out.text,
            t.replace("\"a\",\n", "\"a\",\n        \"ferry.bar\",\n")
        );
    }

    #[test]
    fn the_indentation_is_the_files_own() {
        let four = PRETTY.replace("  ", "    ");
        let t = inserted(&four, 1);
        assert_eq!(parse(&t), {
            let mut v = parse(PRETTY);
            v["bar"]["layout"]["right"]
                .as_array_mut()
                .unwrap()
                .insert(1, json!({"id": "ferry.bar"}));
            v
        });
        assert!(
            t.contains("\n                    \"id\": \"ferry.bar\"\n                },\n"),
            "{t}"
        );
        let tabs = PRETTY.replace("  ", "\t");
        let t = inserted(&tabs, 2);
        assert!(
            t.contains("\n\t\t\t\t\t\"id\": \"ferry.bar\"\n\t\t\t\t}"),
            "{t}"
        );
    }

    #[test]
    fn crlf_files_keep_their_line_ending() {
        let crlf = PRETTY.replace('\n', "\r\n");
        let t = inserted(&crlf, 1);
        assert_eq!(t.matches("\r\n").count(), t.matches('\n').count());
        parse(&t);
    }

    #[test]
    fn removal_takes_the_element_and_one_separator() {
        let with = inserted(PRETTY, 1);
        let a = array(&with);
        assert_eq!(remove(&with, &a, 1).unwrap(), PRETTY, "the middle");
        let appended = inserted(PRETTY, 2);
        let a = array(&appended);
        assert_eq!(remove(&appended, &a, 2).unwrap(), PRETTY, "the last");
        let first = inserted(PRETTY, 0);
        let a = array(&first);
        assert_eq!(remove(&first, &a, 0).unwrap(), PRETTY, "the first");
        let only = r#"{"bar":{"layout":{"right":[ "x" ]}}}"#;
        assert_eq!(
            remove(only, &array(only), 0).unwrap(),
            r#"{"bar":{"layout":{"right":[]}}}"#,
            "the only one empties the brackets"
        );
        assert!(remove(only, &array(only), 1).is_none());
    }

    // ------------------------------------------------------------------------ properties

    /// A JSON value that looks like a widget entry: a string, or an object with an id and some
    /// odd company (brackets and quotes in strings, nested arrays and objects).
    fn element() -> impl Strategy<Value = Value> {
        let text = "[ -~\u{e9}\u{4e2d}]{0,8}";
        prop_oneof![
            text.prop_map(Value::from),
            (text, text, 0i64..5, any::<bool>()).prop_map(|(id, extra, n, flag)| {
                json!({ "id": id, "extra": extra, "list": [n, [], {"k": [flag]}], "empty": {} })
            }),
        ]
    }

    fn document(right: Vec<Value>) -> Value {
        json!({
            "version": 1,
            "pre": [1, {"a": "]"}],
            "bar": { "layout": { "left": [], "right": right, "tray": ["t"] }, "id": "b" },
            "post": ["\\", "}"]
        })
    }

    /// The same value written five ways: one line, two or four spaces, tabs, CRLF.
    fn writings(v: &Value) -> Vec<String> {
        let indented = |unit: &[u8]| {
            let mut buf = Vec::new();
            let fmt = serde_json::ser::PrettyFormatter::with_indent(unit);
            let mut ser = serde_json::Serializer::with_formatter(&mut buf, fmt);
            serde::Serialize::serialize(v, &mut ser).unwrap();
            String::from_utf8(buf).unwrap()
        };
        let two = indented(b"  ");
        vec![
            serde_json::to_string(v).unwrap(),
            two.clone() + "\n",
            indented(b"    "),
            indented(b"\t") + "\n",
            two.replace('\n', "\r\n"),
        ]
    }

    /// `new` is `old` with one run of bytes put in (`grew`) or taken out: they share a prefix and a
    /// suffix that together cover all of the shorter one.
    fn differs_by_one_run(old: &str, new: &str) -> bool {
        let (short, long) = if old.len() <= new.len() {
            (old, new)
        } else {
            (new, old)
        };
        let p = short
            .bytes()
            .zip(long.bytes())
            .take_while(|(a, b)| a == b)
            .count();
        let s = short
            .bytes()
            .rev()
            .zip(long.bytes().rev())
            .take_while(|(a, b)| a == b)
            .count();
        p + s >= short.len()
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(512))]

        /// An insertion at any index parses to the old value plus the element, changes one run of
        /// bytes, and a removal of that element gives the old bytes back.
        #[test]
        fn inserting_and_removing_at_every_index(right in proptest::collection::vec(element(), 0..6)) {
            for old in writings(&document(right.clone())) {
                let a = array(&old);
                prop_assert_eq!(a.elements.len(), right.len());
                for index in 0..=right.len() {
                    let ins = insert_id(&old, &a, index, "ferry.bar");
                    let mut expected = document(right.clone());
                    let widget = if ins.bare_string { json!("ferry.bar") } else { json!({"id": "ferry.bar"}) };
                    if let Some(list) = expected["bar"]["layout"]["right"].as_array_mut() {
                        list.insert(index, widget);
                    }
                    prop_assert_eq!(parse(&ins.text), expected, "insert at {} into {}", index, old);
                    prop_assert!(differs_by_one_run(&old, &ins.text), "{}", ins.text);

                    let again = array(&ins.text);
                    prop_assert_eq!(again.elements.len(), right.len() + 1);
                    let back = remove(&ins.text, &again, index).unwrap();
                    prop_assert_eq!(&back, &old, "removing it again");
                }
                // every element can go, whatever it is
                for index in 0..right.len() {
                    let gone = remove(&old, &a, index).unwrap();
                    let mut expected = document(right.clone());
                    if let Some(list) = expected["bar"]["layout"]["right"].as_array_mut() {
                        list.remove(index);
                    }
                    prop_assert_eq!(parse(&gone), expected);
                    prop_assert!(differs_by_one_run(&old, &gone));
                }
            }
        }

        /// Whatever the text, the scan and the edits do not panic.
        #[test]
        fn nothing_panics(text in "\\PC{0,200}", at in 0usize..8, id in "[a-z.]{0,6}") {
            let _ = array_at(&text, &PATH);
            let fake = Array {
                open: 0,
                close: text.len().saturating_sub(1),
                elements: std::iter::once(0..text.len().min(3)).collect(),
            };
            let _ = insert_id(&text, &fake, at, &id);
            let _ = remove(&text, &fake, at);
        }
    }
}
