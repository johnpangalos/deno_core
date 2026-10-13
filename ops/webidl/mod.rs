// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.

//! Implementation of `#[derive(WebIDL)]`.
//!
//! The generated code only refers to `::deno_core::...` paths, so it works in
//! any crate that depends on `deno_core`.

use proc_macro2::TokenStream;
use quote::format_ident;
use quote::quote;
use quote::ToTokens;
use std::collections::HashSet;
use syn::ext::IdentExt;
use syn::Data;
use syn::DeriveInput;
use syn::Expr;
use syn::Fields;
use syn::GenericArgument;
use syn::Ident;
use syn::LitStr;
use syn::PathArguments;
use syn::Type;

/// What the derive generates for the item.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
  Dictionary,
  Enum,
}

/// A `#[webidl(...)]` or `#[options(...)]` setting on a dictionary member.
struct Member {
  ident: Ident,
  /// The JS key of the member.
  key: String,
  ty: Type,
  /// `Some(inner)` if the member type is `Option<inner>`.
  option_inner: Option<Type>,
  default: Option<Expr>,
  required: bool,
  /// `(option name, value)` pairs from `#[options(...)]`.
  options: Vec<(Ident, Expr)>,
}

pub fn derive(input: DeriveInput) -> syn::Result<TokenStream> {
  if !input.generics.params.is_empty() {
    return Err(syn::Error::new_spanned(
      &input.generics,
      "#[derive(WebIDL)] does not support generic types.",
    ));
  }
  let kind = item_kind(&input)?;
  match (kind, &input.data) {
    (Kind::Dictionary, Data::Struct(data)) => match &data.fields {
      Fields::Named(fields) => {
        let mut keys = HashSet::new();
        let members = fields
          .named
          .iter()
          .map(|field| {
            let member = member(field)?;
            check_key(field, &member.key, &mut keys, "member")?;
            Ok(member)
          })
          .collect::<syn::Result<Vec<_>>>()?;
        Ok(dictionary(&input, members))
      }
      _ => Err(syn::Error::new_spanned(
        &input.ident,
        "#[webidl(dictionary)] requires a struct with named fields.",
      )),
    },
    (Kind::Enum, Data::Enum(data)) => {
      if data.variants.is_empty() {
        return Err(syn::Error::new_spanned(
          &input.ident,
          "#[webidl(enum)] requires at least one variant.",
        ));
      }
      let mut keys = HashSet::new();
      let variants = data
        .variants
        .iter()
        .map(|variant| {
          if !matches!(variant.fields, Fields::Unit) {
            return Err(syn::Error::new_spanned(
              variant,
              "#[webidl(enum)] only supports unit variants.",
            ));
          }
          let value = match webidl_string(&variant.attrs, "rename")? {
            Some(rename) => rename,
            None => stringcase::kebab_case(&variant.ident.unraw().to_string()),
          };
          check_key(variant, &value, &mut keys, "variant")?;
          Ok((variant.ident.clone(), value))
        })
        .collect::<syn::Result<Vec<_>>>()?;
      Ok(enum_impl(&input, variants))
    }
    (Kind::Dictionary, _) => Err(syn::Error::new_spanned(
      &input.ident,
      "#[webidl(dictionary)] can only be used on a struct.",
    )),
    (Kind::Enum, _) => Err(syn::Error::new_spanned(
      &input.ident,
      "#[webidl(enum)] can only be used on an enum.",
    )),
  }
}

/// Rejects a JS key that is empty, or that another member or variant of the
/// same item already maps to.
fn check_key(
  tokens: impl ToTokens,
  key: &str,
  seen: &mut HashSet<String>,
  what: &str,
) -> syn::Result<()> {
  if key.is_empty() {
    return Err(syn::Error::new_spanned(
      tokens,
      format!("the JS key of this {what} is empty, use #[webidl(rename = \"...\")] to give it a name."),
    ));
  }
  if !seen.insert(key.to_string()) {
    return Err(syn::Error::new_spanned(
      tokens,
      format!("two {what}s map to the JS key '{key}', use #[webidl(rename = \"...\")] to give one of them another key."),
    ));
  }
  Ok(())
}

