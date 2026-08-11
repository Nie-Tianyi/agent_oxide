//! `#[derive(Agent)]` — turn a struct into an agent blueprint.
//!
//! Given:
//!
//! ```ignore
//! /// You are an agent specializing in analyzing customer feedback.
//! #[derive(Agent)]
//! struct FeedbackAgent {
//!     #[agent(client)]
//!     client: DeepSeekClient,
//! }
//! ```
//!
//! generates:
//!
//! - inherent methods: `agent_client()`, `agent_model()`,
//!   `agent_system_prompt()`, `into_agent(model)`, `into_agent_with(model, config)`
//! - an `agent_kit::AgentBlueprint` impl covering the *field* half:
//!   system prompt (doc comment), `#[tool]` field registration,
//!   `#[context(...)]` hooks.
//!
//! The `#[agent_impl]` macro implements the *method* half of the same trait.

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::{Data, DeriveInput, Field, Fields, Ident, LitStr, Meta, Type, parse_macro_input};

use crate::util::{is_skip, parse_tool_name};

/// How a `#[context(...)]` field should be rendered into the system prompt.
#[derive(Clone, Copy, PartialEq)]
enum ContextMode {
    /// Rendered once, when the hook is built.
    Static,
    /// Re-rendered before every LLM call (captures a clone of the field;
    /// shared types such as `Arc<RwLock<T>>` stay live).
    Dynamic,
}

struct FieldAttrs {
    is_client: bool,
    is_model: bool,
    is_tool: bool,
    tool_name: Option<LitStr>,
    context_mode: Option<ContextMode>,
}

impl FieldAttrs {
    fn parse(field: &Field) -> syn::Result<Self> {
        let mut out = FieldAttrs {
            is_client: false,
            is_model: false,
            is_tool: false,
            tool_name: None,
            context_mode: None,
        };

        for attr in &field.attrs {
            if attr.path().is_ident("agent") {
                let ident: Ident = match &attr.meta {
                    Meta::List(l) => l.parse_args()?,
                    _ => {
                        return Err(syn::Error::new_spanned(
                            attr,
                            "expected `#[agent(client)]`, `#[agent(model)]`, or `#[agent(skip)]`",
                        ));
                    }
                };
                match ident.to_string().as_str() {
                    "client" => out.is_client = true,
                    "model" => out.is_model = true,
                    "skip" => {}
                    other => {
                        return Err(syn::Error::new_spanned(
                            &ident,
                            format!("unknown agent attribute `{other}`"),
                        ));
                    }
                }
            } else if attr.path().is_ident("tool") {
                out.is_tool = true;
                out.tool_name = parse_tool_name(std::slice::from_ref(attr))?;
            } else if attr.path().is_ident("context") {
                out.context_mode = Some(match &attr.meta {
                    Meta::Path(_) => ContextMode::Static,
                    Meta::List(l) => {
                        let ident: Ident = l.parse_args()?;
                        match ident.to_string().as_str() {
                            "static" => ContextMode::Static,
                            "dynamic" => ContextMode::Dynamic,
                            other => {
                                return Err(syn::Error::new_spanned(
                                    &ident,
                                    format!(
                                        "unknown context mode `{other}` — expected `static` or `dynamic`"
                                    ),
                                ));
                            }
                        }
                    }
                    Meta::NameValue(_) => {
                        return Err(syn::Error::new_spanned(
                            attr,
                            "expected `#[context]`, `#[context(static)]`, or `#[context(dynamic)]`",
                        ));
                    }
                });
            }
        }

        Ok(out)
    }
}

