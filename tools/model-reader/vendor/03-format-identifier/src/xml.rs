//! Minimal XML well-formedness check: UTF-8, balanced and properly nested
//! elements, a single root, quoted attribute values, and only comments,
//! processing instructions or whitespace outside the root. Entities and DTDs
//! are not validated.

/// Returns the root element name when the document is well formed.
pub fn well_formed(bytes: &[u8]) -> Result<String, String> {
    let bytes = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes);
    let text = std::str::from_utf8(bytes).map_err(|e| format!("invalid UTF-8 at byte {}", e.valid_up_to()))?;
    let text = text.trim_end_matches('\0');
    let mut stack: Vec<&str> = Vec::new();
    let mut root: Option<String> = None;
    let mut closed_root = false;
    let mut rest = text;
    let mut pos = 0usize;
    let advance = |rest: &mut &str, pos: &mut usize, n: usize| {
        *rest = &rest[n..];
        *pos += n;
    };
    loop {
        let Some(lt) = rest.find('<') else {
            if !rest.trim().is_empty() && stack.is_empty() {
                return Err(format!("text outside the root at byte {pos}"));
            }
            break;
        };
        if stack.is_empty() && !rest[..lt].trim().is_empty() {
            return Err(format!("text outside the root at byte {pos}"));
        }
        advance(&mut rest, &mut pos, lt);
        let (end_marker, kind) = if rest.starts_with("<?") {
            ("?>", "pi")
        } else if rest.starts_with("<!--") {
            ("-->", "comment")
        } else if rest.starts_with("<![CDATA[") {
            ("]]>", "cdata")
        } else if rest.starts_with("<!") {
            (">", "decl")
        } else {
            ("", "tag")
        };
        if kind != "tag" {
            if kind == "cdata" && stack.is_empty() {
                return Err(format!("CDATA outside the root at byte {pos}"));
            }
            let end = rest.find(end_marker).ok_or_else(|| format!("unterminated {kind} at byte {pos}"))?;
            advance(&mut rest, &mut pos, end + end_marker.len());
            continue;
        }
        // Find the closing '>' outside quoted attribute values.
        let mut quote = None;
        let mut end = None;
        for (i, c) in rest.char_indices().skip(1) {
            match (quote, c) {
                (Some(q), c) if c == q => quote = None,
                (Some(_), _) => {}
                (None, '"' | '\'') => quote = Some(c),
                (None, '<') => return Err(format!("'<' inside a tag at byte {}", pos + i)),
                (None, '>') => {
                    end = Some(i);
                    break;
                }
                _ => {}
            }
        }
        let end = end.ok_or_else(|| format!("unterminated tag at byte {pos}"))?;
        let inner = &rest[1..end];
        if let Some(name) = inner.strip_prefix('/') {
            let name = name.trim_end();
            match stack.pop() {
                Some(open) if open == name => {
                    if stack.is_empty() {
                        closed_root = true;
                    }
                }
                Some(open) => return Err(format!("</{name}> closes <{open}> at byte {pos}")),
                None => return Err(format!("</{name}> without an open element at byte {pos}")),
            }
        } else {
            let self_closing = inner.ends_with('/');
            let body = inner.strip_suffix('/').unwrap_or(inner);
            let name = body.split(|c: char| c.is_whitespace()).next().unwrap_or("");
            if name.is_empty() || !name.chars().all(|c| c.is_alphanumeric() || "_-.:".contains(c)) {
                return Err(format!("invalid element name at byte {pos}"));
            }
            if stack.is_empty() {
                if closed_root || root.is_some() && self_closing {
                    return Err(format!("second root element <{name}> at byte {pos}"));
                }
                root.get_or_insert_with(|| name.to_string());
                if self_closing {
                    closed_root = true;
                }
            }
            if !self_closing {
                stack.push(name);
            }
        }
        advance(&mut rest, &mut pos, end + 1);
    }
    if let Some(open) = stack.last() {
        return Err(format!("<{open}> not closed"));
    }
    match (root, closed_root) {
        (Some(root), true) => Ok(root),
        _ => Err("no root element".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::well_formed;

    #[test]
    fn accepts_nested_document() {
        let doc = b"\xef\xbb\xbf<?xml version=\"1.0\"?>\n<!-- c --><a x=\"1>2\"><b/><c>t</c></a>\n";
        assert_eq!(well_formed(doc).unwrap(), "a");
    }

    #[test]
    fn rejects_bad_nesting_and_truncation() {
        assert!(well_formed(b"<a><b></a></b>").is_err());
        assert!(well_formed(b"<a><b>").is_err());
        assert!(well_formed(b"<a/><b/>").is_err());
        assert!(well_formed(b"<?xml version=\"1.0\"?>").is_err());
    }
}
