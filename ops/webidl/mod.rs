// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.

//! `#[derive(WebIDL)]` for WebIDL dictionaries and enums.
//!
//! The generated code refers to `deno_core::...` paths only, and implements
//! `deno_core::webidl::WebIdlConverter` for the annotated type.

use proc_macro2::TokenStream;
use syn::Data;
use syn::DeriveInput;

mod dictionary;
mod enum_type;

/// The kind of WebIDL type a container is declared as.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
  Dictionary,
  Enum,
}

pub(crate) fn derive(input: TokenStream) -> syn::Result<TokenStream> {
  let input: DeriveInput = syn::parse2(input)?;
  if !input.generics.params.is_empty() {
    return Err(syn::Error::new_spanned(
      &input.generics,
      "generic types are not supported by #[derive(WebIDL)]",
    ));
  }
  let Some(kind) = container_kind(&input)? else {
    return Err(syn::Error::new_spanned(
      &input.ident,
      "#[derive(WebIDL)] requires #[webidl(dictionary)] or #[webidl(enum)]",
    ));
  };
  match (kind, &input.data) {
    (Kind::Dictionary, Data::Struct(data)) => dictionary::derive(&input, data),
    (Kind::Enum, Data::Enum(data)) => enum_type::derive(&input, data),
    (Kind::Dictionary, _) => Err(syn::Error::new_spanned(
      &input.ident,
      "#[webidl(dictionary)] is only valid on structs",
    )),
    (Kind::Enum, _) => Err(syn::Error::new_spanned(
      &input.ident,
      "#[webidl(enum)] is only valid on enums",
    )),
  }
}

/// Reads the `#[webidl(dictionary)]` or `#[webidl(enum)]` attribute on a container.
fn container_kind(input: &DeriveInput) -> syn::Result<Option<Kind>> {
  let mut kind = None;
  for attr in input.attrs.iter().filter(|a| a.path().is_ident("webidl")) {
    attr.parse_nested_meta(|meta| {
      let parsed = if meta.path.is_ident("dictionary") {
        Kind::Dictionary
      } else if meta.path.is_ident("enum") {
        Kind::Enum
      } else {
        return Err(meta.error(
          "unknown #[webidl(...)] key, expected `dictionary` or `enum`",
        ));
      };
      if kind.is_some() {
        return Err(meta.error(
          "only one of #[webidl(dictionary)] or #[webidl(enum)] may be used",
        ));
      }
      kind = Some(parsed);
      Ok(())
    })?;
  }
  Ok(kind)
}

#[cfg(test)]
mod tests {
  use std::path::PathBuf;

  use proc_macro2::TokenStream;
  use quote::ToTokens;
  use syn::Item;

  use super::derive;

  #[testing_macros::fixture("webidl/test_cases/*.rs")]
  fn test_proc_macro_webidl(input: PathBuf) {
    let update_expected = std::env::var("UPDATE_EXPECTED").is_ok();

    let source =
      std::fs::read_to_string(&input).expect("Failed to read test file");
    let file =
      syn::parse_str::<syn::File>(&source).expect("Failed to parse Rust file");

    let mut expected_out = vec![];
    for item in file.items {
      let input = match item {
        Item::Struct(item) => syn::DeriveInput::from(item),
        Item::Enum(item) => syn::DeriveInput::from(item),
        _ => continue,
      };
      let tokens = derive(input.to_token_stream())
        .expect("Failed to expand #[derive(WebIDL)]");
      let tree = syn::parse2(tokens.clone()).unwrap_or_else(|err| {
        panic!("Failed to parse expansion: {err}\n{tokens}")
      });
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

  fn expand(src: &str) -> syn::Result<TokenStream> {
    derive(src.parse().expect("Invalid test input"))
  }

  #[test]
  fn webidl_derive_errors() {
    let cases = [
      (
        "struct Foo { a: u8 }",
        "#[derive(WebIDL)] requires #[webidl(dictionary)] or #[webidl(enum)]",
      ),
      (
        "#[webidl(unknown)] struct Foo { a: u8 }",
        "unknown #[webidl(...)] key, expected `dictionary` or `enum`",
      ),
      (
        "#[webidl(dictionary)] #[webidl(enum)] struct Foo { a: u8 }",
        "only one of #[webidl(dictionary)] or #[webidl(enum)] may be used",
      ),
      (
        "#[webidl(dictionary)] enum Foo { A }",
        "#[webidl(dictionary)] is only valid on structs",
      ),
      (
        "#[webidl(enum)] struct Foo { a: u8 }",
        "#[webidl(enum)] is only valid on enums",
      ),
      (
        "#[webidl(dictionary)] struct Foo(u8);",
        "#[webidl(dictionary)] requires a struct with named fields",
      ),
      (
        "#[webidl(dictionary)] struct Foo;",
        "#[webidl(dictionary)] requires a struct with named fields",
      ),
      (
        "#[webidl(enum)] enum Foo { A(u8) }",
        "#[webidl(enum)] variants must be unit variants",
      ),
      (
        "#[webidl(dictionary)] struct Foo<T> { a: T }",
        "generic types are not supported by #[derive(WebIDL)]",
      ),
      (
        "#[webidl(dictionary)] struct Foo { #[webidl(bogus)] a: u8 }",
        "unknown #[webidl(...)] key on a dictionary member, expected `required`, `rename`, or `default`",
      ),
      (
        "#[webidl(dictionary)] struct Foo { #[webidl(rename = \"x\")] a: u8, #[webidl(rename = \"x\")] b: u8 }",
        "duplicate WebIDL member key 'x'",
      ),
      (
        "#[webidl(enum)] enum Foo { #[webidl(rename = \"x\")] A, #[webidl(rename = \"x\")] B }",
        "duplicate WebIDL enum value 'x'",
      ),
    ];
    for (src, expected) in cases {
      let err = expand(src).expect_err(src);
      assert_eq!(err.to_string(), expected, "{src}");
    }
  }
}
