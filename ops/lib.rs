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

/// Derives `deno_core::webidl::WebIdlConverter`, which converts a JavaScript
/// value to the annotated type following the WebIDL JavaScript-to-IDL rules.
///
/// Apply `#[webidl(dictionary)]` to a struct with named fields, or
/// `#[webidl(enum)]` to an enum whose variants are all unit variants.
///
/// # Dictionaries
///
/// The value must be an object, `undefined` or `null` (which makes every
/// member missing). Members are read in lexicographic order of their keys, each
/// with a single property read.
///
/// - Each member's key is its field name in camelCase (`foo_bar` is `fooBar`).
///   `#[webidl(rename = "key")]` sets the key.
/// - `#[webidl(default = expr)]` gives the value used when the member is
///   missing or `undefined`.
/// - An `Option<T>` member that is missing or `undefined` is `None`.
/// - `#[webidl(required)]` makes a missing or `undefined` member an error. It
///   can't be combined with `default`.
/// - Other members that are missing or `undefined` are an error.
/// - `#[options(field = value, ...)]` sets fields of the member's
///   `WebIdlConverter::Options`, for example `#[options(clamp = true)]`.
///
/// # Enums
///
/// The value is converted to a string, and the string must be the kebab-case
/// name of a variant (`FooBar` is `"foo-bar"`). `#[webidl(rename = "value")]`
/// on a variant sets its string.
///
/// # Example
///
/// ```ignore
/// use deno_core::WebIDL;
///
/// #[derive(WebIDL)]
/// #[webidl(dictionary)]
/// struct Options {
///   #[webidl(default = 1)]
///   retry_count: u8,
///   #[options(clamp = true)]
///   timeout: Option<u8>,
/// }
///
/// #[derive(WebIDL)]
/// #[webidl(enum)]
/// enum Mode {
///   ReadOnly,
///   #[webidl(rename = "rw")]
///   ReadWrite,
/// }
/// ```
#[proc_macro_derive(WebIDL, attributes(webidl, options))]
pub fn webidl(input: TokenStream) -> TokenStream {
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
