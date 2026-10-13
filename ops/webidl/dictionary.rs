// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.

use super::impl_lifetime;
use proc_macro2::TokenStream;
use quote::format_ident;
use quote::quote;
use syn::ext::IdentExt;
use syn::spanned::Spanned;
use syn::DataStruct;
use syn::DeriveInput;
use syn::Expr;
use syn::Field;
use syn::Fields;
use syn::Ident;
use syn::Lifetime;
use syn::LitStr;
use syn::Type;

/// One dictionary member, after its attributes have been parsed.
struct Member {
  ident: Ident,
  /// The local the converted value is bound to. Field names are never used as
  /// bindings, so a field called `__input` cannot clash with generated code.
  local: Ident,
  ty: Type,
  /// The JavaScript property name.
  key: String,
  default: Option<Expr>,
  required: bool,
  /// Assignments for `#[options(...)]`, applied to `Default::default()`.
  options: Vec<(Ident, Expr)>,
  is_option: bool,
}

pub(super) fn expand(
  input: &DeriveInput,
  data: &DataStruct,
) -> Result<TokenStream, syn::Error> {
  let fields = match &data.fields {
    Fields::Named(fields) => fields,
    _ => {
      return Err(syn::Error::new(
        data.struct_token.span,
        "#[webidl(dictionary)] requires a struct with named fields",
      ))
    }
  };

  let mut members = fields
    .named
    .iter()
    .enumerate()
    .map(|(index, field)| parse_member(field, index))
    .collect::<Result<Vec<_>, _>>()?;
  // WebIDL orders members by their keys compared as UTF-16 code units.
  members.sort_by(|a, b| a.key.encode_utf16().cmp(b.key.encode_utf16()));
  for pair in members.windows(2) {
    if pair[0].key == pair[1].key {
      return Err(syn::Error::new(
        pair[1].ident.span(),
        format!("duplicate WebIDL member key '{}'", pair[1].key),
      ));
    }
  }

  let dict_name = input.ident.to_string();
  let (lifetime, generics) = impl_lifetime(input);
  let (impl_generics, _, where_clause) = generics.split_for_impl();
  let (_, ty_generics, _) = input.generics.split_for_impl();
  let name = &input.ident;

  // Members are read in lexicographic order of their JavaScript keys, which is
  // the order `members` is sorted in.
  let conversions = members
    .iter()
    .map(|member| member_conversion(member, &dict_name, &lifetime));
  let field_idents = members.iter().map(|member| &member.ident);
  let locals = members.iter().map(|member| &member.local);

  Ok(quote! {
    impl #impl_generics ::deno_core::webidl::WebIdlConverter<#lifetime> for #name #ty_generics #where_clause {
      type Options = ();

      #[allow(unused_variables)]
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
        if !(__input.is_null_or_undefined() || __input.is_object()) {
          return ::std::result::Result::Err(::deno_core::webidl::WebIdlError::new(
            __prefix,
            __context(),
            "is not an object",
          ));
        }
        #(#conversions)*
        ::std::result::Result::Ok(Self { #(#field_idents: #locals),* })
      }
    }
  })
}

fn parse_member(field: &Field, index: usize) -> Result<Member, syn::Error> {
  let ident = field.ident.clone().expect("named field");
  let mut rename = None;
  let mut default = None;
  let mut required = None;
  let mut options = Vec::new();

  for attr in &field.attrs {
    if attr.path().is_ident("webidl") {
      attr.parse_nested_meta(|meta| {
        if meta.path.is_ident("rename") {
          rename = Some(meta.value()?.parse::<LitStr>()?.value());
        } else if meta.path.is_ident("default") {
          default = Some(meta.value()?.parse::<Expr>()?);
        } else if meta.path.is_ident("required") {
          required = Some(meta.path.span());
        } else {
          return Err(meta.error(
            "unknown webidl member attribute, expected `rename`, `default` or `required`",
          ));
        }
        Ok(())
      })?;
    } else if attr.path().is_ident("options") {
      attr.parse_nested_meta(|meta| {
        let Some(name) = meta.path.get_ident().cloned() else {
          return Err(meta.error("expected the name of an option field"));
        };
        let value: Expr = meta.value()?.parse()?;
        options.push((name, value));
        Ok(())
      })?;
    }
  }

  if let (Some(span), Some(_)) = (required, &default) {
    return Err(syn::Error::new(
      span,
      "`required` and `default` cannot be used together",
    ));
  }

  let key = rename
    .unwrap_or_else(|| stringcase::camel_case(&ident.unraw().to_string()));
  let is_option = matches!(&field.ty, Type::Path(path)
    if path.path.segments.last().is_some_and(|s| s.ident == "Option"));

  Ok(Member {
    local: format_ident!("__member_{}", index),
    ident,
    ty: field.ty.clone(),
    key,
    default,
    required: required.is_some(),
    options,
    is_option,
  })
}

/// Generates the code that reads one member into its local.
fn member_conversion(
  member: &Member,
  dict_name: &str,
  lifetime: &Lifetime,
) -> TokenStream {
  let local = &member.local;
  let ty = &member.ty;
  let key = &member.key;

  let required_error = quote! {
    return ::std::result::Result::Err(::deno_core::webidl::WebIdlError::new(
      __prefix.clone(),
      __ctx(),
      "is required",
    ))
  };
  let missing = if let Some(default) = &member.default {
    quote!(#default)
  } else if member.is_option && !member.required {
    quote!(::std::option::Option::None)
  } else {
    required_error
  };

  let options = if member.options.is_empty() {
    quote!(&::std::default::Default::default())
  } else {
    let assignments = member
      .options
      .iter()
      .map(|(name, value)| quote!(__options.#name = #value;));
    quote! {
      &{
        let mut __options: <#ty as ::deno_core::webidl::WebIdlConverter<#lifetime>>::Options =
          ::std::default::Default::default();
        #(#assignments)*
        __options
      }
    }
  };

  quote! {
    let #local: #ty = {
      let __ctx = || ::std::borrow::Cow::Owned(::std::format!(
        "{}, member '{}' of '{}'",
        __context(),
        #key,
        #dict_name,
      ));
      let __value = ::deno_core::webidl::get_dictionary_member(
        __scope,
        __input,
        #key,
        &__prefix,
        &__ctx,
      )?;
      if __value.is_undefined() {
        #missing
      } else {
        <#ty as ::deno_core::webidl::WebIdlConverter<#lifetime>>::convert(
          __scope,
          __value,
          __prefix.clone(),
          &__ctx,
          #options,
        )?
      }
    };
  }
}
