//! `#[module_interface(version = N)]`: one trait, two adapters.
//!
//! On a trait of `async fn name(&self, args...) -> anyhow::Result<T>` methods it
//! emits the trait (each method returning a `Send` future), `<Trait>Client`, which
//! calls a module service process over `webagent_module_service::Peer`, and
//! `<Trait>Server<T>`, which answers those calls from an implementation. Linked
//! builds use the implementation directly. Arguments and results are serde
//! values; `&str`, `&[T]` and `&T` arguments arrive as `String`, `Vec<T>` and `T`.
use proc_macro::TokenStream;
use quote::{format_ident, quote};
use syn::parse::{Parse, ParseStream};
use syn::spanned::Spanned;
use syn::{
    parse_macro_input, Error, FnArg, Ident, ItemTrait, LitInt, Pat, ReturnType, Token, TraitItem,
    Type,
};

struct Version(u32);

impl Parse for Version {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let key: Ident = input.parse()?;
        if key != "version" {
            return Err(Error::new(key.span(), "expected `version = N`"));
        }
        input.parse::<Token![=]>()?;
        Ok(Version(input.parse::<LitInt>()?.base10_parse()?))
    }
}

struct Method {
    name: Ident,
    wire: String,
    args: Vec<(Ident, Type)>,
    output: Type,
}

