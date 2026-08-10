//! The template engine: `{var}` substitution and non-nesting
//! `[[if var]]…[[else]]…[[end]]` blocks. Sixty lines instead of a dependency.

use std::collections::HashMap;

pub(crate) fn render_template(tpl: &str, vars: &HashMap<&'static str, String>) -> String {
    expand_placeholders(&expand_conditionals(tpl, vars), vars)
}

/// Expand `[[if var]]…[[else]]…[[end]]`. The condition is "the variable is
/// non-empty". Nested blocks are not supported: the first `[[end]]` closes the
/// block. An unclosed block is left in the text as is — that way a broken
/// template stays visible.
fn expand_conditionals(tpl: &str, vars: &HashMap<&'static str, String>) -> String {
    let mut out = String::with_capacity(tpl.len());
    let mut rest = tpl;
    while let Some(start) = rest.find("[[if ") {
        let after = &rest[start + "[[if ".len()..];
        let Some(name_end) = after.find("]]") else {
            break;
        };
        let body = &after[name_end + 2..];
        let Some(end) = body.find("[[end]]") else {
            break;
        };
        let name = after[..name_end].trim();
        let (then_part, else_part) = match body[..end].find("[[else]]") {
            Some(i) => (&body[..i], &body[i + "[[else]]".len()..end]),
            None => (&body[..end], ""),
        };
        let truthy = vars.get(name).is_some_and(|v| !v.is_empty());
        out.push_str(&rest[..start]);
        out.push_str(if truthy { then_part } else { else_part });
        rest = &body[end + "[[end]]".len()..];
    }
    out.push_str(rest);
    out
}

/// Substitute `{var}`. Unknown names are left as is — otherwise a typo in a
/// template would silently swallow a chunk of the prompt.
fn expand_placeholders(tpl: &str, vars: &HashMap<&'static str, String>) -> String {
    let mut out = String::with_capacity(tpl.len());
    let mut rest = tpl;
    while let Some(start) = rest.find('{') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let Some(end) = after.find('}') else {
            rest = &rest[start..];
            break;
        };
        let name = &after[..end];
        match vars.get(name) {
            Some(v) => out.push_str(v),
            None => out.push_str(&rest[start..start + end + 2]),
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars(pairs: &[(&'static str, &str)]) -> HashMap<&'static str, String> {
        pairs.iter().map(|(k, v)| (*k, v.to_string())).collect()
    }

    #[test]
    fn placeholders_are_substituted() {
        let v = vars(&[("iid", "42"), ("title", "Add widget")]);
        assert_eq!(
            expand_placeholders("MR !{iid}: {title}", &v),
            "MR !42: Add widget"
        );
    }

    #[test]
    fn unknown_placeholder_is_left_as_is() {
        let v = vars(&[("iid", "42")]);
        assert_eq!(expand_placeholders("{iid} {nope} {", &v), "42 {nope} {");
    }

    #[test]
    fn conditional_picks_branch_by_emptiness() {
        let filled = vars(&[("threads", "x")]);
        let empty = vars(&[("threads", "")]);
        let tpl = "a[[if threads]]YES[[else]]NO[[end]]b";
        assert_eq!(expand_conditionals(tpl, &filled), "aYESb");
        assert_eq!(expand_conditionals(tpl, &empty), "aNOb");
    }

    #[test]
    fn conditional_without_else_drops_block() {
        let empty: HashMap<&'static str, String> = HashMap::new();
        assert_eq!(expand_conditionals("a[[if t]]YES[[end]]b", &empty), "ab");
    }

    #[test]
    fn several_conditionals_in_one_template() {
        let v = vars(&[("a", "1"), ("b", "")]);
        let tpl = "[[if a]]A[[end]]-[[if b]]B[[else]]nb[[end]]";
        assert_eq!(expand_conditionals(tpl, &v), "A-nb");
    }

    #[test]
    fn unclosed_conditional_stays_visible() {
        let v = vars(&[("a", "1")]);
        assert_eq!(expand_conditionals("x[[if a]]y", &v), "x[[if a]]y");
    }
}
