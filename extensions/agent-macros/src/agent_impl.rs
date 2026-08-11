//! `#[agent_impl]` — process an agent's method block.
//!
//! Given:
//!
//! ```ignore
//! #[agent_impl]
//! impl FeedbackAgent {
//!     /// Analyze customer feedback for sentiment and key topics in one sentence.
//!     async fn analyze_feedback(&self, text: String) -> String {}
//!
//!     /// Get the current stock level of an item.
//!     fn get_stock(&self, item: String) -> i32 {
//!         self.inventory.get(&item).map(|i| i.stock).unwrap_or(0)
//!     }
//! }
//! ```
//!
//! generates:
//!
//! 1. **Synchronous methods → tools.** Each `fn` becomes a `Tool` adapter
//!    (name = method name, description = doc comment, JSON Schema derived
//!    from the parameter list) and an auto-derived `__AgentArgs_*` struct.
//!    The original method is preserved so Rust code can still call it.
//! 2. **Async methods with an empty body → generation methods.** The body
//!    is replaced with an LLM call via `agent_kit::run_generation`, and the
//!    return type is wrapped: `-> T` becomes
//!    `-> Result<T, agent_kit::GenerationError>`.
//! 3. An `agent_kit::AgentBlueprint` impl covering the *method* half
//!    (tool registration) — the counterpart of the `#[derive(Agent)]`
//!    field half. The two macros must be used together.

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::{
    Attribute, Block, FnArg, Ident, ImplItem, ImplItemFn, ItemImpl, LitInt, LitStr, Meta, Pat,
    PathArguments, ReturnType, Type,
    parse::{Parse, ParseStream},
    parse_macro_input,
};

use crate::util::{doc_comment, is_skip, parse_tool_name, strip_helpers};

pub fn expand(attr: TokenStream, item: TokenStream) -> TokenStream {
    if !attr.is_empty() {
        return syn::Error::new(
            proc_macro2::Span::call_site(),
            "#[agent_impl] takes no arguments",
        )
        .to_compile_error()
        .into();
    }
    let input = parse_macro_input!(item as ItemImpl);
    expand_impl(input)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

// ── Strategy attribute ────────────────────────────────────────────────────────

/// The execution strategy for a generation method, mirroring
/// `#[strategy(PredictStrategy())]` / `#[strategy(CodeActStrategy(...))]`.
enum Strategy {
    Predict {
        max_retries: usize,
    },
    CodeAct {
        max_iterations: usize,
        max_retries: usize,
    },
}

impl Default for Strategy {
    fn default() -> Self {
        Self::CodeAct {
            max_iterations: 50,
            max_retries: 2,
        }
    }
}

/// Parsed form of `#[strategy(predict)]` /
/// `#[strategy(code_act, max_iterations = 10)]`.
struct StrategyArgs {
    kind: Ident,
    kv: Vec<(Ident, LitInt)>,
}

impl Parse for StrategyArgs {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let kind: Ident = input.parse()?;
        let mut kv = Vec::new();
        while input.peek(syn::Token![,]) {
            input.parse::<syn::Token![,]>()?;
            if input.is_empty() {
                break;
            }
            let key: Ident = input.parse()?;
            input.parse::<syn::Token![=]>()?;
            let value: LitInt = input.parse()?;
            kv.push((key, value));
        }
        Ok(Self { kind, kv })
    }
}

impl StrategyArgs {
    fn to_strategy(&self) -> syn::Result<Strategy> {
        let get = |key: &str, default: usize| -> syn::Result<usize> {
            for (k, v) in &self.kv {
                if k == key {
                    return v.base10_parse();
                }
            }
            Ok(default)
        };
        match self.kind.to_string().as_str() {
            "predict" => {
                let max_retries = get("max_retries", 2)?;
                Ok(Strategy::Predict { max_retries })
            }
            "code_act" => {
                let max_iterations = get("max_iterations", 50)?;
                let max_retries = get("max_retries", 2)?;
                Ok(Strategy::CodeAct {
                    max_iterations,
                    max_retries,
                })
            }
            other => Err(syn::Error::new_spanned(
                &self.kind,
                format!("unknown strategy `{other}` — expected `predict` or `code_act`"),
            )),
        }
    }
}

