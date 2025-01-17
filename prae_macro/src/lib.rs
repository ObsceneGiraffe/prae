use proc_macro::TokenStream;
use quote::{format_ident, quote, ToTokens};
use syn::{
    parse::{Parse, ParseStream},
    parse_macro_input,
    spanned::Spanned,
    Error, ExprClosure, GenericArgument, Ident, Pat, PatType, Token, Type, TypePath, Visibility,
};

/// Convenience macro that defines a guarded type that promises to be
/// always valid. It may be used in different ways, see examples section for details.
#[proc_macro]
pub fn define(input: TokenStream) -> TokenStream {
    let Define {
        vis,
        ident,
        ty,
        adjust,
        guard,
    } = parse_macro_input!(input as Define);

    let adjust_fn = match adjust {
        None => quote! {
            fn adjust(v: &mut Self::Target) {}
        },
        Some(AdjustClosure(closure)) => quote! {
            fn adjust(v: &mut Self::Target) {
                let adjust: fn(&mut Self::Target) = #closure;
                adjust(v);
            }
        },
    };

    let err_ty = match &guard {
        None => quote!(()),
        Some(GuardClosure::Ensure(_)) => quote!(&'static str),
        Some(GuardClosure::Validate(ValidateClosure(_, err_ty))) => quote!(#err_ty),
    };

    let validate_fn = match &guard {
        None => quote! {
            fn validate(v: &Self::Target) -> Result<(), #err_ty> {
                Ok(())
            }
        },
        Some(GuardClosure::Ensure(EnsureClosure(closure))) => quote! {
            fn validate(v: &Self::Target) -> Result<(), #err_ty> {
                let f: fn(&Self::Target) -> bool = #closure;
                if f(v) { Ok(()) } else { Err("provided value is invalid") }
            }
        },
        Some(GuardClosure::Validate(ValidateClosure(closure, err_ty))) => quote! {
            fn validate(v: &Self::Target) -> Result<(), #err_ty> {
                let f: fn(&Self::Target) -> Result<(), #err_ty> = #closure;
                f(v)
            }
        },
    };

    let guard_ident = format_ident!("{}Guard", ident);
    let output = quote! {
       #[derive(Debug)]
        #vis struct #guard_ident;
        impl prae::Guard for #guard_ident {
            type Target = #ty;
            type Error = #err_ty;
            #adjust_fn
            #validate_fn
            fn alias_name() -> &'static str { stringify!(#ident) }
        }
        #vis type #ident = prae::Guarded<#guard_ident>;
    };

    TokenStream::from(output)
}

struct Define {
    vis: Visibility,
    ident: Ident,
    ty: Type,
    adjust: Option<AdjustClosure>,
    guard: Option<GuardClosure>,
}

impl Parse for Define {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        // Parse type definition.
        let vis: Visibility = input.parse()?;
        let ident: Ident = input.parse()?;
        input.parse::<Token![:]>()?;
        let ty: Type = input.parse()?;

        // Parse adjust closure (it may not exist).
        let adjust = parse_adjust_closure_for_ty(&ty, input)?;

        // Parse guard closure (it must exist).
        let guard = parse_guard_closure_for_ty(&ty, input)?;

        if adjust.is_none() && guard.is_none() {
            Err(Error::new(
                input.span(),
                "neither `adjust` nor `ensure`/`validate` closures were given",
            ))
        } else {
            Ok(Define {
                vis,
                ident,
                ty,
                adjust,
                guard,
            })
        }
    }
}

// Closure that takes one argument by mutable reference and returns nothing.
struct AdjustClosure(ExprClosure);

fn parse_adjust_closure_for_ty(
    ty: &Type,
    input: ParseStream,
) -> syn::Result<Option<AdjustClosure>> {
    // If there's no `adjust` keyword, return None.
    if !input.lookahead1().peek(kw::adjust) {
        return Ok(None);
    }

    // Parse the closure.
    input.parse::<kw::adjust>()?;
    let closure: ExprClosure = input.parse()?;

    // Validate the input of the closure.
    // Valid variants (`v` is an arbitrary name):
    // 1)  |v| ...
    // 2)  |v: &mut #ty| ...
    if closure.inputs.len() != 1 {
        return Err(Error::new(
            closure.inputs.span(),
            "closure must take exactly 1 argument",
        ));
    }
    let ty = ty.to_token_stream().to_string();
    let arg = closure.inputs.first().unwrap();
    match arg {
        Pat::Ident(_) => {}
        Pat::Type(PatType { ty: pty, .. })
            if pty.to_token_stream().to_string() == format!("& mut {}", ty) => {}
        _ => {
            return Err(Error::new(
                arg.span(),
                format!("must be ither `v` or `v: &mut {}`", ty),
            ))
        }
    }

    // Validate the output of the closure. It should be empty.
    if let syn::ReturnType::Type(_, _) = &closure.output {
        return Err(Error::new(
            closure.output.span(),
            "closure must not return anything",
        ));
    }

    Ok(Some(AdjustClosure(closure)))
}

