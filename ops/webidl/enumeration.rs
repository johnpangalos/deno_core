// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.

use super::impl_lifetime;
use proc_macro2::TokenStream;
use quote::quote;
use syn::ext::IdentExt;
use syn::DataEnum;
use syn::DeriveInput;
use syn::Fields;
use syn::LitStr;

pub(super) fn expand(
  input: &DeriveInput,
  data: &DataEnum,
) -> Result<TokenStream, syn::Error> {
  let mut arms = TokenStream::new();
  let mut values: Vec<String> = Vec::new();

  for variant in &data.variants {
    if !matches!(variant.fields, Fields::Unit) {
      return Err(syn::Error::new_spanned(
        variant,
        "#[webidl(enum)] only supports unit variants",
      ));
    }

    let mut rename = None;
    for attr in variant.attrs.iter().filter(|a| a.path().is_ident("webidl")) {
      attr.parse_nested_meta(|meta| {
        if meta.path.is_ident("rename") {
          rename = Some(meta.value()?.parse::<LitStr>()?.value());
          Ok(())
        } else {
          Err(meta.error("unknown webidl variant attribute, expected `rename`"))
        }
      })?;
    }

    let value = rename.unwrap_or_else(|| {
      stringcase::kebab_case(&variant.ident.unraw().to_string())
    });
    if values.contains(&value) {
      return Err(syn::Error::new_spanned(
        variant,
        format!("duplicate WebIDL enum value '{value}'"),
      ));
    }
    let ident = &variant.ident;
    arms.extend(quote!(#value => ::std::result::Result::Ok(Self::#ident),));
    values.push(value);
  }

  let enum_name = input.ident.to_string();
  let (lifetime, generics) = impl_lifetime(input);
  let (impl_generics, _, where_clause) = generics.split_for_impl();
  let (_, ty_generics, _) = input.generics.split_for_impl();
  let name = &input.ident;

  Ok(quote! {
    impl #impl_generics ::deno_core::webidl::WebIdlConverter<#lifetime> for #name #ty_generics #where_clause {
      type Options = ();

      fn convert<C>(
        __scope: &mut ::deno_core::v8::HandleScope<#lifetime>,
        __input: ::deno_core::v8::Local<#lifetime, ::deno_core::v8::Value>,
        __prefix: ::std::borrow::Cow<'static, str>,
        __context: C,
        _options: &Self::Options,
      ) -> ::std::result::Result<Self, ::deno_core::webidl::WebIdlError>
      where
        C: ::std::ops::Fn() -> ::std::borrow::Cow<'static, str>,
      {
        let __value = <::std::string::String as ::deno_core::webidl::WebIdlConverter<#lifetime>>::convert(
          __scope,
          __input,
          __prefix.clone(),
          &__context,
          &::std::default::Default::default(),
        )?;
        match __value.as_str() {
          #arms
          __other => ::std::result::Result::Err(::deno_core::webidl::WebIdlError::new(
            __prefix,
            __context(),
            ::std::format!("value '{}' is not a valid member of enum '{}'", __other, #enum_name),
          )),
        }
      }
    }
  })
}
