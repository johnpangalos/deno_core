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

/// Derives `deno_core::webidl::WebIdlConverter` for dictionaries and enums.
///
/// - `#[webidl(dictionary)]` on a struct with named fields. Each field is a
///   member, keyed by its name in camelCase, or by `#[webidl(rename = "key")]`.
///   Members are read in lexicographic order of their keys.
/// - `#[webidl(enum)]` on an enum whose variants are all unit variants. Values
///   are matched case-sensitively against the variant name in kebab-case, or
///   `#[webidl(rename = "value")]`.
/// - `#[webidl(required)]` on a field makes a missing or `undefined` member an
///   error. `#[webidl(default = <expr>)]` supplies a value for a missing or
///   `undefined` member instead. A member cannot be both `default` and
///   `required`, because the WebIDL spec forbids a default on a required member.
/// - `#[options(clamp = true, enforce_range = true, treat_null_as_empty_string = true)]`
///   on a field sets the options for that member's conversion. Unknown option
///   names are a compile error.
/// - A field of type `Option<T>` is `None` when the member is missing or `null`.
#[proc_macro_derive(WebIDL, attributes(webidl, options))]
pub fn webidl_derive(item: TokenStream) -> TokenStream {
  let input = syn::parse_macro_input!(item as syn::DeriveInput);
  match crate::webidl::derive(input) {
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