pub fn expand(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    expand_impl(input)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

fn expand_impl(input: DeriveInput) -> syn::Result<TokenStream2> {
    let name = &input.ident;
    let doc = crate::util::doc_comment(&input.attrs);

    let fields = match &input.data {
        Data::Struct(s) => &s.fields,
        _ => {
            return Err(syn::Error::new_spanned(
                name,
                "#[derive(Agent)] only supports structs",
            ));
        }
    };
    let fields = match fields {
        Fields::Named(f) => &f.named,
        _ => {
            return Err(syn::Error::new_spanned(
                name,
                "#[derive(Agent)] requires named fields",
            ));
        }
    };

    // ── Scan fields ──────────────────────────────────────────────────────
    let mut client: Option<(Ident, Type)> = None;
    let mut model: Option<Ident> = None;
    let mut tool_fields: Vec<(Ident, LitStr)> = Vec::new(); // (field, tool name)
    let mut context_fields: Vec<(Ident, ContextMode)> = Vec::new();

    for field in fields {
        let ident = field.ident.as_ref().expect("named field has ident");
        let attrs = FieldAttrs::parse(field)?;

        if attrs.is_client {
            if client.is_some() {
                return Err(syn::Error::new_spanned(
                    ident,
                    "multiple `#[agent(client)]` fields",
                ));
            }
            client = Some((ident.clone(), field.ty.clone()));
            continue;
        }
        if attrs.is_model {
            if model.is_some() {
                return Err(syn::Error::new_spanned(
                    ident,
                    "multiple `#[agent(model)]` fields",
                ));
            }
            if !is_string_type(&field.ty) {
                return Err(syn::Error::new_spanned(
                    &field.ty,
                    "`#[agent(model)]` field must be `String`",
                ));
            }
            model = Some(ident.clone());
            continue;
        }
        if attrs.is_tool {
            let tool_name = attrs
                .tool_name
                .unwrap_or_else(|| LitStr::new(&ident.to_string(), ident.span()));
            tool_fields.push((ident.clone(), tool_name));
            continue;
        }
        if let Some(mode) = attrs.context_mode {
            context_fields.push((ident.clone(), mode));
            continue;
        }
        if is_skip(&field.attrs) {
            continue;
        }
        // Auto-detect the client by field name.
        if client.is_none() && (ident == "client" || ident == "llm") {
            client = Some((ident.clone(), field.ty.clone()));
        }
    }

    let Some((client_field, client_ty)) = client else {
        return Err(syn::Error::new_spanned(
            name,
            "no client field found — annotate a field with `#[agent(client)]` \
             (or name it `client` or `llm`)",
        ));
    };

    // ── Context blocks (used by both impl halves) ────────────────────────
    let (context_inherent, context_trait) = if context_fields.is_empty() {
        (
            quote! {
                /// Rendered context blocks — empty because this agent
                /// defines no `#[context(...)]` fields.
                pub fn agent_context_prompt(&self) -> String {
                    String::new()
                }
            },
            TokenStream2::new(),
        )
    } else {
        let blocks: Vec<TokenStream2> = context_fields
            .iter()
            .map(|(ident, mode)| {
                let key = LitStr::new(&ident.to_string(), ident.span());
                match mode {
                    ContextMode::Static => quote! {
                        __hook.add(agent_kit::ContextBlock::static_block(
                            #key,
                            10,
                            agent_kit::serde_json::to_string(&self.#ident).unwrap_or_default(),
                        ));
                    },
                    ContextMode::Dynamic => quote! {
                        {
                            let __snapshot = self.#ident.clone();
                            __hook.add(agent_kit::ContextBlock::dynamic_block(
                                #key,
                                10,
                                move || {
                                    agent_kit::serde_json::to_string(&__snapshot)
                                        .map_err(|e| e.to_string())
                                        .ok()
                                },
                            ));
                        }
                    },
                }
            })
            .collect();

        let inherent = quote! {
            #[doc(hidden)]
            fn __agent_context_hook(&self) -> agent_kit::ContextBlockHook {
                let mut __hook = agent_kit::ContextBlockHook::new();
                #( #blocks )*
                __hook
            }

            /// The rendered context blocks, appended to the system prompt
            /// by generation methods (their LLM calls bypass the hook
            /// pipeline of full agent runs).
            pub fn agent_context_prompt(&self) -> String {
                let mut __out = String::new();
                for (__key, __content) in self.__agent_context_hook().render_all() {
                    __out.push_str(&format!("\n[CONTEXT:{__key}]\n{__content}"));
                }
                __out
            }
        };
        let trait_impl = quote! {
            fn blueprint_context_hooks(&self) -> Vec<Box<dyn agent_kit::engine::AgentHook>> {
                vec![Box::new(self.__agent_context_hook())]
            }
        };
        (inherent, trait_impl)
    };

    // ── Inherent methods ─────────────────────────────────────────────────
    let model_expr = match &model {
        Some(f) => quote! { self.#f.clone() },
        None => quote! { agent_kit::DEFAULT_MODEL.to_string() },
    };
    let doc_lit = LitStr::new(&doc, name.span());

    let inherent = quote! {
        #[automatically_derived]
        impl #name {
            #context_inherent
            /// The LLM client used by this agent's generation methods.
            pub fn agent_client(&self) -> &#client_ty {
                &self.#client_field
            }

            /// The model name used by this agent's generation methods.
            pub fn agent_model(&self) -> String {
                #model_expr
            }

            /// The system prompt (from the struct's doc comment).
            pub fn agent_system_prompt(&self) -> String {
                #doc_lit.to_string()
            }

            /// Assemble a `core::engine::Agent` from this agent struct.
            pub fn into_agent(
                self,
                model: impl Into<String>,
            ) -> Result<agent_kit::engine::Agent<#client_ty>, agent_kit::engine::AgentError> {
                self.into_agent_with(model, agent_kit::BuildConfig::default())
            }

            /// Assemble a `core::engine::Agent` with a custom [`BuildConfig`].
            pub fn into_agent_with(
                self,
                model: impl Into<String>,
                mut config: agent_kit::BuildConfig,
            ) -> Result<agent_kit::engine::Agent<#client_ty>, agent_kit::engine::AgentError> {
                let mut __registry = agent_kit::tools::ToolRegistry::new();
                agent_kit::AgentBlueprint::blueprint_register_tools(&self, &mut __registry);
                let mut __hooks = agent_kit::AgentBlueprint::blueprint_context_hooks(&self);
                __hooks.extend(config.extra_hooks.drain(..));
                let __config = agent_kit::BuildConfig { extra_hooks: __hooks, ..config };
                let __system_prompt = self.agent_system_prompt();
                agent_kit::AgentAssembler::new(self.#client_field, model.into())
                    .system_prompt(__system_prompt)
                    .tools(move |__reg: &mut agent_kit::tools::ToolRegistry| {
                        for (_, __tool) in __registry.iter() {
                            __reg.register(__tool.clone());
                        }
                    })
                    .config(__config)
                    .build()
            }
        }
    };

    // ── AgentBlueprint impl (field half) ──────────────────────────────────
    let field_tools = if tool_fields.is_empty() {
        TokenStream2::new()
    } else {
        let tool_idents: Vec<&Ident> = tool_fields.iter().map(|(i, _)| i).collect();
        let tool_name_lits: Vec<&LitStr> = tool_fields.iter().map(|(_, n)| n).collect();

        quote! {
            fn blueprint_register_field_tools(
                &self,
                __registry: &mut agent_kit::tools::ToolRegistry,
            ) {
                #(
                    __registry.register(::std::sync::Arc::new(self.#tool_idents.clone()));
                )*
            }

            fn blueprint_field_tool_names(&self) -> Vec<String> {
                vec![#( #tool_name_lits.to_string() ),*]
            }
        }
    };

    let blueprint = quote! {
        #[automatically_derived]
        impl agent_kit::AgentBlueprint for #name {
            fn blueprint_system_prompt(&self) -> String {
                #doc_lit.to_string()
            }

            // The method half (sync-method tools) is contributed by
            // `#[agent_impl]` as INHERENT methods of the same names.
            // At this concrete impl site inherent methods win over the
            // trait's no-op defaults, so an agent struct without a
            // `#[agent_impl]` block simply registers nothing — and only
            // one trait impl exists (two trait impls for one type are
            // illegal in Rust).
            fn blueprint_register_tools(
                &self,
                __registry: &mut agent_kit::tools::ToolRegistry,
            ) {
                self.blueprint_register_field_tools(__registry);
                self.blueprint_register_method_tools(__registry);
            }

            fn blueprint_tool_names(&self) -> Vec<String> {
                let mut __names = self.blueprint_field_tool_names();
                __names.extend(self.blueprint_method_tool_names());
                __names
            }

            #field_tools
            #context_trait
        }
    };

    Ok(quote! { #inherent #blueprint })
}

fn is_string_type(ty: &Type) -> bool {
    matches!(ty, Type::Path(p) if p.qself.is_none() && p.path.segments.last().is_some_and(|s| s.ident == "String" && s.arguments.is_empty()))
}
