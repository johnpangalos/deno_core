// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.

//! The `#[derive(WebIDL)]` macro, which generates a
//! `deno_core::webidl::WebIdlConverter` implementation for dictionaries and
//! enums.
//!
//! Attributes:
//!
//! - `#[webidl(dictionary)]` on a struct with named fields. Each field is a
//!   dictionary member.
//! - `#[webidl(enum)]` on an enum whose variants are all unit variants. Each
//!   variant is an enum value.
//! - `#[webidl(rename = "key")]` on a field or variant sets the JavaScript
//!   key or value. Fields default to their name in camelCase, and variants to
//!   their name in kebab-case.
//! - `#[webidl(required)]` on a field makes the member mandatory. Missing or
//!   `undefined` members are an error. Cannot be combined with `default`.
//! - `#[webidl(default = <expr>)]` on a field is used when the member is
//!   missing or `undefined`. Cannot be combined with `required`.
//! - `#[options(<name> = <expr>, ...)]` on a field sets options for the
//!   member's conversion. Supported names are `clamp`, `enforce_range` and
//!   `treat_null_as_empty_string`.
//!
//! A field of type `Option<T>` is optional: a missing or `null` member is
//! `None`. Any other missing member without `default` is an error.

use proc_macro2::TokenStream;
use quote::quote;
use syn::ext::IdentExt;
use syn::spanned::Spanned;
use syn::Data;
use syn::DeriveInput;
use syn::Fields;
use syn::LitStr;

const OPTION_NAMES: [&str; 3] =
  ["clamp", "enforce_range", "treat_null_as_empty_string"];

pub(crate) fn derive(input: DeriveInput) -> syn::Result<TokenStream> {
  let kind = container_kind(&input)?;
  if !input.generics.params.is_empty() {
    return Err(syn::Error::new(
      input.generics.span(),
      "WebIDL derive does not support generic types",
    ));
  }
  match kind {
    Kind::Dictionary => derive_dictionary(&input),
    Kind::Enum => derive_enum(&input),
  }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
  Dictionary,
  Enum,
}

fn container_kind(input: &DeriveInput) -> syn::Result<Kind> {
  let mut kind = None;
  for attr in input.attrs.iter().filter(|a| a.path().is_ident("webidl")) {
    attr.parse_nested_meta(|meta| {
      let this = if meta.path.is_ident("dictionary") {
        Kind::Dictionary
      } else if meta.path.is_ident("enum") {
        Kind::Enum
      } else {
        return Err(
          meta
            .error("unknown webidl attribute, expected `dictionary` or `enum`"),
        );
      };
      if kind.is_some() {
        return Err(
          meta.error("expected exactly one of `dictionary` or `enum`"),
        );
      }
      kind = Some(this);
      Ok(())
    })?;
  }
  kind.ok_or_else(|| {
    syn::Error::new(
      input.ident.span(),
      "WebIDL derive requires `#[webidl(dictionary)]` or `#[webidl(enum)]`",
    )
  })
}

#[derive(Default)]
struct MemberAttrs {
  rename: Option<LitStr>,
  required: bool,
  default: Option<syn::Expr>,
  options: Vec<(syn::Ident, syn::Expr)>,
}

fn parse_member_attrs(
  attrs: &[syn::Attribute],
  allow_field_attrs: bool,
) -> syn::Result<MemberAttrs> {
  let mut out = MemberAttrs::default();
  for attr in attrs {
    if attr.path().is_ident("webidl") {
      attr.parse_nested_meta(|meta| {
        if meta.path.is_ident("rename") {
          out.rename = Some(meta.value()?.parse()?);
        } else if allow_field_attrs && meta.path.is_ident("required") {
          out.required = true;
        } else if allow_field_attrs && meta.path.is_ident("default") {
          out.default = Some(meta.value()?.parse()?);
        } else {
          return Err(meta.error(if allow_field_attrs {
            "unknown webidl attribute, expected `rename`, `required` or `default`"
          } else {
            "unknown webidl attribute, expected `rename`"
          }));
        }
        Ok(())
      })?;
    } else if allow_field_attrs && attr.path().is_ident("options") {
      attr.parse_nested_meta(|meta| {
        let name = meta
          .path
          .get_ident()
          .ok_or_else(|| meta.error("expected an option name"))?
          .clone();
        if !OPTION_NAMES.contains(&name.to_string().as_str()) {
          return Err(meta.error(format!(
            "unknown option `{name}`, expected one of {}",
            OPTION_NAMES.join(", ")
          )));
        }
        let value: syn::Expr = meta.value()?.parse()?;
        out.options.push((name, value));
        Ok(())
      })?;
    }
  }
  if out.required && out.default.is_some() {
    return Err(syn::Error::new(
      attrs[0].span(),
      "a member cannot be both `default` and `required`: the spec forbids a default on a required member",
    ));
  }
  Ok(out)
}

