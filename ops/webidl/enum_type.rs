// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.

use std::collections::HashSet;

use proc_macro2::TokenStream;
use quote::quote;
use syn::DataEnum;
use syn::DeriveInput;
use syn::Fields;
use syn::LitStr;

/// Generates `WebIdlConverter` for a WebIDL enum. A value is its ToString,
/// matched case-sensitively against each variant's kebab-case name (or its
/// `#[webidl(rename = "...")]` value).
pub(crate) fn derive(
  input: &DeriveInput,
  data: &DataEnum,
) -> syn::Result<TokenStream> {
  let name = &input.ident;
  let name_str = name.to_string();

  let mut arms = vec![];
  let mut values = HashSet::new();
  for variant in &data.variants {
    if !matches!(variant.fields, Fields::Unit) {
      return Err(syn::Error::new_spanned(
        variant,
        "#[webidl(enum)] variants must be unit variants",
      ));
    }

    let mut value = stringcase::kebab_case(&variant.ident.to_string());
    for attr in &variant.attrs {
      if attr.path().is_ident("webidl") {
        attr.parse_nested_meta(|meta| {
          if meta.path.is_ident("rename") {
            let lit: LitStr = meta.value()?.parse()?;
            value = lit.value();
            Ok(())
          } else {
            Err(meta.error(
              "unknown #[webidl(...)] key on an enum variant, expected `rename`",
            ))
          }
        })?;
      }
    }

    if !values.insert(value.clone()) {
      return Err(syn::Error::new_spanned(
        &variant.ident,
        format!("duplicate WebIDL enum value '{value}'"),
      ));
    }
    let ident = &variant.ident;
    arms.push(quote! {
      #value => std::result::Result::Ok(#name::#ident),
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
        options: &Self::Options,
      ) -> std::result::Result<Self, deno_core::webidl::WebIdlError>
      where
        C: Fn() -> std::borrow::Cow<'static, str>,
      {
        let _ = options;
        let string = <String as deno_core::webidl::WebIdlConverter<'a>>::convert(
          scope,
          value,
          prefix.clone(),
          || context(),
          &std::default::Default::default(),
        )?;
        match string.as_str() {
          #(#arms)*
          _ => std::result::Result::Err(deno_core::webidl::WebIdlError::new(
            prefix,
            context(),
            deno_core::webidl::WebIdlErrorKind::InvalidEnumValue {
              enum_name: #name_str,
              value: string,
            },
          )),
        }
      }
    }
  })
}
