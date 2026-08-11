//! Shared parsing helpers for the `Agent` derive and `#[agent_impl]` macros.

use syn::{
    Attribute, Expr, ExprLit, Ident, Lit, LitStr, Meta,
    parse::{Parse, ParseStream},
};

/// Extract the doc comment (`/// ...` / `#[doc = "..."]`) from a list of
/// attributes, joined with newlines.
pub(crate) fn doc_comment(attrs: &[Attribute]) -> String {
    let mut parts = Vec::new();
    for attr in attrs {
        if !attr.path().is_ident("doc") {
            continue;
        }
        if let Meta::NameValue(nv) = &attr.meta
            && let Expr::Lit(ExprLit {
                lit: Lit::Str(s), ..
            }) = &nv.value
        {
            parts.push(s.value());
        }
    }
    parts.join("\n")
}

/// Remove the helper attributes this crate defines (`agent`, `tool`,
/// `context`, `strategy`) from a list of attributes — the compiler would
/// otherwise reject them as unknown attributes on the re-emitted items.
pub(crate) fn strip_helpers(attrs: Vec<Attribute>) -> Vec<Attribute> {
    attrs
        .into_iter()
        .filter(|a| {
            !(a.path().is_ident("agent")
                || a.path().is_ident("tool")
                || a.path().is_ident("context")
                || a.path().is_ident("strategy"))
        })
        .collect()
}

/// Whether `#[agent(skip)]` is present (excludes a field/method from
/// tool registration / client auto-detection).
pub(crate) fn is_skip(attrs: &[Attribute]) -> bool {
    attrs.iter().any(|a| {
        a.path().is_ident("agent")
            && matches!(&a.meta, Meta::List(l) if l.parse_args::<Ident>().is_ok_and(|i| i == "skip"))
    })
}

/// Parsed form of `#[tool(name = "...")]` — an optional tool-name override.
pub(crate) struct ToolAttr {
    pub name: Option<LitStr>,
}

impl Parse for ToolAttr {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let mut name: Option<LitStr> = None;
        while !input.is_empty() {
            let key: Ident = input.parse()?;
            input.parse::<syn::Token![=]>()?;
            match key.to_string().as_str() {
                "name" => {
                    if name.is_some() {
                        return Err(syn::Error::new(input.span(), "duplicate key `name`"));
                    }
                    name = Some(input.parse()?);
                }
                other => {
                    return Err(syn::Error::new_spanned(
                        &key,
                        format!("unknown key `{other}` — expected `name`"),
                    ));
                }
            }
            let _ = input.parse::<syn::Token![,]>();
        }
        Ok(Self { name })
    }
}

/// The tool name for an item annotated `#[tool(name = "...")]`, if any.
pub(crate) fn parse_tool_name(attrs: &[Attribute]) -> syn::Result<Option<LitStr>> {
    for attr in attrs {
        if !attr.path().is_ident("tool") {
            continue;
        }
        match &attr.meta {
            Meta::Path(_) => return Ok(None),
            Meta::List(l) => return l.parse_args::<ToolAttr>().map(|a| a.name),
            Meta::NameValue(_) => {
                return Err(syn::Error::new_spanned(
                    attr,
                    "expected `#[tool]` or `#[tool(name = \"...\")]`",
                ));
            }
        }
    }
    Ok(None)
}
