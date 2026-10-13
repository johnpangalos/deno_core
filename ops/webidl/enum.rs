// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.

use proc_macro2::TokenStream;
use quote::quote;
use syn::DataEnum;
use syn::DeriveInput;
use syn::Expr;
use syn::ExprLit;
use syn::Fields;
use syn::Lit;

use super::parse_args;

pub(super) fn derive(
  input: &DeriveInput,
  data: &DataEnum,
) -> Result<TokenStream, syn::Error> {
  if !input.generics.params.is_empty() {
    return Err(syn::Error::new_spanned(
      &input.generics,
      "#[webidl(enum)] does not support generic enums",
    ));
  }
  if data.variants.is_empty() {
    return Err(syn::Error::new_spanned(
      &input.ident,
      "#[webidl(enum)] requires at least one variant",
    ));
  }

  let name = &input.ident;
  let enum_name = name.to_string();
  let mut arms = Vec::new();
  let mut values: Vec<String> = Vec::new();
  for variant in &data.variants {
    if !matches!(variant.fields, Fields::Unit) {
      return Err(syn::Error::new_spanned(
        variant,
        "#[webidl(enum)] requires unit variants",
      ));
    }
    let mut value = stringcase::kebab_case(&variant.ident.to_string());
    let mut seen: Vec<String> = Vec::new();
    for attr in &variant.attrs {
      if !attr.path().is_ident("webidl") {
        continue;
      }
      for arg in parse_args(attr)? {
        let name = arg.name.to_string();
        if seen.contains(&name) {
          return Err(syn::Error::new_spanned(
            &arg.name,
            format!("duplicate #[webidl] variant attribute `{name}`"),
          ));
        }
        seen.push(name.clone());
        match (name.as_str(), arg.value) {
          ("rename", Some(expr)) => {
            value = match &expr {
              Expr::Lit(ExprLit {
                lit: Lit::Str(s), ..
              }) => s.value(),
              other => {
                return Err(syn::Error::new_spanned(
                  other,
                  "expected a string literal",
                ))
              }
            };
          }
          (name, _) => {
            return Err(syn::Error::new_spanned(
              &arg.name,
              format!("unknown #[webidl] variant attribute `{name}`"),
            ))
          }
        }
      }
    }
    if values.contains(&value) {
      return Err(syn::Error::new_spanned(
        variant,
        format!("duplicate enumeration value '{value}'"),
      ));
    }
    values.push(value.clone());
    let ident = &variant.ident;
    arms.push(quote! {
      #value => Ok(#name::#ident),
    });
  }

  Ok(quote! {
    impl<'a> deno_core::webidl::WebIdlConverter<'a> for #name {
      type Options = ();

      fn convert<C>(
        scope: &mut deno_core::v8::HandleScope<'a>,
        value: deno_core::v8::Local<'a, deno_core::v8::Value>,
        prefix: std::borrow::Cow<'static, str>,
        context: C,
        _options: &Self::Options,
      ) -> Result<Self, deno_core::webidl::WebIdlError>
      where
        C: Fn() -> std::borrow::Cow<'static, str>,
      {
        let string = deno_core::webidl::string_value(scope, value, &prefix, &context)?;
        match string.as_str() {
          #(#arms)*
          _ => Err(deno_core::webidl::WebIdlError::new(
            prefix,
            &context,
            format!("'{}' is not a valid value for enumeration '{}'", string, #enum_name),
          )),
        }
      }
    }
  })
}
