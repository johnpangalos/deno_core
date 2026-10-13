// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.

use proc_macro2::TokenStream;
use quote::quote;
use syn::ext::IdentExt;
use syn::spanned::Spanned;
use syn::DataEnum;
use syn::Error;
use syn::Fields;
use syn::Lifetime;
use syn::LitStr;

pub fn convert(
  name: &str,
  lifetime: &Lifetime,
  data: &DataEnum,
) -> Result<TokenStream, Error> {
  let mut values = Vec::<String>::new();
  let mut arms = vec![];

  for variant in &data.variants {
    if !matches!(variant.fields, Fields::Unit) {
      return Err(Error::new(
        variant.span(),
        "enum variants must not have fields",
      ));
    }

    let mut value = stringcase::kebab_case(&variant.ident.unraw().to_string());
    for attr in &variant.attrs {
      if attr.path().is_ident("webidl") {
        attr.parse_nested_meta(|meta| {
          if meta.path.is_ident("rename") {
            value = meta.value()?.parse::<LitStr>()?.value();
            Ok(())
          } else {
            Err(meta.error("expected `rename = \"...\"`"))
          }
        })?;
      }
    }

    if values.contains(&value) {
      return Err(Error::new(
        variant.span(),
        format!("duplicate enum value '{value}'"),
      ));
    }

    let ident = &variant.ident;
    arms.push(quote!(#value => ::std::result::Result::Ok(Self::#ident),));
    values.push(value);
  }

  Ok(quote! {
    let __value = <::std::string::String as ::deno_core::webidl::WebIdlConverter<#lifetime>>::convert(
      __scope,
      __value,
      __prefix.clone(),
      &__context,
      &::std::default::Default::default(),
    )?;
    match __value.as_str() {
      #(#arms)*
      _ => ::std::result::Result::Err(::deno_core::webidl::WebIdlError::new(
        __prefix,
        &__context,
        ::deno_core::webidl::WebIdlErrorKind::InvalidEnumVariant {
          converter: #name,
          variant: __value,
        },
      )),
    }
  })
}
