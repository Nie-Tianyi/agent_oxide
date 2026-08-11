#![deny(unsafe_code)]
//! Proc macros for the [`tools`] crate.
//!
//! Provides the [`tool`] attribute macro that generates `Tool` trait
//! implementations from a struct definition and attribute parameters.

use proc_macro::TokenStream;
use quote::quote;
use syn::{
    Ident, ItemStruct, LitStr, Type,
    parse::{Parse, ParseStream},
    parse_macro_input,
};

// ── Attribute arguments ────────────────────────────────────────────────────

/// Parsed form of `#[tool(name = "...", description = "...", args = Type)]`.
struct ToolArgs {
    name: LitStr,
    description: LitStr,
    args: Type,
}

impl Parse for ToolArgs {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let mut name: Option<LitStr> = None;
        let mut description: Option<LitStr> = None;
        let mut args: Option<Type> = None;

        while !input.is_empty() {
            let key: Ident = input.parse()?;
            input.parse::<syn::Token![=]>()?;

            match key.to_string().as_str() {
                "name" => {
                    let val: LitStr = input.parse()?;
                    set_or_duplicate(&mut name, val, "name")?;
                }
                "description" => {
                    let val: LitStr = input.parse()?;
                    set_or_duplicate(&mut description, val, "description")?;
                }
                "args" => {
                    let val: Type = input.parse()?;
                    set_or_duplicate(&mut args, val, "args")?;
                }
                other => {
                    return Err(syn::Error::new_spanned(
                        &key,
                        format!(
                            "unknown key `{other}` — expected `name`, `description`, or `args`"
                        ),
                    ));
                }
            }

            // Consume optional trailing comma
            let _ = input.parse::<syn::Token![,]>();
        }

        let name =
            name.ok_or_else(|| syn::Error::new(input.span(), "missing required key `name`"))?;
        let description = description
            .ok_or_else(|| syn::Error::new(input.span(), "missing required key `description`"))?;
        let args =
            args.ok_or_else(|| syn::Error::new(input.span(), "missing required key `args`"))?;

        Ok(ToolArgs {
            name,
            description,
            args,
        })
    }
}

fn set_or_duplicate<T>(slot: &mut Option<T>, val: T, label: &str) -> syn::Result<()> {
    if slot.is_some() {
        Err(syn::Error::new(
            proc_macro2::Span::call_site(),
            format!("duplicate key `{label}`"),
        ))
    } else {
        *slot = Some(val);
        Ok(())
    }
}

// ── The macro ──────────────────────────────────────────────────────────────

/// Generate a [`Tool`](tools::Tool) trait implementation for a struct.
///
/// # Parameters
///
/// | Key | Value | Required |
/// |---|---|---|
/// | `name` | string literal — tool name sent to the LLM | yes |
/// | `description` | string literal — human-readable description | yes |
/// | `args` | type — the `Deserialize + JsonSchema` arguments struct | yes |
///
/// # What it generates
///
/// - `Tool::name()` returns `name`
/// - `Tool::description()` returns `description`
/// - `Tool::parameter_schema()` lazily generates JSON Schema from `args` (cached via `OnceLock`)
/// - `Tool::execute_stream()` deserializes JSON into `args` and delegates to an inherent
///   `fn execute_stream(&self, args: ArgsType) -> Result<ProgressStream, ToolError>` method
///
/// # Example
///
/// ```ignore
/// #[derive(JsonSchema, Deserialize)]
/// #[serde(deny_unknown_fields)]
/// struct EchoArgs {
///     #[schemars(description = "The text to echo back.")]
///     pub text: String,
/// }
///
/// #[tool(
///     name = "echo",
///     description = "Echo the input text back unchanged.",
///     args = EchoArgs
/// )]
/// pub struct EchoTool;
///
/// impl EchoTool {
///     fn execute_stream(&self, args: EchoArgs) -> Result<ProgressStream, ToolError> {
///         Ok(Box::pin(futures_util::stream::once(async { Progress::Done(args.text) })))
///     }
/// }
/// ```
#[proc_macro_attribute]
pub fn tool(attr: TokenStream, item: TokenStream) -> TokenStream {
    let args = parse_macro_input!(attr as ToolArgs);
    let input = parse_macro_input!(item as ItemStruct);

    let ToolArgs {
        name,
        description,
        args: args_type,
    } = args;

    let struct_name = &input.ident;
    let (impl_generics, ty_generics, where_clause) = input.generics.split_for_impl();

    let expanded = quote! {
        #input

        impl #impl_generics ::tools::Tool for #struct_name #ty_generics #where_clause {
            fn name(&self) -> &str {
                #name
            }

            fn description(&self) -> &str {
                #description
            }

            fn parameter_schema(&self) -> ::serde_json::Value {
                static SCHEMA: ::std::sync::OnceLock<::serde_json::Value> =
                    ::std::sync::OnceLock::new();
                SCHEMA
                    .get_or_init(|| ::tools::generate_schema::<#args_type>())
                    .clone()
            }

            fn execute_stream(
                &self,
                raw_args: &str,
            ) -> ::std::result::Result<::tools::ProgressStream, ::tools::ToolError> {
                let args: #args_type = ::serde_json::from_str(raw_args)
                    .map_err(|e| ::tools::ToolError::InvalidArgs(
                        format!("invalid args: {e}")
                    ))?;
                #struct_name::execute_stream(self, args)
            }
        }
    };

    TokenStream::from(expanded)
}