fn parse_strategy(attrs: &[Attribute]) -> syn::Result<Strategy> {
    let mut result: Option<Strategy> = None;
    for attr in attrs {
        if !attr.path().is_ident("strategy") {
            continue;
        }
        let args: StrategyArgs = match &attr.meta {
            Meta::List(l) => l.parse_args()?,
            _ => {
                return Err(syn::Error::new_spanned(
                    attr,
                    "expected `#[strategy(predict)]` or `#[strategy(code_act, max_iterations = 10)]`",
                ));
            }
        };
        result = Some(args.to_strategy()?);
    }
    Ok(result.unwrap_or_default())
}

// ── Main expansion ────────────────────────────────────────────────────────────

fn expand_impl(impl_block: ItemImpl) -> syn::Result<TokenStream2> {
    if impl_block.trait_.is_some() {
        return Err(syn::Error::new_spanned(
            &impl_block.self_ty,
            "#[agent_impl] only supports inherent impl blocks",
        ));
    }
    if !impl_block.generics.params.is_empty() {
        return Err(syn::Error::new_spanned(
            &impl_block.self_ty,
            "#[agent_impl] does not support generic impl blocks (Phase 1)",
        ));
    }

    // Self type must be a plain struct name — the adapter structs and
    // tool registration reference it directly.
    let struct_name = match &*impl_block.self_ty {
        Type::Path(p)
            if p.qself.is_none()
                && p.path.segments.len() == 1
                && p.path.segments[0].arguments.is_empty() =>
        {
            p.path.segments[0].ident.clone()
        }
        _ => {
            return Err(syn::Error::new_spanned(
                &impl_block.self_ty,
                "#[agent_impl] requires a plain struct name (no generics, no path)",
            ));
        }
    };
    let struct_ty = &impl_block.self_ty;

    let mut output_items: Vec<TokenStream2> = Vec::new();
    let mut tool_adapters: Vec<TokenStream2> = Vec::new();
    let mut tool_registrations: Vec<TokenStream2> = Vec::new();
    let mut tool_names: Vec<LitStr> = Vec::new();

    for item in impl_block.items {
        match item {
            ImplItem::Fn(mut method) => {
                let is_generation = method.sig.asyncness.is_some() && method.block.stmts.is_empty();
                if is_generation {
                    let transformed = transform_generation_method(method)?;
                    output_items.push(transformed);
                } else if method.sig.asyncness.is_some() || is_skip(&method.attrs) {
                    // User-implemented async method, or explicitly excluded.
                    method.attrs = strip_helpers(method.attrs);
                    output_items.push(quote! { #method });
                } else {
                    let (method, tool) = make_tool_adapter(method, &struct_name, struct_ty)?;
                    output_items.push(method);
                    tool_adapters.push(tool.adapter);
                    tool_registrations.push(tool.registration);
                    tool_names.push(tool.name);
                }
            }
            other => output_items.push(quote! { #other }),
        }
    }

    // ── Method half of AgentBlueprint (inherent methods) ──────────────────
    //
    // The `#[derive(Agent)]` trait impl calls these by name at its concrete
    // impl site, where inherent methods win over the trait's no-op defaults.
    // This keeps exactly ONE `impl AgentBlueprint for #struct_name` in the
    // program — two trait impls for one type would be a conflict.
    let method_half = if tool_names.is_empty() {
        TokenStream2::new()
    } else {
        quote! {
            #[automatically_derived]
            impl #struct_ty {
                #[doc(hidden)]
                fn blueprint_register_method_tools(
                    &self,
                    __registry: &mut agent_kit::tools::ToolRegistry,
                ) {
                    #( #tool_registrations )*
                }

                #[doc(hidden)]
                fn blueprint_method_tool_names(&self) -> Vec<String> {
                    vec![#( #tool_names.to_string() ),*]
                }
            }
        }
    };

    Ok(quote! {
        #( #tool_adapters )*

        impl #struct_ty {
            #( #output_items )*
        }

        #method_half
    })
}

// ── Generation methods (empty async bodies → LLM call) ──────────────────────

fn transform_generation_method(mut method: ImplItemFn) -> syn::Result<TokenStream2> {
    let name = &method.sig.ident;

    if !method.sig.generics.params.is_empty() {
        return Err(syn::Error::new_spanned(
            name,
            "generation methods cannot be generic (Phase 1)",
        ));
    }
    let ret_ty = match &method.sig.output {
        ReturnType::Type(_, ty) => (**ty).clone(),
        ReturnType::Default => {
            return Err(syn::Error::new_spanned(
                name,
                "generation methods must declare a return type \
                 (the macro wraps it in `Result<T, agent_kit::GenerationError>`)",
            ));
        }
    };
    if is_result_type(&method.sig.output) {
        return Err(syn::Error::new_spanned(
            &ret_ty,
            "declare the plain value type — `#[agent_impl]` wraps it in \
             `Result<T, agent_kit::GenerationError>` automatically",
        ));
    }

    let args = collect_args(&method.sig.inputs, name)?;
    let strategy = parse_strategy(&method.attrs)?;

    // Prompt: doc comment + serialized arguments (Debug rendering).
    let doc = doc_comment(&method.attrs);
    let mut prompt = String::new();
    if !doc.is_empty() {
        prompt.push_str(&doc);
    }
    if !args.is_empty() {
        prompt.push_str("\n\nArguments:");
        for (arg, _) in &args {
            prompt.push_str(&format!("\n- {arg}: {{:?}}"));
        }
    }
    let prompt_lit = LitStr::new(&prompt, name.span());

    // Strategy → generated expression + whether tools are available.
    let (strategy_expr, use_tools) = match &strategy {
        Strategy::Predict { max_retries } => (
            quote! {
                agent_kit::Strategy::Predict { max_retries: #max_retries }
            },
            false,
        ),
        Strategy::CodeAct {
            max_iterations,
            max_retries,
        } => (
            quote! {
                agent_kit::Strategy::CodeAct {
                    max_iterations: #max_iterations,
                    max_retries: #max_retries,
                }
            },
            true,
        ),
    };

    let registry_stmts = if use_tools {
        quote! {
            let mut __registry = agent_kit::tools::ToolRegistry::new();
            agent_kit::AgentBlueprint::blueprint_register_tools(self, &mut __registry);
        }
    } else {
        TokenStream2::new()
    };
    let tools_arg = if use_tools {
        quote! { Some(&__registry) }
    } else {
        quote! { None }
    };

    let arg_idents: Vec<&Ident> = args.iter().map(|(i, _)| i).collect();

    let body: Block = syn::parse_quote! {
        {
            let __prompt = format!(#prompt_lit, #(#arg_idents),*);
            // Generation calls bypass the hook pipeline, so `#[context]`
            // blocks are inlined into the system prompt here.
            let __system = format!(
                "{}{}",
                self.agent_system_prompt(),
                self.agent_context_prompt()
            );
            #registry_stmts
            agent_kit::run_generation::<_, #ret_ty>(
                self.agent_client(),
                &self.agent_model(),
                &__system,
                &__prompt,
                #tools_arg,
                &#strategy_expr,
            )
            .await
        }
    };
    method.block = body;

    // `-> T` becomes `-> Result<T, GenerationError>` (the Rust equivalent
    // of the Python version raising exceptions).
    method.sig.output = syn::parse_quote! {
        -> Result<#ret_ty, agent_kit::GenerationError>
    };

    method.attrs = strip_helpers(method.attrs);
    Ok(quote! { #method })
}

// ── Sync methods → Tool adapters ─────────────────────────────────────────────

struct ToolAdapter {
    adapter: TokenStream2,
    registration: TokenStream2,
    name: LitStr,
}

fn make_tool_adapter(
    mut method: ImplItemFn,
    struct_name: &Ident,
    struct_ty: &Type,
) -> syn::Result<(TokenStream2, ToolAdapter)> {
    let name = &method.sig.ident;

    if !method.sig.generics.params.is_empty() {
        return Err(syn::Error::new_spanned(
            name,
            "tool methods cannot be generic (Phase 1)",
        ));
    }
    let args = collect_args(&method.sig.inputs, name)?;

    let tool_name = parse_tool_name(&method.attrs)?
        .unwrap_or_else(|| LitStr::new(&name.to_string(), name.span()));
    let description = doc_comment(&method.attrs);
    let description_lit = LitStr::new(&description, name.span());

    let args_struct_name = Ident::new(&format!("__AgentArgs_{struct_name}_{name}"), name.span());
    let tool_struct_name = Ident::new(&format!("__AgentTool_{struct_name}_{name}"), name.span());

    // Auto-derived arguments struct — the method signature IS the contract.
    let arg_idents: Vec<&Ident> = args.iter().map(|(i, _)| i).collect();
    let arg_tys: Vec<&Type> = args.iter().map(|(_, t)| t).collect();
    let args_struct = if args.is_empty() {
        quote! {
            #[doc(hidden)]
            #[allow(non_camel_case_types)]
            #[derive(agent_kit::serde::Deserialize, agent_kit::schemars::JsonSchema)]
            #[serde(crate = "agent_kit::serde")]
            #[schemars(crate = "agent_kit::schemars")]
            struct #args_struct_name {}
        }
    } else {
        quote! {
            #[doc(hidden)]
            #[allow(non_camel_case_types)]
            #[derive(agent_kit::serde::Deserialize, agent_kit::schemars::JsonSchema)]
            #[serde(crate = "agent_kit::serde")]
            #[schemars(crate = "agent_kit::schemars")]
            struct #args_struct_name {
                #( #arg_idents: #arg_tys ),*
            }
        }
    };

    // Delegate to the user's method (through the deserialized args struct);
    // `Result<T, E>` returns propagate E.
    let call = if is_result_type(&method.sig.output) {
        quote! {
            let __result = match self.agent.#name(#(args.#arg_idents),*) {
                Ok(__v) => __v,
                Err(__e) => {
                    return Err(agent_kit::tools::ToolError::Execution(__e.to_string()));
                }
            };
        }
    } else {
        quote! {
            let __result = self.agent.#name(#(args.#arg_idents),*);
        }
    };

    let adapter = quote! {
        #args_struct

        #[doc(hidden)]
        #[allow(non_camel_case_types)]
        struct #tool_struct_name {
            agent: ::std::sync::Arc<#struct_ty>,
        }

        impl agent_kit::tools::Tool for #tool_struct_name {
            fn name(&self) -> &str {
                #tool_name
            }

            fn description(&self) -> &str {
                #description_lit
            }

            fn parameter_schema(&self) -> agent_kit::serde_json::Value {
                static __SCHEMA: ::std::sync::OnceLock<agent_kit::serde_json::Value> =
                    ::std::sync::OnceLock::new();
                __SCHEMA
                    .get_or_init(|| agent_kit::tools::generate_schema::<#args_struct_name>())
                    .clone()
            }

            fn execute_stream(
                &self,
                raw_args: &str,
            ) -> Result<agent_kit::tools::ProgressStream, agent_kit::tools::ToolError> {
                let args: #args_struct_name = agent_kit::serde_json::from_str(raw_args)
                    .map_err(|e| {
                        agent_kit::tools::ToolError::InvalidArgs(format!("invalid args: {e}"))
                    })?;
                #call
                let __json = agent_kit::serde_json::to_string(&__result).map_err(|e| {
                    agent_kit::tools::ToolError::Execution(format!("serialize result: {e}"))
                })?;
                Ok(agent_kit::tools::ProgressStream::done(__json))
            }
        }
    };

    let registration = quote! {
        __registry.register(::std::sync::Arc::new(#tool_struct_name {
            agent: ::std::sync::Arc::new(self.clone()),
        }));
    };

    method.attrs = strip_helpers(method.attrs);
    Ok((
        quote! { #method },
        ToolAdapter {
            adapter,
            registration,
            name: tool_name,
        },
    ))
}

// ── Shared helpers ───────────────────────────────────────────────────────────

/// Validate the receiver (`&self` required) and collect the parameter
/// idents + types.
fn collect_args(
    inputs: &syn::punctuated::Punctuated<FnArg, syn::Token![,]>,
    name: &Ident,
) -> syn::Result<Vec<(Ident, Type)>> {
    let mut out = Vec::new();
    for input in inputs {
        match input {
            FnArg::Receiver(r) => {
                // Accept `&self` (with or without a lifetime), reject
                // `self`, `mut self`, `&mut self`, and typed receivers.
                let is_shared_ref = matches!(&r.kind, syn::ReceiverKind::Reference(_, _, None));
                if !is_shared_ref {
                    return Err(syn::Error::new_spanned(
                        name,
                        "agent methods must take `&self`",
                    ));
                }
            }
            FnArg::Typed(pt) => {
                let ident = match &*pt.pat {
                    Pat::Ident(pi) => pi.ident.clone(),
                    _ => {
                        return Err(syn::Error::new_spanned(
                            &pt.pat,
                            "unsupported parameter pattern — use plain identifiers",
                        ));
                    }
                };
                out.push((ident, (*pt.ty).clone()));
            }
        }
    }
    Ok(out)
}

/// Whether the declared return type is `Result<T, E>`.
fn is_result_type(output: &ReturnType) -> bool {
    matches!(
        output,
        ReturnType::Type(_, ty)
            if matches!(
                &**ty,
                Type::Path(p)
                    if p.qself.is_none()
                        && p.path.segments.last().is_some_and(|s| {
                            s.ident == "Result"
                                && matches!(&s.arguments, PathArguments::AngleBracketed(ab) if ab.args.len() == 2)
                        })
            )
    )
}
