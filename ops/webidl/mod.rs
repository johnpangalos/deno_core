// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.

//! Expansion of `#[derive(WebIDL)]`, which implements
//! `deno_core::webidl::WebIdlConverter` for dictionaries and enums.

use proc_macro2::TokenStream;
use syn::parse_quote;
use syn::Data;
use syn::DeriveInput;
use syn::GenericParam;
use syn::Generics;
use syn::Lifetime;
use syn::LifetimeParam;

mod dictionary;
mod enumeration;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
  Dictionary,
  Enum,
}

pub(crate) fn expand(input: TokenStream) -> Result<TokenStream, syn::Error> {
  let input: DeriveInput = syn::parse2(input)?;

  let mut kind = None;
  for attr in input.attrs.iter().filter(|a| a.path().is_ident("webidl")) {
    attr.parse_nested_meta(|meta| {
      let parsed = if meta.path.is_ident("dictionary") {
        Kind::Dictionary
      } else if meta.path.is_ident("enum") {
        Kind::Enum
      } else {
        return Err(
          meta
            .error("unknown webidl attribute, expected `dictionary` or `enum`"),
        );
      };
      if kind.replace(parsed).is_some() {
        return Err(
          meta.error("expected exactly one of `dictionary` or `enum`"),
        );
      }
      Ok(())
    })?;
  }

  let Some(kind) = kind else {
    return Err(syn::Error::new(
      input.ident.span(),
      "#[derive(WebIDL)] requires #[webidl(dictionary)] or #[webidl(enum)]",
    ));
  };

  match (kind, &input.data) {
    (Kind::Dictionary, Data::Struct(data)) => dictionary::expand(&input, data),
    (Kind::Enum, Data::Enum(data)) => enumeration::expand(&input, data),
    (Kind::Dictionary, _) => Err(syn::Error::new(
      input.ident.span(),
      "#[webidl(dictionary)] can only be used on a struct",
    )),
    (Kind::Enum, _) => Err(syn::Error::new(
      input.ident.span(),
      "#[webidl(enum)] can only be used on an enum",
    )),
  }
}

/// Returns the lifetime the generated impl converts with, and the generics
/// for the impl. A struct's first lifetime is reused; otherwise `'a` is added.
fn impl_lifetime(input: &DeriveInput) -> (Lifetime, Generics) {
  let mut generics = input.generics.clone();
  let lifetime = match input.generics.lifetimes().next() {
    Some(param) => param.lifetime.clone(),
    None => {
      let lifetime: Lifetime = parse_quote!('a);
      generics.params.insert(
        0,
        GenericParam::Lifetime(LifetimeParam::new(lifetime.clone())),
      );
      lifetime
    }
  };
  (lifetime, generics)
}

#[cfg(test)]
mod tests {
  use super::expand;
  use pretty_assertions::assert_eq;
  use quote::quote;
  use std::path::PathBuf;
  use syn::Item;

  fn error_message(input: proc_macro2::TokenStream) -> String {
    match expand(input) {
      Ok(_) => panic!("expected the derive to fail"),
      Err(err) => err.to_string(),
    }
  }

  #[test]
  fn derive_requires_kind() {
    assert_eq!(
      error_message(quote! { struct Foo { a: u8 } }),
      "#[derive(WebIDL)] requires #[webidl(dictionary)] or #[webidl(enum)]"
    );
  }

  #[test]
  fn unknown_container_attribute() {
    assert_eq!(
      error_message(quote! { #[webidl(foo)] struct Foo { a: u8 } }),
      "unknown webidl attribute, expected `dictionary` or `enum`"
    );
  }

  #[test]
  fn dictionary_on_enum_or_enum_on_struct() {
    assert_eq!(
      error_message(quote! { #[webidl(dictionary)] enum Foo { A } }),
      "#[webidl(dictionary)] can only be used on a struct"
    );
    assert_eq!(
      error_message(quote! { #[webidl(enum)] struct Foo { a: u8 } }),
      "#[webidl(enum)] can only be used on an enum"
    );
  }

  #[test]
  fn dictionary_requires_named_fields() {
    assert_eq!(
      error_message(quote! { #[webidl(dictionary)] struct Foo(u8); }),
      "#[webidl(dictionary)] requires a struct with named fields"
    );
    assert_eq!(
      error_message(quote! { #[webidl(dictionary)] struct Foo; }),
      "#[webidl(dictionary)] requires a struct with named fields"
    );
  }

  #[test]
  fn dictionary_unknown_and_conflicting_member_attributes() {
    assert_eq!(
      error_message(quote! {
        #[webidl(dictionary)]
        struct Foo { #[webidl(bogus)] a: u8 }
      }),
      "unknown webidl member attribute, expected `rename`, `default` or `required`"
    );
    assert_eq!(
      error_message(quote! {
        #[webidl(dictionary)]
        struct Foo { #[webidl(required, default = 1)] a: u8 }
      }),
      "`required` and `default` cannot be used together"
    );
    assert_eq!(
      error_message(quote! {
        #[webidl(dictionary)]
        struct Foo { #[webidl(rename = "x")] a: u8, #[webidl(rename = "x")] b: u8 }
      }),
      "duplicate WebIDL member key 'x'"
    );
  }

  #[test]
  fn enum_rejects_non_unit_variants() {
    assert_eq!(
      error_message(quote! {
        #[webidl(enum)]
        enum Foo { A(u8) }
      }),
      "#[webidl(enum)] only supports unit variants"
    );
  }

  #[test]
  fn enum_unknown_and_duplicate_variant_names() {
    assert_eq!(
      error_message(quote! {
        #[webidl(enum)]
        enum Foo { #[webidl(bogus)] A }
      }),
      "unknown webidl variant attribute, expected `rename`"
    );
    assert_eq!(
      error_message(quote! {
        #[webidl(enum)]
        enum Foo { FooBar, #[webidl(rename = "foo-bar")] Other }
      }),
      "duplicate WebIDL enum value 'foo-bar'"
    );
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
      panic!("Source does not start with expected prelude:]n{PRELUDE}");
    }

    let file =
      syn::parse_str::<syn::File>(&source).expect("Failed to parse Rust file");
    let mut expected_out = vec![];
    for item in file.items {
      let tokens = match &item {
        Item::Struct(s) if has_webidl_derive(&s.attrs) => {
          quote::ToTokens::to_token_stream(&s)
        }
        Item::Enum(e) if has_webidl_derive(&e.attrs) => {
          quote::ToTokens::to_token_stream(&e)
        }
        _ => continue,
      };
      let expanded = expand(tokens).expect("Failed to expand derive");
      let tree = syn::parse2(expanded).unwrap();
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

  fn has_webidl_derive(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|a| {
      a.path().is_ident("derive")
        && quote::ToTokens::to_token_stream(a)
          .to_string()
          .contains("WebIDL")
    })
  }
}