// Either `ensure` or `validate` guard closure.
#[allow(clippy::large_enum_variant)]
enum GuardClosure {
    Ensure(EnsureClosure),
    Validate(ValidateClosure),
}

fn parse_guard_closure_for_ty(ty: &Type, input: ParseStream) -> syn::Result<Option<GuardClosure>> {
    let lk = input.lookahead1();
    if lk.peek(kw::ensure) {
        Ok(Some(GuardClosure::Ensure(parse_ensure_closure_for_ty(
            ty, input,
        )?)))
    } else if lk.peek(kw::validate) {
        Ok(Some(GuardClosure::Validate(parse_validate_closure_for_ty(
            ty, input,
        )?)))
    } else {
        Ok(None)
    }
}

// Closure that takes one argument by shared reference and returns
// true if the given argument holds it's invariants and false, if it
// doesn't.
struct EnsureClosure(ExprClosure);

fn parse_ensure_closure_for_ty(ty: &Type, input: ParseStream) -> syn::Result<EnsureClosure> {
    // Parse the closure.
    input.parse::<kw::ensure>()?;
    let closure: ExprClosure = input.parse()?;

    // Validate the input of the closure.
    // Valid variants (`v` is an arbitrary name):
    // 1)  |v| ...
    // 2)  |v: &#ty| ...
    if closure.inputs.len() != 1 {
        return Err(Error::new(
            closure.inputs.span(),
            "closure must take exactly 1 argument",
        ));
    }
    let ty = ty.to_token_stream().to_string();
    let arg = closure.inputs.first().unwrap();
    match arg {
        Pat::Ident(_) => {}
        Pat::Type(PatType { ty: pty, .. })
            if pty.to_token_stream().to_string() == format!("& {}", ty) => {}
        _ => {
            return Err(Error::new(
                arg.span(),
                format!("must be ither `v` or `v: &{}`", ty),
            ))
        }
    }

    // Validate the output of the closure.
    // Valid variants:
    // 1)  |...|
    // 2)  |...| -> bool
    if let syn::ReturnType::Type(_, ret_type) = &closure.output {
        if ret_type.to_token_stream().to_string() != "bool" {
            return Err(Error::new(ret_type.span(), "must be `bool`"));
        }
    }

    Ok(EnsureClosure(closure))
}

// Closure that takes one argument by shared reference and returns
// None if the given argument holds it's invariants and Some(YourError), if it
// doesn't.
struct ValidateClosure(ExprClosure, GenericArgument);

fn parse_validate_closure_for_ty(ty: &Type, input: ParseStream) -> syn::Result<ValidateClosure> {
    // Parse the closure.
    input.parse::<kw::validate>()?;
    let closure: ExprClosure = input.parse()?;

    // Validate the input of the closure.
    // Valid variants (`v` is an arbitrary name):
    // 1)  |v| ...
    // 2)  |v: &#ty| ...
    if closure.inputs.len() != 1 {
        return Err(Error::new(
            closure.inputs.span(),
            "closure must take exactly 1 argument",
        ));
    }
    let ty = ty.to_token_stream().to_string();
    let arg = closure.inputs.first().unwrap();
    match arg {
        Pat::Ident(_) => {}
        Pat::Type(PatType { ty: pty, .. })
            if pty.to_token_stream().to_string() == format!("& {}", ty) => {}
        _ => {
            return Err(Error::new(
                arg.span(),
                format!("must be ither `v` or `v: &{}`", ty),
            ))
        }
    }

    // Validate the output of the closure. It must return `Result<(), E>`.
    // Otherwise we won't be able to extract the error type `E`.
    let mut err_type: Option<GenericArgument> = None;
    if let syn::ReturnType::Type(_, ret_type) = &closure.output {
        if let Type::Path(TypePath { path, .. }) = ret_type.as_ref() {
            let seg = path.segments.first().unwrap(); // is it safe?
            if let "Result" | "std::result::Result" = seg.ident.to_string().as_str() {
                if let syn::PathArguments::AngleBracketed(ab) = &seg.arguments {
                    err_type = Some(ab.args.last().unwrap().clone())
                }
            }
        }
    }
    if err_type.is_none() {
        return Err(Error::new(
            closure.span(),
            "closure specify return type Result<(), E>",
        ));
    }

    Ok(ValidateClosure(closure, err_type.unwrap()))
}

mod kw {
    syn::custom_keyword!(adjust);
    syn::custom_keyword!(ensure);
    syn::custom_keyword!(validate);
}