/// The owned type a reference argument is decoded into on the serving side.
fn owned(ty: &Type) -> syn::Result<Type> {
    let Type::Reference(reference) = ty else {
        return Ok(ty.clone());
    };
    if reference.mutability.is_some() {
        return Err(Error::new(
            ty.span(),
            "module calls cannot take `&mut` arguments",
        ));
    }
    Ok(match &*reference.elem {
        Type::Path(path) if path.path.is_ident("str") => syn::parse_quote!(::std::string::String),
        Type::Slice(slice) => {
            let elem = &slice.elem;
            syn::parse_quote!(::std::vec::Vec<#elem>)
        }
        elem => elem.clone(),
    })
}

fn method(interface: &str, item: &mut TraitItem) -> syn::Result<Method> {
    let TraitItem::Fn(function) = item else {
        return Err(Error::new(
            item.span(),
            "module interfaces contain only methods",
        ));
    };
    let signature = &mut function.sig;
    if signature.asyncness.is_none()
        || function.default.is_some()
        || !signature.generics.params.is_empty()
    {
        return Err(Error::new(
            signature.span(),
            "module methods are `async fn` without generics or default bodies",
        ));
    }
    let mut inputs = signature.inputs.iter();
    if !matches!(inputs.next(), Some(FnArg::Receiver(receiver)) if receiver.reference.is_some() && receiver.mutability.is_none())
    {
        return Err(Error::new(signature.span(), "module methods take `&self`"));
    }
    let mut args = Vec::new();
    for input in inputs {
        let FnArg::Typed(typed) = input else {
            unreachable!("only the first input is a receiver")
        };
        let Pat::Ident(name) = &*typed.pat else {
            return Err(Error::new(
                typed.pat.span(),
                "module arguments are plain names",
            ));
        };
        owned(&typed.ty)?;
        args.push((name.ident.clone(), (*typed.ty).clone()));
    }
    let ReturnType::Type(_, output) = &signature.output else {
        return Err(Error::new(
            signature.span(),
            "module methods return `anyhow::Result<T>`",
        ));
    };
    let output = (**output).clone();
    // Declare the method as returning a Send future so services can run it on any task.
    signature.asyncness = None;
    signature.output = syn::parse_quote!(-> impl ::std::future::Future<Output = #output> + Send);
    let name = signature.ident.clone();
    Ok(Method {
        wire: format!("{interface}/{name}"),
        name,
        args,
        output,
    })
}

#[proc_macro_attribute]
pub fn module_interface(attribute: TokenStream, item: TokenStream) -> TokenStream {
    let Version(version) = parse_macro_input!(attribute as Version);
    let mut definition = parse_macro_input!(item as ItemTrait);
    match expand(version, &mut definition) {
        Ok(tokens) => tokens.into(),
        Err(error) => error.to_compile_error().into(),
    }
}

fn expand(version: u32, definition: &mut ItemTrait) -> syn::Result<proc_macro2::TokenStream> {
    let name = definition.ident.clone();
    let interface = format!("{name}/{version}");
    let methods = definition
        .items
        .iter_mut()
        .map(|item| method(&interface, item))
        .collect::<syn::Result<Vec<_>>>()?;
    definition.supertraits.push(syn::parse_quote!(Send));
    definition.supertraits.push(syn::parse_quote!(Sync));
    definition.supertraits.push(syn::parse_quote!('static));
    let visibility = &definition.vis;
    let client = format_ident!("{name}Client");
    let server = format_ident!("{name}Server");
    let krate = quote!(::webagent_module_service);
    let client_methods = methods.iter().map(|m| {
        let Method { name, wire, args, output } = m;
        let names: Vec<_> = args.iter().map(|(name, _)| name).collect();
        let types = args.iter().map(|(_, ty)| ty);
        quote! {
            fn #name(&self, #(#names: #types),*) -> impl ::std::future::Future<Output = #output> + Send {
                let peer = self.0.clone();
                let args = #krate::__serde_json::to_value((#(#names,)*));
                async move {
                    let value = peer.call(#wire, args?).await?;
                    Ok(#krate::__serde_json::from_value(value)?)
                }
            }
        }
    });
    let server_arms = methods
        .iter()
        .map(|m| {
            let Method {
                name, wire, args, ..
            } = m;
            let names: Vec<_> = args.iter().map(|(name, _)| name).collect();
            let owned = args
                .iter()
                .map(|(_, ty)| owned(ty))
                .collect::<syn::Result<Vec<_>>>()?;
            let pass = args.iter().map(|(name, ty)| match ty {
                Type::Reference(_) => quote!(&#name),
                _ => quote!(#name),
            });
            Ok(quote! {
                #wire => Some(Box::pin(async move {
                    let (#(#names,)*): (#(#owned,)*) = #krate::__serde_json::from_value(args)?;
                    Ok(#krate::__serde_json::to_value(implementation.#name(#(#pass),*).await?)?)
                })),
            })
        })
        .collect::<syn::Result<Vec<_>>>()?;
    let client_doc = format!("Calls `{name}` in a module service process.");
    let server_doc = format!("Answers `{name}` calls from an implementation.");
    Ok(quote! {
        #definition

        #[doc = #client_doc]
        #[derive(Clone)]
        #visibility struct #client(pub #krate::Peer);

        impl #name for #client {
            #(#client_methods)*
        }

        #[doc = #server_doc]
        #visibility struct #server<T>(pub ::std::sync::Arc<T>);

        impl<T: #name> #krate::Service for #server<T> {
            fn interface(&self) -> &'static str {
                #interface
            }

            fn dispatch(
                &self,
                method: &str,
                args: #krate::__serde_json::Value,
            ) -> Option<#krate::BoxFuture<#krate::__anyhow::Result<#krate::__serde_json::Value>>> {
                let implementation = self.0.clone();
                match method {
                    #(#server_arms)*
                    _ => None,
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn expanded(definition: ItemTrait) -> syn::Result<String> {
        let mut definition = definition;
        expand(3, &mut definition).map(|tokens| tokens.to_string())
    }

    #[test]
    fn generates_client_and_server_with_versioned_wire_names() {
        let tokens = expanded(syn::parse_quote! {
            pub trait Ledger {
                async fn total(&self, owner: &str, days: &[u32]) -> anyhow::Result<u64>;
            }
        })
        .expect("expands");
        assert!(tokens.contains("LedgerClient"));
        assert!(tokens.contains("LedgerServer"));
        assert!(tokens.contains("\"Ledger/3/total\""));
        assert!(tokens.contains("\"Ledger/3\""));
        // Reference arguments decode into owned values on the serving side.
        assert!(tokens.contains(":: std :: string :: String"));
        assert!(tokens.contains(":: std :: vec :: Vec < u32 >"));
    }

    #[test]
    fn rejects_shapes_that_cannot_cross_a_process() {
        let cases: [ItemTrait; 4] = [
            syn::parse_quote!(
                trait T {
                    async fn f(&self, x: &mut u8) -> anyhow::Result<()>;
                }
            ),
            syn::parse_quote!(
                trait T {
                    async fn f<X>(&self, x: X) -> anyhow::Result<()>;
                }
            ),
            syn::parse_quote!(
                trait T {
                    async fn f(&self) -> anyhow::Result<()> {
                        Ok(())
                    }
                }
            ),
            syn::parse_quote!(
                trait T {
                    fn f(&self) -> anyhow::Result<()>;
                }
            ),
        ];
        for case in cases {
            assert!(expanded(case).is_err());
        }
    }
}
