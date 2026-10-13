// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.

//! `#[derive(WebIDL)]`: generates `deno_core::webidl::WebIdlConverter` impls for
//! dictionaries (`#[webidl(dictionary)]` on a struct with named fields) and
//! enumerations (`#[webidl(enum)]` on an enum with unit variants).

use proc_macro2::TokenStream;
use syn::ext::IdentExt;
use syn::parse::Parse;
use syn::parse::ParseStream;
use syn::punctuated::Punctuated;
use syn::DeriveInput;
use syn::Expr;
use syn::Ident;
use syn::Token;

mod dictionary;
mod r#enum;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
  Dictionary,
  Enum,
}

pub(crate) fn webidl(input: TokenStream) -> Result<TokenStream, syn::Error> {
  let input: DeriveInput = syn::parse2(input)?;
  let kind = parse_kind(&input)?;
  match (kind, &input.data) {
    (Kind::Dictionary, syn::Data::Struct(data)) => {
      dictionary::derive(&input, data)
    }
    (Kind::Enum, syn::Data::Enum(data)) => r#enum::derive(&input, data),
    (Kind::Dictionary, _) => Err(syn::Error::new_spanned(
      &input.ident,
      "#[webidl(dictionary)] can only be used on a struct with named fields",
    )),
    (Kind::Enum, _) => Err(syn::Error::new_spanned(
      &input.ident,
      "#[webidl(enum)] can only be used on an enum",
    )),
  }
}

/// Parses the `#[webidl(...)]` attribute on the type, which must name exactly
/// one kind.
fn parse_kind(input: &DeriveInput) -> Result<Kind, syn::Error> {
  let mut kind = None;
  for attr in input.attrs.iter().filter(|a| a.path().is_ident("webidl")) {
    for arg in parse_args(attr)? {
      let found = match (arg.name.to_string().as_str(), &arg.value) {
        ("dictionary", None) => Kind::Dictionary,
        ("enum", None) => Kind::Enum,
        _ => {
          return Err(syn::Error::new_spanned(
            &arg.name,
            "unknown #[webidl] kind, expected `dictionary` or `enum`",
          ))
        }
      };
      if kind.replace(found).is_some() {
        return Err(syn::Error::new_spanned(
          &arg.name,
          "only one #[webidl] kind may be given",
        ));
      }
    }
  }
  kind.ok_or_else(|| {
    syn::Error::new_spanned(
      &input.ident,
      "missing #[webidl(dictionary)] or #[webidl(enum)] on this type",
    )
  })
}

/// One `name` or `name = value` argument inside `#[name(...)]`. Names may be
/// keywords (e.g. `enum`), so they are parsed with `Ident::parse_any`.
pub(super) struct Arg {
  pub name: Ident,
  pub value: Option<Expr>,
}

impl Parse for Arg {
  fn parse(input: ParseStream) -> syn::Result<Self> {
    let name = input.call(Ident::parse_any)?;
    let value = if input.peek(Token![=]) {
      input.parse::<Token![=]>()?;
      Some(input.parse()?)
    } else {
      None
    };
    Ok(Arg { name, value })
  }
}

/// Parses the comma-separated arguments of a `#[name(...)]` attribute.
pub(super) fn parse_args(
  attr: &syn::Attribute,
) -> Result<Vec<Arg>, syn::Error> {
  let args: Punctuated<Arg, Token![,]> =
    attr.parse_args_with(Punctuated::parse_terminated)?;
  Ok(args.into_iter().collect())
}

/// Returns true if `ty` is syntactically `Option<...>`.
fn is_option(ty: &syn::Type) -> bool {
  match ty {
    syn::Type::Path(path) => path
      .path
      .segments
      .last()
      .is_some_and(|segment| segment.ident == "Option"),
    _ => false,
  }
}

/// Strips a raw identifier prefix, so `r#type` becomes `type`.
fn unraw(ident: &proc_macro2::Ident) -> String {
  let name = ident.to_string();
  name.strip_prefix("r#").map(str::to_owned).unwrap_or(name)
}

#[cfg(test)]
mod tests {
  use super::webidl;
  use quote::quote;
  use std::path::PathBuf;

  #[testing_macros::fixture("webidl/test_cases/*.rs")]
  fn test_webidl_expansion(input: PathBuf) {
    let update_expected = std::env::var("UPDATE_EXPECTED").is_ok();
    let source =
      std::fs::read_to_string(&input).expect("Failed to read test file");
    const PRELUDE: &str = "// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.";
    assert!(source.starts_with(PRELUDE), "Missing copyright header");
    let file =
      syn::parse_str::<syn::File>(&source).expect("Failed to parse Rust file");
    let mut expanded = Vec::new();
    for item in file.items {
      let has_derive = match &item {
        syn::Item::Struct(s) => {
          s.attrs.iter().any(|a| a.path().is_ident("derive"))
        }
        syn::Item::Enum(e) => {
          e.attrs.iter().any(|a| a.path().is_ident("derive"))
        }
        _ => false,
      };
      if !has_derive {
        continue;
      }
      let output = webidl(quote::ToTokens::to_token_stream(&item))
        .expect("Failed to expand WebIDL derive");
      let tree = syn::parse2(output).unwrap();
      expanded.push(prettyplease::unparse(&tree));
    }
    let actual = expanded.join("\n");
    if update_expected {
      std::fs::write(input.with_extension("out"), &actual)
        .expect("Failed to write expectation file");
    } else {
      let expected = std::fs::read_to_string(input.with_extension("out"))
        .expect("Failed to read expectation file");
      assert_eq!(
        expected, actual,
        "Failed to match expectation. Use UPDATE_EXPECTED=1."
      );
    }
  }