/// Reads the `#[webidl(dictionary)]` or `#[webidl(enum)]` item attribute.
fn item_kind(input: &DeriveInput) -> syn::Result<Kind> {
  let mut kind = None;
  for attr in input.attrs.iter().filter(|a| a.path().is_ident("webidl")) {
    attr.parse_nested_meta(|meta| {
      let this = if meta.path.is_ident("dictionary") {
        Kind::Dictionary
      } else if meta.path.is_ident("enum") {
        Kind::Enum
      } else {
        return Err(meta.error(
          "unknown #[webidl] key for an item, expected `dictionary` or `enum`",
        ));
      };
      if kind.replace(this).is_some() {
        return Err(meta.error("duplicate #[webidl] item kind"));
      }
      Ok(())
    })?;
  }
  kind.ok_or_else(|| {
    syn::Error::new_spanned(
      &input.ident,
      "#[derive(WebIDL)] requires #[webidl(dictionary)] or #[webidl(enum)].",
    )
  })
}

/// Reads a string-valued `#[webidl(<name> = "...")]` setting.
fn webidl_string(
  attrs: &[syn::Attribute],
  name: &str,
) -> syn::Result<Option<String>> {
  let mut value = None;
  for attr in attrs.iter().filter(|a| a.path().is_ident("webidl")) {
    attr.parse_nested_meta(|meta| {
      if meta.path.is_ident(name) {
        let literal: LitStr = meta.value()?.parse()?;
        value = Some(literal.value());
        Ok(())
      } else {
        Err(meta.error(format!("unknown #[webidl] key, expected `{name}`")))
      }
    })?;
  }
  Ok(value)
}

fn member(field: &syn::Field) -> syn::Result<Member> {
  let ident = field
    .ident
    .clone()
    .ok_or_else(|| syn::Error::new_spanned(field, "expected a named field."))?;
  let mut rename = None;
  let mut default = None;
  let mut required = false;
  for attr in field.attrs.iter().filter(|a| a.path().is_ident("webidl")) {
    attr.parse_nested_meta(|meta| {
      if meta.path.is_ident("rename") {
        let literal: LitStr = meta.value()?.parse()?;
        rename = Some(literal.value());
      } else if meta.path.is_ident("default") {
        default = Some(meta.value()?.parse::<Expr>()?);
      } else if meta.path.is_ident("required") {
        required = true;
      } else {
        return Err(meta.error(
          "unknown #[webidl] key for a member, expected `rename`, `default` or `required`",
        ));
      }
      Ok(())
    })?;
  }
  if required && default.is_some() {
    return Err(syn::Error::new_spanned(
      field,
      "#[webidl(required)] can not be combined with #[webidl(default = ...)].",
    ));
  }

  let mut options = Vec::new();
  for attr in field.attrs.iter().filter(|a| a.path().is_ident("options")) {
    attr.parse_nested_meta(|meta| {
      let Some(name) = meta.path.get_ident().cloned() else {
        return Err(meta.error("expected an option name"));
      };
      let value: Expr = meta.value()?.parse()?;
      options.push((name, value));
      Ok(())
    })?;
  }

  let name = ident.unraw().to_string();
  let key = match rename {
    Some(rename) => rename,
    None => stringcase::camel_case(&name),
  };
  Ok(Member {
    ident,
    key,
    option_inner: option_inner(&field.ty).cloned(),
    ty: field.ty.clone(),
    default,
    required,
    options,
  })
}

