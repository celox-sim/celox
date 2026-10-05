//! Instrument original Rust assertions, never evaluate hardware outputs.
use proc_macro::TokenStream;
use quote::{ToTokens, quote, quote_spanned};
use syn::{
    Expr, ExprMethodCall, Macro, Token, parse::Parser, punctuated::Punctuated, spanned::Spanned,
    visit_mut::VisitMut,
};

fn read(expr: &Expr) -> Option<(&ExprMethodCall, bool)> {
    match expr {
        Expr::Paren(x) => read(&x.expr),
        Expr::Group(x) => read(&x.expr),
        Expr::Reference(x) if x.mutability.is_none() => read(&x.expr).map(|(x, _)| (x, true)),
        Expr::MethodCall(x)
            if matches!(
                x.method.to_string().as_str(),
                "get" | "get_as" | "get_four_state"
            ) && x.args.len() == 1 =>
        {
            Some((x, false))
        }
        _ => None,
    }
}
fn instrument(mac: &Macro, alias: Option<(&syn::Ident, &Expr)>) -> Option<Expr> {
    let name = mac.path.get_ident()?.to_string();
    if name != "assert_eq" && name != "assert_ne" {
        return None;
    }
    let args = Punctuated::<Expr, Token![,]>::parse_terminated
        .parse2(mac.tokens.clone())
        .ok()?;
    if args.len() < 2 {
        return None;
    }
    let mut it = args.iter();
    let lhs = it.next()?;
    let rhs = it.next()?;
    if let Some((name, _)) = alias {
        let is_alias = |e: &Expr| matches!(e, Expr::Path(p) if p.path.is_ident(name));
        if !is_alias(lhs) && !is_alias(rhs) {
            return None;
        }
    }
    fn resolve<'a>(
        expr: &'a Expr,
        alias: Option<(&syn::Ident, &'a Expr)>,
    ) -> Option<(&'a ExprMethodCall, bool)> {
        if let Some((name, init)) = alias {
            if matches!(expr, Expr::Path(path) if path.path.is_ident(name)) {
                return read(init);
            }
        }
        read(expr)
    }
    let (actual, expected, reverse) = match (resolve(lhs, alias), resolve(rhs, alias)) {
        (Some(x), None) => (x, rhs, false),
        (None, Some(x)) => (x, lhs, true),
        _ => return None,
    };
    let (call, borrowed) = actual;
    // Restrict method receiver to a named Simulator. Rust type checking enforces
    // the capture method is available; arbitrary expressions are not rewritten.
    if !matches!(*call.receiver, Expr::Path(_)) {
        return None;
    }
    let receiver = &call.receiver;
    let signal = call.args.first()?;
    let kind = call.method.to_string();
    let comparison = if name == "assert_eq" { "eq" } else { "ne" };
    let ty = match kind.as_str() {
        "get" => quote! { crate::BigUint },
        "get_four_state" => quote! { (crate::BigUint, crate::BigUint) },
        "get_as" => {
            let types = &call.turbofish.as_ref()?.args;
            if types.len() != 1 {
                return None;
            }
            quote! { #types }
        }
        _ => unreachable!(),
    };
    let expected = if borrowed {
        quote! { #expected }
    } else {
        quote! { &(#expected) }
    };
    let actual_form = if alias.is_some() {
        "adjacent_immutable_alias"
    } else {
        "direct_read"
    };
    let sample = quote_spanned! { call.method.span()=> (file!(), line!(), column!()) };
    let begin = quote_spanned! { mac.path.span()=>
        #receiver.capture_observation(#signal, #kind, #comparison, file!(), line!(), column!(), stringify!(#mac), #sample, #actual_form)
    };
    let finish = match kind.as_str() {
        "get" => {
            quote! { crate::capture::finish_get(__capture_observation, (*__capture_expected).clone()) }
        }
        "get_four_state" => {
            quote! { crate::capture::finish_four_state(__capture_observation, (*__capture_expected).clone()) }
        }
        "get_as" => {
            quote! { crate::capture::finish_scalar::<#ty>(__capture_observation, *__capture_expected) }
        }
        _ => unreachable!(),
    };
    let tokens = if reverse && alias.is_none() {
        quote! {{ let __capture_expected: &#ty = #expected; let __capture_observation = #begin; #finish; }}
    } else {
        quote! {{ let __capture_observation = #begin; let __capture_expected: &#ty = #expected; #finish; }}
    };
    Some(syn::parse2(tokens).expect("generated capture expression"))
}
struct Capture;
fn ident_count(tokens: proc_macro2::TokenStream, name: &syn::Ident) -> usize {
    tokens
        .into_iter()
        .map(|t| match t {
            proc_macro2::TokenTree::Ident(i) => usize::from(i == *name),
            proc_macro2::TokenTree::Group(g) => ident_count(g.stream(), name),
            _ => 0,
        })
        .sum()
}
impl VisitMut for Capture {
    fn visit_block_mut(&mut self, block: &mut syn::Block) {
        let mut i = 0;
        while i + 1 < block.stmts.len() {
            let replacement = (|| {
                let syn::Stmt::Local(local) = &block.stmts[i] else {
                    return None;
                };
                let syn::Pat::Ident(pattern) = &local.pat else {
                    return None;
                };
                if pattern.mutability.is_some()
                    || pattern.by_ref.is_some()
                    || pattern.subpat.is_some()
                {
                    return None;
                }
                let init = local.init.as_ref()?;
                if init.diverge.is_some() || read(&init.expr).is_none() {
                    return None;
                }
                // Bound the supported dataflow: exactly one syntactic use in the
                // remaining block, no mutable binding, shadow, derived operation,
                // other control flow, or intervening statement/event.
                let uses: usize = block.stmts[i + 1..]
                    .iter()
                    .map(|s| ident_count(s.to_token_stream(), &pattern.ident))
                    .sum();
                if uses != 1 {
                    return None;
                }
                let mac = match &block.stmts[i + 1] {
                    syn::Stmt::Macro(m) => &m.mac,
                    syn::Stmt::Expr(Expr::Macro(m), _) => &m.mac,
                    _ => return None,
                };
                instrument(mac, Some((&pattern.ident, &init.expr)))
            })();
            if let Some(replacement) = replacement {
                block.stmts[i] =
                    syn::Stmt::Expr(replacement, Some(Token![;](proc_macro2::Span::call_site())));
                block.stmts.remove(i + 1);
            }
            i += 1;
        }
        syn::visit_mut::visit_block_mut(self, block);
    }
    fn visit_expr_mut(&mut self, expr: &mut Expr) {
        if let Expr::Macro(m) = expr {
            if let Some(replacement) = instrument(&m.mac, None) {
                *expr = replacement;
                return;
            }
        }
        syn::visit_mut::visit_expr_mut(self, expr);
    }
    fn visit_stmt_mut(&mut self, stmt: &mut syn::Stmt) {
        if let syn::Stmt::Macro(m) = stmt {
            if let Some(replacement) = instrument(&m.mac, None) {
                *stmt = syn::Stmt::Expr(replacement, Some(Token![;](m.mac.path.span())));
                return;
            }
        }
        syn::visit_mut::visit_stmt_mut(self, stmt);
    }
}
#[proc_macro]
pub fn capture_body(input: TokenStream) -> TokenStream {
    let mut block = syn::parse_macro_input!(input as syn::Block);
    Capture.visit_block_mut(&mut block);
    quote! { #block }.into()
}
