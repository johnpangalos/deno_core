// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.
#![doc = include_str!("README.md")]

use proc_macro::TokenStream;
use std::error::Error;

mod op2;
mod webidl;

/// A macro designed to provide an extremely fast V8->Rust interface layer.
#[doc = include_str!("op2/README.md")]
#[proc_macro_attribute]
pub fn op2(attr: TokenStream, item: TokenStream) -> TokenStream {
  op2_macro(attr, item)
}

/// Derives `deno_core::webidl::WebIdlConverter` for a dictionary or an enum.
///
/// Choose the kind with `#[webidl(dictionary)]` on a struct with named fields,
/// or with `#[webidl(enum)]` on an enum whose variants are all unit variants.
///
/// Dictionary members are read with `Get` in the code-unit order of their
/// keys, whatever the declaration order. `undefined` and `null` make every
/// member missing, and other non-objects are an error. Field attributes:
///
/// - `#[webidl(rename = "key")]` uses `key` instead of the derived key.
/// - `#[webidl(required)]` makes a missing member an error.
/// - `#[webidl(default = expr)]` uses `expr` for a missing member.
/// - `#[options(name = value, ...)]` sets fields of the member's converter
///   `Options`. The remaining fields take their defaults.
///
/// A missing member without `required` or `default` is `None` if its type is
/// `Option<T>`, and an error otherwise.
///
/// Naming: a member's key is its field name in camelCase (`foo_bar` becomes
/// `fooBar`). An enum value is its variant name in kebab-case (`FooBar`
/// becomes `foo-bar`). Variants can use `#[webidl(rename = "value")]` as well.
/// Matching is exact: case-sensitive, with no trimming.
#[proc_macro_derive(WebIDL, attributes(webidl, options))]
pub fn derive_webidl(input: TokenStream) -> TokenStream {
  let input = syn::parse_macro_input!(input as syn::DeriveInput);
  match webidl::derive(&input) {
    Ok(output) => output.into(),
    Err(err) => err.to_compile_error().into(),
  }
}

fn op2_macro(attr: TokenStream, item: TokenStream) -> TokenStream {
  match crate::op2::op2(attr.into(), item.into()) {
    Ok(output) => output.into(),
    Err(err) => {
      let mut err: &dyn Error = &err;
      let mut output = "Failed to parse #[op2]:\n".to_owned();
      loop {
        output += &format!(" - {err}\n");
        if let Some(source) = err.source() {
          err = source;
        } else {
          break;
        }
      }
      panic!("{output}");
    }
  }
}
