// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.

//! Expansion of `#[derive(WebIDL)]`.
//!
//! A struct marked `#[webidl(dictionary)]` becomes a WebIDL dictionary. Its
//! members are read in the order of their JavaScript keys, and each member is
//! converted as soon as it is read. An enum marked `#[webidl(enum)]` becomes a
//! WebIDL enumeration whose string value is matched exactly against each
//! variant's kebab-case name.
//!
//! Generated code uses only fully qualified `::deno_core` and `::std` paths,
//! and every local it binds starts with an underscore, so it compiles under
//! `#![deny(warnings)]` in a crate that depends only on `deno_core`.

use std::collections::HashSet;

use proc_macro2::TokenStream;
use quote::format_ident;
use quote::quote;
use quote::ToTokens;
use syn::ext::IdentExt;
use syn::meta::ParseNestedMeta;
use syn::Data;
use syn::DataEnum;
use syn::DataStruct;
use syn::DeriveInput;
use syn::Expr;
use syn::Field;
use syn::Fields;
use syn::FieldsNamed;
use syn::Ident;
use syn::LitStr;
use syn::Type;

#[derive(Clone, Copy, PartialEq, Eq)]
enum ItemKind {
  Dictionary,
  Enum,
}

/// A dictionary member, in declaration order.
struct Member {
  /// The field name, used in the struct literal.
  ident: Ident,
  /// The JavaScript property name.
  key: String,
  ty: Type,
  default: Option<Expr>,
  required: bool,
  options: Vec<(Ident, Expr)>,
  /// The local that holds the converted member.
  value: Ident,
  /// The local closure that builds the member's error context.
  context: Ident,
}

/// Expands `#[derive(WebIDL)]`, or returns an error that points at the part of
/// the input that is wrong.
pub(crate) fn derive(input: DeriveInput) -> syn::Result<TokenStream> {
  let kind = item_kind(&input)?;
  if let Some(attr) = input
    .attrs
    .iter()
    .find(|attr| attr.path().is_ident("options"))
  {
    return Err(syn::Error::new_spanned(
      attr,
      "#[options] is only valid on dictionary fields",
    ));
  }
  if !input.generics.params.is_empty() {
    return Err(syn::Error::new_spanned(
      &input.generics,
      "#[derive(WebIDL)] does not support generic or lifetime parameters",
    ));
  }
  let name = &input.ident;
  let body =
    match (kind, &input.data) {
      (
        ItemKind::Dictionary,
        Data::Struct(DataStruct {
          fields: Fields::Named(fields),
          ..
        }),
      ) => dictionary_body(name, fields)?,
      (ItemKind::Dictionary, _) => return Err(syn::Error::new_spanned(
        name,
        "#[webidl(dictionary)] can only be used on a struct with named fields",
      )),
      (ItemKind::Enum, Data::Enum(data)) => enum_body(name, data)?,
      (ItemKind::Enum, _) => {
        return Err(syn::Error::new_spanned(
          name,
          "#[webidl(enum)] can only be used on an enum",
        ))
      }
    };
  Ok(quote! {
    impl<'a> ::deno_core::webidl::WebIdlConverter<'a> for #name {
      type Options = ();

      fn convert<C>(
        __scope: &mut ::deno_core::v8::HandleScope<'a>,
        __value: ::deno_core::v8::Local<'a, ::deno_core::v8::Value>,
        __prefix: ::std::borrow::Cow<'static, str>,
        __context: C,
        _options: &Self::Options,
      ) -> ::std::result::Result<Self, ::deno_core::webidl::WebIdlError>
      where
        C: Fn() -> ::std::borrow::Cow<'static, str>,
      {
        #body
      }
    }
  })
}

fn item_kind(input: &DeriveInput) -> syn::Result<ItemKind> {
  let mut kind = None;
  for attr in input
    .attrs
    .iter()
    .filter(|attr| attr.path().is_ident("webidl"))
  {
    attr.parse_nested_meta(|meta| {
      let parsed = if meta.path.is_ident("dictionary") {
        ItemKind::Dictionary
      } else if meta.path.is_ident("enum") {
        ItemKind::Enum
      } else {
        return Err(unknown_key(&meta, "dictionary or enum"));
      };
      if kind.replace(parsed).is_some() {
        return Err(meta.error("expected only one of `dictionary` or `enum`"));
      }
      Ok(())
    })?;
  }
  kind.ok_or_else(|| {
    syn::Error::new_spanned(
      &input.ident,
      "#[derive(WebIDL)] requires #[webidl(dictionary)] or #[webidl(enum)]",
    )
  })
}

