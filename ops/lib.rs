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
/// A dictionary is a struct with named fields, marked `#[webidl(dictionary)]`.
/// Each field is read from the member whose key is the field name in
/// camelCase, or the `#[webidl(rename = "key")]` value. Members are read in
/// lexicographic order of their keys. A missing or `undefined` member takes
/// `#[webidl(default = expr)]`, is `None` for an `Option` field, or is an error
/// when it is `#[webidl(required)]`. `#[options(name = value, ...)]` sets
/// fields of the member's converter options.
///
/// An enum of unit variants, marked `#[webidl(enum)]`, converts from a string
/// that matches a variant's kebab-case name, or its `#[webidl(rename = "...")]`
/// value.
#[proc_macro_derive(WebIDL, attributes(webidl, options))]
pub fn derive_webidl(input: TokenStream) -> TokenStream {
  match crate::webidl::derive(input.into()) {
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