  fn err(input: proc_macro2::TokenStream) -> String {
    match webidl(input) {
      Ok(_) => panic!("expected an error"),
      Err(err) => err.to_string(),
    }
  }

  #[test]
  fn missing_kind() {
    assert_eq!(
      err(quote!(
        struct Foo {}
      )),
      "missing #[webidl(dictionary)] or #[webidl(enum)] on this type"
    );
  }

  #[test]
  fn unknown_kind() {
    assert_eq!(
      err(quote!(
        #[webidl(union)]
        struct Foo {}
      )),
      "unknown #[webidl] kind, expected `dictionary` or `enum`"
    );
  }

  #[test]
  fn dictionary_on_tuple_struct() {
    assert_eq!(
      err(quote!(
        #[webidl(dictionary)]
        struct Foo(u8);
      )),
      "#[webidl(dictionary)] can only be used on a struct with named fields"
    );
  }

  #[test]
  fn dictionary_on_unit_struct() {
    assert_eq!(
      err(quote!(
        #[webidl(dictionary)]
        struct Foo;
      )),
      "#[webidl(dictionary)] can only be used on a struct with named fields"
    );
  }

  #[test]
  fn dictionary_on_enum() {
    assert_eq!(
      err(quote!(
        #[webidl(dictionary)]
        enum Foo {
          A,
        }
      )),
      "#[webidl(dictionary)] can only be used on a struct with named fields"
    );
  }

  #[test]
  fn enum_on_struct() {
    assert_eq!(
      err(quote!(
        #[webidl(enum)]
        struct Foo {
          a: u8,
        }
      )),
      "#[webidl(enum)] can only be used on an enum"
    );
  }

  #[test]
  fn enum_with_tuple_variant() {
    assert_eq!(
      err(quote!(
        #[webidl(enum)]
        enum Foo {
          A(u8),
        }
      )),
      "#[webidl(enum)] requires unit variants"
    );
  }

  #[test]
  fn enum_with_struct_variant() {
    assert_eq!(
      err(quote!(
        #[webidl(enum)]
        enum Foo {
          A { a: u8 },
        }
      )),
      "#[webidl(enum)] requires unit variants"
    );
  }

  #[test]
  fn unknown_field_attribute() {
    assert_eq!(
      err(quote!(
        #[webidl(dictionary)]
        struct Foo {
          #[webidl(bogus)]
          a: u8,
        }
      )),
      "unknown #[webidl] field attribute `bogus`"
    );
  }

  #[test]
  fn required_with_default() {
    assert_eq!(
      err(quote!(
        #[webidl(dictionary)]
        struct Foo {
          #[webidl(required, default = 1)]
          a: u8,
        }
      )),
      "#[webidl(required)] can not be combined with #[webidl(default)]"
    );
  }

  #[test]
  fn duplicate_options_field() {
    assert_eq!(
      err(quote!(
        #[webidl(dictionary)]
        struct Foo {
          #[options(clamp = true, clamp = false)]
          a: u8,
        }
      )),
      "duplicate #[options] field `clamp`"
    );
  }

  #[test]
  fn duplicate_field_rename() {
    assert_eq!(
      err(quote!(
        #[webidl(dictionary)]
        struct Foo {
          #[webidl(rename = "a", rename = "b")]
          a: u8,
        }
      )),
      "duplicate #[webidl] field attribute `rename`"
    );
  }

  #[test]
  fn duplicate_field_default() {
    assert_eq!(
      err(quote!(
        #[webidl(dictionary)]
        struct Foo {
          #[webidl(default = 1, default = 2)]
          a: u8,
        }
      )),
      "duplicate #[webidl] field attribute `default`"
    );
  }

  #[test]
  fn duplicate_field_default_in_separate_attributes() {
    assert_eq!(
      err(quote!(
        #[webidl(dictionary)]
        struct Foo {
          #[webidl(default = 1)]
          #[webidl(default = 2)]
          a: u8,
        }
      )),
      "duplicate #[webidl] field attribute `default`"
    );
  }

  #[test]
  fn duplicate_field_required() {
    assert_eq!(
      err(quote!(
        #[webidl(dictionary)]
        struct Foo {
          #[webidl(required, required)]
          a: u8,
        }
      )),
      "duplicate #[webidl] field attribute `required`"
    );
  }

  #[test]
  fn duplicate_variant_rename() {
    assert_eq!(
      err(quote!(
        #[webidl(enum)]
        enum Foo {
          #[webidl(rename = "a", rename = "b")]
          A,
        }
      )),
      "duplicate #[webidl] variant attribute `rename`"
    );
  }

  #[test]
  fn duplicate_enum_value() {
    assert_eq!(
      err(quote!(
        #[webidl(enum)]
        enum Foo {
          A,
          #[webidl(rename = "a")]
          B,
        }
      )),
      "duplicate enumeration value 'a'"
    );
  }
}