/// Returns `T` for `Option<T>`.
fn option_inner(ty: &syn::Type) -> Option<&syn::Type> {
  let syn::Type::Path(path) = ty else {
    return None;
  };
  let segment = path.path.segments.last()?;
  if segment.ident != "Option" {
    return None;
  }
  let syn::PathArguments::AngleBracketed(args) = &segment.arguments else {
    return None;
  };
  match args.args.first() {
    Some(syn::GenericArgument::Type(inner)) if args.args.len() == 1 => {
      Some(inner)
    }
    _ => None,
  }
}

fn derive_dictionary(input: &DeriveInput) -> syn::Result<TokenStream> {
  let Data::Struct(data) = &input.data else {
    return Err(syn::Error::new(
      input.ident.span(),
      "`#[webidl(dictionary)]` can only be used on a struct",
    ));
  };
  let Fields::Named(fields) = &data.fields else {
    return Err(syn::Error::new(
      input.ident.span(),
      "`#[webidl(dictionary)]` requires a struct with named fields",
    ));
  };

  struct Member {
    key: String,
    ident: syn::Ident,
    ty: syn::Type,
    attrs: MemberAttrs,
  }

  let mut members = vec![];
  for field in &fields.named {
    let attrs = parse_member_attrs(&field.attrs, true)?;
    let ident = field.ident.clone().expect("named field");
    let key = match &attrs.rename {
      Some(rename) => rename.value(),
      None => stringcase::camel_case(&ident.unraw().to_string()),
    };
    members.push(Member {
      key,
      ident,
      ty: field.ty.clone(),
      attrs,
    });
  }
  // WebIDL reads members in the lexicographic order of their keys.
  members.sort_by(|a, b| a.key.encode_utf16().cmp(b.key.encode_utf16()));

  let name = &input.ident;
  let name_str = name.to_string();
  let mut member_code = vec![];
  let mut field_inits = vec![];
  for (index, member) in members.iter().enumerate() {
    let raw = quote::format_ident!("__webidl_raw_{}", index);
    let var = quote::format_ident!("__webidl_value_{}", index);
    let key = &member.key;
    let (inner_ty, is_option) = match option_inner(&member.ty) {
      Some(inner) => (inner, true),
      None => (&member.ty, false),
    };
    let field_ty = &member.ty;
    let options_expr = if member.attrs.options.is_empty() {
      quote! { &::std::default::Default::default() }
    } else {
      let sets = member.attrs.options.iter().map(|(name, value)| {
        quote! { __options.#name = #value; }
      });
      quote! {
        &{
          let mut __options = <<#inner_ty as ::deno_core::webidl::WebIdlConverter<'a>>::Options as ::std::default::Default>::default();
          #(#sets)*
          __options
        }
      }
    };
    let convert = quote! {
      <#inner_ty as ::deno_core::webidl::WebIdlConverter<'a>>::convert(
        scope,
        __webidl_present,
        prefix.clone(),
        || ::std::borrow::Cow::Owned(format!("{} member \"{}\"", context(), #key)),
        #options_expr,
      )
    };
    let present = if is_option {
      quote! {
        if __webidl_present.is_null() {
          None
        } else {
          Some(#convert?)
        }
      }
    } else {
      quote! { #convert? }
    };
    let missing_message = format!(
      "is missing required member \"{key}\" of dictionary \"{name_str}\"."
    );
    let missing = if let Some(default) = &member.attrs.default {
      quote! { #default }
    } else if is_option && !member.attrs.required {
      quote! { None }
    } else {
      quote! {
        return Err(::deno_core::webidl::WebIdlError::new(
          prefix.clone(),
          context(),
          #missing_message,
        ))
      }
    };
    member_code.push(quote! {
      let #raw = ::deno_core::webidl::dictionary_member(
        scope,
        object,
        #key,
        &prefix,
        &context,
      )?;
      let #var: #field_ty = match #raw {
        Some(__webidl_present) => { #present }
        None => { #missing }
      };
    });
    let ident = &member.ident;
    field_inits.push(quote! { #ident: #var });
  }

  Ok(quote! {
    impl<'a> ::deno_core::webidl::WebIdlConverter<'a> for #name {
      type Options = ();

      fn convert<C>(
        scope: &mut ::deno_core::v8::HandleScope<'a>,
        value: ::deno_core::v8::Local<'a, ::deno_core::v8::Value>,
        prefix: ::std::borrow::Cow<'static, str>,
        context: C,
        _options: &(),
      ) -> ::std::result::Result<Self, ::deno_core::webidl::WebIdlError>
      where
        C: Fn() -> ::std::borrow::Cow<'static, str>,
      {
        let object = ::deno_core::webidl::dictionary_object(value, &prefix, &context)?;
        let _ = (&scope, &prefix, &context);
        #(#member_code)*
        Ok(Self {
          #(#field_inits,)*
        })
      }
    }
  })
}

fn derive_enum(input: &DeriveInput) -> syn::Result<TokenStream> {
  let Data::Enum(data) = &input.data else {
    return Err(syn::Error::new(
      input.ident.span(),
      "`#[webidl(enum)]` can only be used on an enum",
    ));
  };

  let mut arms = vec![];
  for variant in &data.variants {
    if !matches!(variant.fields, Fields::Unit) {
      return Err(syn::Error::new(
        variant.span(),
        "`#[webidl(enum)]` requires every variant to be a unit variant",
      ));
    }
    let attrs = parse_member_attrs(&variant.attrs, false)?;
    let value = match &attrs.rename {
      Some(rename) => rename.value(),
      None => stringcase::kebab_case(&variant.ident.unraw().to_string()),
    };
    let ident = &variant.ident;
    arms.push(quote! { #value => Ok(Self::#ident), });
  }

  let name_ident = &input.ident;
  let name = name_ident.to_string();
  let not_valid = format!("is not a valid value for enum \"{name}\": ");
  Ok(quote! {
    impl<'a> ::deno_core::webidl::WebIdlConverter<'a> for #name_ident {
      type Options = ();

      fn convert<C>(
        scope: &mut ::deno_core::v8::HandleScope<'a>,
        value: ::deno_core::v8::Local<'a, ::deno_core::v8::Value>,
        prefix: ::std::borrow::Cow<'static, str>,
        context: C,
        _options: &(),
      ) -> ::std::result::Result<Self, ::deno_core::webidl::WebIdlError>
      where
        C: Fn() -> ::std::borrow::Cow<'static, str>,
      {
        let string = ::deno_core::webidl::to_string_value(scope, value, &prefix, &context)?;
        match string.as_str() {
          #(#arms)*
          _ => Err(::deno_core::webidl::WebIdlError::new(
            prefix,
            context(),
            format!("{}\"{}\".", #not_valid, string),
          )),
        }
      }
    }
  })
}

#[cfg(test)]
mod tests {
  use std::path::PathBuf;

  use syn::File;
  use syn::Item;

  fn derive_error(source: &str) -> String {
    let input = syn::parse_str::<syn::DeriveInput>(source).unwrap();
    super::derive(input).unwrap_err().to_string()
  }

  #[test]
  fn derive_errors() {
    assert_eq!(
      derive_error("struct A { x: u8 }"),
      "WebIDL derive requires `#[webidl(dictionary)]` or `#[webidl(enum)]`"
    );
    assert_eq!(
      derive_error("#[webidl(dictionary)] struct A(u8);"),
      "`#[webidl(dictionary)]` requires a struct with named fields"
    );
    assert_eq!(
      derive_error("#[webidl(dictionary)] struct A;"),
      "`#[webidl(dictionary)]` requires a struct with named fields"
    );
    assert_eq!(
      derive_error("#[webidl(dictionary)] enum A { B }"),
      "`#[webidl(dictionary)]` can only be used on a struct"
    );
    assert_eq!(
      derive_error("#[webidl(enum)] struct A { x: u8 }"),
      "`#[webidl(enum)]` can only be used on an enum"
    );
    assert_eq!(
      derive_error("#[webidl(enum)] enum A { B(u8) }"),
      "`#[webidl(enum)]` requires every variant to be a unit variant"
    );
    assert_eq!(
      derive_error("#[webidl(dictionary)] struct A { #[webidl(bogus)] x: u8 }"),
      "unknown webidl attribute, expected `rename`, `required` or `default`"
    );
    assert_eq!(
      derive_error("#[webidl(enum)] enum A { #[webidl(required)] B }"),
      "unknown webidl attribute, expected `rename`"
    );
    assert_eq!(
      derive_error("#[webidl(dictionary)] struct A { #[webidl(required, default = 1)] x: u8 }"),
      "a member cannot be both `default` and `required`: the spec forbids a default on a required member"
    );
    assert_eq!(
      derive_error("#[webidl(dictionary)] struct A { #[options(bogus = true)] x: u8 }"),
      "unknown option `bogus`, expected one of clamp, enforce_range, treat_null_as_empty_string"
    );
    assert_eq!(
      derive_error("#[webidl(dictionary, enum)] struct A { x: u8 }"),
      "expected exactly one of `dictionary` or `enum`"
    );
    assert_eq!(
      derive_error("#[webidl(dictionary)] struct A<T> { x: T }"),
      "WebIDL derive does not support generic types"
    );
  }

  #[testing_macros::fixture("webidl/test_cases/*.rs")]
  fn test_proc_macro_webidl(input: PathBuf) {
    let update_expected = std::env::var("UPDATE_EXPECTED").is_ok();
    let source =
      std::fs::read_to_string(&input).expect("Failed to read test file");
    const PRELUDE: &str =
      "// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.";
    if !source.starts_with(PRELUDE) {
      panic!("Source does not start with expected prelude:\n{PRELUDE}");
    }
    let file =
      syn::parse_str::<File>(&source).expect("Failed to parse Rust file");
    let mut expected_out = vec![];
    for item in file.items {
      let input = match item {
        Item::Struct(item) => syn::DeriveInput::from(item),
        Item::Enum(item) => syn::DeriveInput::from(item),
        _ => continue,
      };
      let tokens = super::derive(input).expect("Failed to generate impl");
      let tree = syn::parse2(tokens).unwrap();
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
}