fn unknown_key(meta: &ParseNestedMeta, expected: &str) -> syn::Error {
  let key = meta.path.to_token_stream().to_string();
  meta.error(format!(
    "unknown webidl attribute `{key}`, expected {expected}"
  ))
}

fn dictionary_body(
  name: &Ident,
  fields: &FieldsNamed,
) -> syn::Result<TokenStream> {
  let mut members = Vec::new();
  for (index, field) in fields.named.iter().enumerate() {
    members.push(parse_member(field, index)?);
  }
  let mut keys = HashSet::new();
  for member in &members {
    if !keys.insert(member.key.as_str()) {
      return Err(syn::Error::new_spanned(
        &member.ident,
        format!("two fields map to the JavaScript key '{}'", member.key),
      ));
    }
  }

  // The spec reads members in the order of their keys, so sort at expansion
  // time. Each member is read and converted before the next one is read.
  let dictionary = name.to_string();
  let mut sorted: Vec<&Member> = members.iter().collect();
  sorted.sort_by(|a, b| a.key.cmp(&b.key));
  let reads = sorted
    .iter()
    .map(|member| member_read(member, &dictionary))
    .collect::<Vec<_>>();
  let inits = members
    .iter()
    .map(|member| {
      let ident = &member.ident;
      let value = &member.value;
      quote!(#ident: #value)
    })
    .collect::<Vec<_>>();

  Ok(quote! {
    let __object = ::deno_core::webidl::dictionary_object(__value, __prefix.clone(), &__context)?;
    #(#reads)*
    ::std::result::Result::Ok(Self { #(#inits),* })
  })
}

fn parse_member(field: &Field, index: usize) -> syn::Result<Member> {
  let Some(ident) = field.ident.clone() else {
    return Err(syn::Error::new_spanned(
      field,
      "#[webidl(dictionary)] fields must be named",
    ));
  };
  let mut rename = None;
  let mut default = None;
  let mut required = false;
  for attr in field
    .attrs
    .iter()
    .filter(|attr| attr.path().is_ident("webidl"))
  {
    attr.parse_nested_meta(|meta| {
      if meta.path.is_ident("rename") {
        rename = Some(meta.value()?.parse::<LitStr>()?.value());
      } else if meta.path.is_ident("default") {
        default = Some(meta.value()?.parse::<Expr>()?);
      } else if meta.path.is_ident("required") {
        required = true;
      } else {
        return Err(unknown_key(&meta, "rename, default or required"));
      }
      Ok(())
    })?;
  }
  if required && default.is_some() {
    return Err(syn::Error::new_spanned(
      &ident,
      "`required` and `default` can not be used together",
    ));
  }
  let mut options = Vec::new();
  for attr in field
    .attrs
    .iter()
    .filter(|attr| attr.path().is_ident("options"))
  {
    attr.parse_nested_meta(|meta| {
      let Some(option) = meta.path.get_ident().cloned() else {
        return Err(
          meta.error("expected an option name, such as `clamp = true`"),
        );
      };
      options.push((option, meta.value()?.parse::<Expr>()?));
      Ok(())
    })?;
  }
  let key = rename
    .unwrap_or_else(|| stringcase::camel_case(&ident.unraw().to_string()));
  Ok(Member {
    ident,
    key,
    ty: field.ty.clone(),
    default,
    required,
    options,
    value: format_ident!("__field_{}", index),
    context: format_ident!("__member_context_{}", index),
  })
}

/// Generates the statements that read, convert and bind one member.
fn member_read(member: &Member, dictionary: &str) -> TokenStream {
  let key = &member.key;
  let ty = &member.ty;
  let value = &member.value;
  let context = &member.context;
  let context_format = format!(
    "'{}' of '{}' ({{}})",
    escape_braces(key),
    escape_braces(dictionary)
  );
  let options_type =
    quote!(<#ty as ::deno_core::webidl::WebIdlConverter<'a>>::Options);
  let options = if member.options.is_empty() {
    quote! {
      let __options: #options_type = ::std::default::Default::default();
    }
  } else {
    let assignments = member
      .options
      .iter()
      .map(|(name, expr)| quote!(__options.#name = #expr;))
      .collect::<Vec<_>>();
    quote! {
      #[allow(clippy::field_reassign_with_default)]
      let mut __options: #options_type = ::std::default::Default::default();
      #(#assignments)*
    }
  };
  let missing = match &member.default {
    Some(default) => quote!({ #default }),
    None if is_option(ty) && !member.required => {
      quote!(::std::option::Option::None)
    }
    None => quote! {
      return ::std::result::Result::Err(::deno_core::webidl::WebIdlError::new(
        __prefix.clone(),
        &__context,
        ::deno_core::webidl::WebIdlErrorKind::DictionaryCannotConvertKey {
          converter: #dictionary,
          key: #key,
        },
      ))
    },
  };
  quote! {
    let #value = {
      let #context = || {
        ::std::borrow::Cow::Owned(::std::format!(#context_format, __context()))
      };
      match ::deno_core::webidl::dictionary_member(
        __scope,
        __object,
        #key,
        __prefix.clone(),
        &#context,
      )? {
        ::std::option::Option::Some(__value) => {
          #options
          <#ty as ::deno_core::webidl::WebIdlConverter<'a>>::convert(
            __scope,
            __value,
            __prefix.clone(),
            &#context,
            &__options,
          )?
        }
        ::std::option::Option::None => #missing,
      }
    };
  }
}

fn enum_body(name: &Ident, data: &DataEnum) -> syn::Result<TokenStream> {
  let mut values: Vec<String> = Vec::new();
  let mut arms = Vec::new();
  for variant in &data.variants {
    if !matches!(variant.fields, Fields::Unit) {
      return Err(syn::Error::new_spanned(
        variant,
        "#[webidl(enum)] variants must be unit variants",
      ));
    }
    if let Some(attr) = variant
      .attrs
      .iter()
      .find(|attr| attr.path().is_ident("options"))
    {
      return Err(syn::Error::new_spanned(
        attr,
        "#[options] is only valid on dictionary fields",
      ));
    }
    let mut rename = None;
    for attr in variant
      .attrs
      .iter()
      .filter(|attr| attr.path().is_ident("webidl"))
    {
      attr.parse_nested_meta(|meta| {
        if meta.path.is_ident("rename") {
          rename = Some(meta.value()?.parse::<LitStr>()?.value());
          Ok(())
        } else {
          Err(unknown_key(&meta, "rename"))
        }
      })?;
    }
    let value = rename.unwrap_or_else(|| {
      stringcase::kebab_case(&variant.ident.unraw().to_string())
    });
    if values.contains(&value) {
      return Err(syn::Error::new_spanned(
        &variant.ident,
        format!("two variants map to the value '{value}'"),
      ));
    }
    let ident = &variant.ident;
    arms.push(quote!(#value => ::std::result::Result::Ok(Self::#ident),));
    values.push(value);
  }

  let enumeration = name.to_string();
  Ok(quote! {
    let __string = <::std::string::String as ::deno_core::webidl::WebIdlConverter<'a>>::convert(
      __scope,
      __value,
      __prefix.clone(),
      &__context,
      &::std::default::Default::default(),
    )?;
    match __string.as_str() {
      #(#arms)*
      _ => ::std::result::Result::Err(::deno_core::webidl::WebIdlError::new(
        __prefix,
        &__context,
        ::deno_core::webidl::WebIdlErrorKind::InvalidEnumVariant {
          converter: #enumeration,
          variant: __string,
        },
      )),
    }
  })
}

/// True when the type's last path segment is `Option`.
fn is_option(ty: &Type) -> bool {
  matches!(
    ty,
    Type::Path(path)
      if path.path.segments.last().is_some_and(|segment| segment.ident == "Option")
  )
}

/// Escapes braces so that text can be used inside a `format!` string.
fn escape_braces(text: &str) -> String {
  text.replace('{', "{{").replace('}', "}}")
}

#[cfg(test)]
mod tests {
  use super::*;
  use pretty_assertions::assert_eq;
  use std::path::PathBuf;
  use syn::parse_quote;
  use syn::File;
  use syn::Item;

  const PRELUDE: &str = r"// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.
#![deny(warnings)]
deno_ops_compile_test_runner::prelude!();";

  const GENERIC: &str =
    "#[derive(WebIDL)] does not support generic or lifetime parameters";
  const NOT_A_DICTIONARY: &str =
    "#[webidl(dictionary)] can only be used on a struct with named fields";

  fn has_webidl_derive(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|attr| {
      attr.path().is_ident("derive")
        && attr.to_token_stream().to_string().contains("WebIDL")
    })
  }

  #[testing_macros::fixture("webidl/test_cases/*.rs")]
  fn test_proc_macro_webidl(input: PathBuf) {
    let update_expected = std::env::var("UPDATE_EXPECTED").is_ok();

    let source =
      std::fs::read_to_string(&input).expect("Failed to read test file");

    if !source.starts_with(PRELUDE) {
      panic!("Source does not start with expected prelude:\n{PRELUDE}");
    }

    let file =
      syn::parse_str::<File>(&source).expect("Failed to parse Rust file");
    let mut expected_out = vec![];
    for item in file.items {
      let input = match item {
        Item::Struct(item) if has_webidl_derive(&item.attrs) => {
          DeriveInput::from(item)
        }
        Item::Enum(item) if has_webidl_derive(&item.attrs) => {
          DeriveInput::from(item)
        }
        _ => continue,
      };
      let tokens = derive(input).expect("Failed to generate WebIDL impl");
      let tree: File = syn::parse2(tokens).unwrap();
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

  fn error_message(input: DeriveInput) -> String {
    match derive(input) {
      Ok(_) => panic!("expected the derive to fail"),
      Err(error) => error.to_string(),
    }
  }

  #[test]
  fn misuse_is_a_compile_error_with_a_message() {
    let cases: Vec<(DeriveInput, &str)> = vec![
      (
        parse_quote! { struct S { a: u8 } },
        "#[derive(WebIDL)] requires #[webidl(dictionary)] or #[webidl(enum)]",
      ),
      (
        parse_quote! { #[webidl(dictionary)] struct S(u8); },
        NOT_A_DICTIONARY,
      ),
      (parse_quote! { #[webidl(dictionary)] struct S; }, NOT_A_DICTIONARY),
      (parse_quote! { #[webidl(dictionary)] enum E { A } }, NOT_A_DICTIONARY),
      (
        parse_quote! { #[webidl(dictionary)] union U { a: u8 } },
        NOT_A_DICTIONARY,
      ),
      (
        parse_quote! { #[webidl(enum)] struct S { a: u8 } },
        "#[webidl(enum)] can only be used on an enum",
      ),
      (
        parse_quote! { #[webidl(enum)] enum E { A(u8) } },
        "#[webidl(enum)] variants must be unit variants",
      ),
      (
        parse_quote! { #[webidl(dictionary)] struct S { #[webidl(bogus)] a: u8 } },
        "unknown webidl attribute `bogus`, expected rename, default or required",
      ),
      (
        parse_quote! { #[webidl(enum)] enum E { #[webidl(bogus)] A } },
        "unknown webidl attribute `bogus`, expected rename",
      ),
      (
        parse_quote! { #[webidl(bogus)] struct S { } },
        "unknown webidl attribute `bogus`, expected dictionary or enum",
      ),
      (
        parse_quote! { #[webidl(dictionary, enum)] struct S { } },
        "expected only one of `dictionary` or `enum`",
      ),
      (
        parse_quote! { #[webidl(dictionary)] struct S { #[webidl(required, default = 1)] a: u8 } },
        "`required` and `default` can not be used together",
      ),
      (
        parse_quote! { #[webidl(dictionary)] struct S<T> { a: T } },
        GENERIC,
      ),
      (
        parse_quote! { #[webidl(dictionary)] struct S<'a> { a: &'a str } },
        GENERIC,
      ),
      (parse_quote! { #[webidl(enum)] enum E<T> { A } }, GENERIC),
      (
        parse_quote! { #[webidl(dictionary)] struct S { #[webidl(rename = "x")] a: u8, #[webidl(rename = "x")] b: u8 } },
        "two fields map to the JavaScript key 'x'",
      ),
      (
        parse_quote! { #[webidl(enum)] enum E { A, #[webidl(rename = "a")] B } },
        "two variants map to the value 'a'",
      ),
      (
        parse_quote! { #[webidl(dictionary)] #[options(clamp = true)] struct S { } },
        "#[options] is only valid on dictionary fields",
      ),
    ];
    for (input, expected) in cases {
      assert_eq!(error_message(input), expected);
    }
  }
}