/// Returns `T` for a type written as `Option<T>`.
fn option_inner(ty: &Type) -> Option<&Type> {
  let Type::Path(path) = ty else {
    return None;
  };
  if path.qself.is_some() {
    return None;
  }
  let segment = path.path.segments.last()?;
  if segment.ident != "Option" {
    return None;
  }
  let PathArguments::AngleBracketed(args) = &segment.arguments else {
    return None;
  };
  match args.args.first()? {
    GenericArgument::Type(inner) if args.args.len() == 1 => Some(inner),
    _ => None,
  }
}

fn dictionary(input: &DeriveInput, mut members: Vec<Member>) -> TokenStream {
  let name = &input.ident;
  let name_string = name.unraw().to_string();
  let name_str = name_string.as_str();
  // Members are read in lexicographic order of their JS keys, as WebIDL
  // requires for dictionary conversion.
  members.sort_by(|a, b| a.key.cmp(&b.key));

  // Each member is bound under a generated name so that a member called
  // `scope`, `prefix`, `context` and so on cannot shadow the parameters and
  // locals used by the generated code.
  let reads = members.iter().enumerate().map(|(index, member)| {
    let binding = member_binding(index);
    let key = member.key.as_str();
    let ty = &member.ty;
    let inner = member.option_inner.as_ref().unwrap_or(ty);

    let options = if member.options.is_empty() {
      quote! {
        let __options = <#inner as ::deno_core::webidl::WebIdlConverter<'a>>::Options::default();
      }
    } else {
      let assignments = member.options.iter().map(|(option, value)| {
        quote! { __options.#option = #value; }
      });
      quote! {
        let mut __options = <#inner as ::deno_core::webidl::WebIdlConverter<'a>>::Options::default();
        #(#assignments)*
      }
    };

    let missing = if let Some(default) = &member.default {
      quote! { #default }
    } else if member.option_inner.is_some() && !member.required {
      quote! { None }
    } else {
      quote! {
        return Err(::deno_core::webidl::derive_support::required_member(
          prefix.clone(),
          context(),
          #name_str,
          #key,
        ))
      }
    };

    let present = if member.option_inner.is_some() {
      quote! {
        if __value.is_null() {
          None
        } else {
          Some(<#inner as ::deno_core::webidl::WebIdlConverter<'a>>::convert(
            scope,
            __value,
            prefix.clone(),
            &__context,
            &__options,
          )?)
        }
      }
    } else {
      quote! {
        <#ty as ::deno_core::webidl::WebIdlConverter<'a>>::convert(
          scope,
          __value,
          prefix.clone(),
          &__context,
          &__options,
        )?
      }
    };

    quote! {
      let #binding: #ty = {
        let __context = || ::deno_core::webidl::derive_support::member_context(
          #key,
          #name_str,
          context(),
        );
        let __value = ::deno_core::webidl::derive_support::dictionary_member(
          scope,
          __object,
          #key,
          &prefix,
          &__context,
        )?;
        #options
        if __value.is_undefined() {
          #missing
        } else {
          #present
        }
      };
    }
  });

  let fields = members.iter().enumerate().map(|(index, member)| {
    let ident = &member.ident;
    let binding = member_binding(index);
    quote! { #ident: #binding }
  });
  quote! {
    impl<'a> ::deno_core::webidl::WebIdlConverter<'a> for #name {
      type Options = ();

      #[allow(unused_variables, unused_mut)]
      fn convert<C>(
        scope: &mut ::deno_core::v8::HandleScope<'a>,
        value: ::deno_core::v8::Local<'a, ::deno_core::v8::Value>,
        prefix: ::std::borrow::Cow<'static, str>,
        context: C,
        _options: &Self::Options,
      ) -> ::std::result::Result<Self, ::deno_core::webidl::WebIdlError>
      where
        C: Fn() -> ::std::borrow::Cow<'static, str>,
      {
        let __object = ::deno_core::webidl::derive_support::dictionary_object(
          value,
          #name_str,
          &prefix,
          &context,
        )?;
        #(#reads)*
        Ok(Self { #(#fields),* })
      }
    }
  }
}

/// The local name a dictionary member is bound to while it is converted. It
/// is built from the member's position, so it is unique and never clashes
/// with a member name.
fn member_binding(index: usize) -> Ident {
  format_ident!("__member_{}", index)
}

fn enum_impl(
  input: &DeriveInput,
  variants: Vec<(Ident, String)>,
) -> TokenStream {
  let name = &input.ident;
  let name_string = name.unraw().to_string();
  let name_str = name_string.as_str();
  let arms = variants.iter().map(|(variant, value)| {
    let value = value.as_str();
    quote! { #value => Ok(Self::#variant), }
  });
  quote! {
    impl<'a> ::deno_core::webidl::WebIdlConverter<'a> for #name {
      type Options = ();

      fn convert<C>(
        scope: &mut ::deno_core::v8::HandleScope<'a>,
        value: ::deno_core::v8::Local<'a, ::deno_core::v8::Value>,
        prefix: ::std::borrow::Cow<'static, str>,
        context: C,
        _options: &Self::Options,
      ) -> ::std::result::Result<Self, ::deno_core::webidl::WebIdlError>
      where
        C: Fn() -> ::std::borrow::Cow<'static, str>,
      {
        let __string = ::deno_core::webidl::derive_support::enum_string(
          scope, value, &prefix, &context,
        )?;
        match __string.as_str() {
          #(#arms)*
          _ => Err(::deno_core::webidl::derive_support::invalid_enum_value(
            prefix,
            context(),
            &__string,
            #name_str,
          )),
        }
      }
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use pretty_assertions::assert_eq;
  use std::path::PathBuf;
  use syn::File;
  use syn::Item;

  fn derives_webidl(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|attr| {
      attr.path().is_ident("derive")
        && attr
          .meta
          .require_list()
          .map(|list| list.tokens.to_string().contains("WebIDL"))
          .unwrap_or(false)
    })
  }

  #[testing_macros::fixture("webidl/test_cases/*.rs")]
  fn test_proc_macro_webidl(input: PathBuf) {
    let update_expected = std::env::var("UPDATE_EXPECTED").is_ok();

    let source =
      std::fs::read_to_string(&input).expect("Failed to read test file");

    const PRELUDE: &str = r"// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.
#![deny(warnings)]
deno_ops_compile_test_runner::prelude!();";

    if !source.starts_with(PRELUDE) {
      panic!("Source does not start with expected prelude:\n{PRELUDE}");
    }

    let file =
      syn::parse_str::<File>(&source).expect("Failed to parse Rust file");
    let mut expected_out = vec![];
    for item in file.items {
      let derive_input = match item {
        Item::Struct(item) if derives_webidl(&item.attrs) => {
          DeriveInput::from(item)
        }
        Item::Enum(item) if derives_webidl(&item.attrs) => {
          DeriveInput::from(item)
        }
        _ => continue,
      };
      let tokens = derive(derive_input).expect("Failed to expand derive");
      let tree: File = syn::parse2(tokens).expect("Failed to parse expansion");
      expected_out.push(prettyplease::unparse(&tree));
    }

    let expected_out = expected_out.join("\n");

    if update_expected {
      std::fs::write(input.with_extension("out"), expected_out)
        .expect("Failed to write expectation file");
    } else {
      let expected = std::fs::read_to_string(input.with_extension("out"))
        .expect("Failed to read expectation file");
      assert_eq!(
        expected, expected_out,
        "Failed to match expectation. Use UPDATE_EXPECTED=1."
      );
    }
  }

  fn derive_error(source: &str) -> String {
    let input = syn::parse_str::<DeriveInput>(source).expect("valid Rust");
    derive(input)
      .expect_err("expected a compile error")
      .to_string()
  }

  #[test]
  fn error_requires_item_kind() {
    assert_eq!(
      derive_error("struct A { b: u8 }"),
      "#[derive(WebIDL)] requires #[webidl(dictionary)] or #[webidl(enum)]."
    );
  }

  #[test]
  fn error_dictionary_must_have_named_fields() {
    for source in [
      "#[webidl(dictionary)] struct A(u8);",
      "#[webidl(dictionary)] struct A;",
    ] {
      assert_eq!(
        derive_error(source),
        "#[webidl(dictionary)] requires a struct with named fields."
      );
    }
  }

  #[test]
  fn error_kind_matches_item() {
    assert_eq!(
      derive_error("#[webidl(dictionary)] enum A { B }"),
      "#[webidl(dictionary)] can only be used on a struct."
    );
    assert_eq!(
      derive_error("#[webidl(enum)] struct A { b: u8 }"),
      "#[webidl(enum)] can only be used on an enum."
    );
  }

  #[test]
  fn error_enum_variants_must_be_unit() {
    assert_eq!(
      derive_error("#[webidl(enum)] enum A { B(u8) }"),
      "#[webidl(enum)] only supports unit variants."
    );
    assert_eq!(
      derive_error("#[webidl(enum)] enum A {}"),
      "#[webidl(enum)] requires at least one variant."
    );
  }

  #[test]
  fn error_unknown_keys() {
    assert_eq!(
      derive_error("#[webidl(foo)] struct A { b: u8 }"),
      "unknown #[webidl] key for an item, expected `dictionary` or `enum`"
    );
    assert_eq!(
      derive_error("#[webidl(dictionary)] struct A { #[webidl(foo)] b: u8 }"),
      "unknown #[webidl] key for a member, expected `rename`, `default` or `required`"
    );
    assert_eq!(
      derive_error("#[webidl(enum)] enum A { #[webidl(default)] B }"),
      "unknown #[webidl] key, expected `rename`"
    );
  }

  #[test]
  fn error_required_with_default() {
    assert_eq!(
      derive_error(
        "#[webidl(dictionary)] struct A { #[webidl(required, default = 1)] b: u8 }"
      ),
      "#[webidl(required)] can not be combined with #[webidl(default = ...)]."
    );
  }

  #[test]
  fn error_generics_are_not_supported() {
    assert_eq!(
      derive_error("#[webidl(dictionary)] struct A<T> { b: T }"),
      "#[derive(WebIDL)] does not support generic types."
    );
  }

  #[test]
  fn error_duplicate_member_keys() {
    assert_eq!(
      derive_error(
        "#[webidl(dictionary)] struct A { r#type: u8, #[webidl(rename = \"type\")] kind: u8 }"
      ),
      "two members map to the JS key 'type', use #[webidl(rename = \"...\")] to give one of them another key."
    );
  }

  #[test]
  fn error_duplicate_variant_values() {
    assert_eq!(
      derive_error(
        "#[webidl(enum)] enum A { Foo, #[webidl(rename = \"foo\")] Bar }"
      ),
      "two variants map to the JS key 'foo', use #[webidl(rename = \"...\")] to give one of them another key."
    );
  }

  #[test]
  fn error_empty_keys() {
    assert_eq!(
      derive_error("#[webidl(dictionary)] struct A { __: u8 }"),
      "the JS key of this member is empty, use #[webidl(rename = \"...\")] to give it a name."
    );
    assert_eq!(
      derive_error("#[webidl(enum)] enum A { __ }"),
      "the JS key of this variant is empty, use #[webidl(rename = \"...\")] to give it a name."
    );
  }

  #[test]
  fn option_inner_detects_option_only() {
    let ty: Type = syn::parse_str("Option<u8>").unwrap();
    assert!(option_inner(&ty).is_some());
    let ty: Type = syn::parse_str("std::option::Option<Vec<u8>>").unwrap();
    assert!(option_inner(&ty).is_some());
    let ty: Type = syn::parse_str("Vec<u8>").unwrap();
    assert!(option_inner(&ty).is_none());
  }
}
