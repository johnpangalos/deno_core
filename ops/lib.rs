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

/// Implements `deno_core::webidl::WebIdlConverter` for a WebIDL dictionary
/// (`#[webidl(dictionary)]` on a struct with named fields) or enum
/// (`#[webidl(enum)]` on an enum of unit variants).
///
/// Dictionary members are converted in lexicographical order of their keys.
/// Each field is read from the member named after the field in camelCase,
/// unless renamed. Field attributes:
///
/// - `#[webidl(rename = "key")]`: use `key` as the member name.
/// - `#[webidl(default = <expr>)]`: the value used when the member is missing
///   or undefined.
/// - `#[webidl(required)]`: the member must be present and not undefined.
/// - `#[options(<option> = <value>, ...)]`: set the options passed to the
///   member's converter, ie: `#[options(clamp = true)]`.
///
/// A missing or undefined member without a default value is `None` for
/// `Option<T>` fields, and an error for any other field.
///
/// Enum variants match the variant name in kebab-case (`FooBar` is
/// `"foo-bar"`), unless renamed with `#[webidl(rename = "value")]`.
///
/// ```rust,ignore
/// #[derive(WebIDL)]
/// #[webidl(dictionary)]
/// pub struct RequestInit {
///   #[webidl(default = String::from("GET"))]
///   method: String,
///   #[options(enforce_range = true)]
///   priority: Option<u8>,
///   #[webidl(required, rename = "mode")]
///   request_mode: RequestMode,
/// }
///
/// #[derive(WebIDL)]
/// #[webidl(enum)]
/// pub enum RequestMode {
///   Navigate,
///   SameOrigin,
///   #[webidl(rename = "no-cors")]
///   NoCors,
///   Cors,
/// }
/// ```
#[proc_macro_derive(WebIDL, attributes(webidl, options))]
pub fn webidl(item: TokenStream) -> TokenStream {
  match crate::webidl::webidl(item.into()) {
    Ok(output) => output.into(),
    Err(err) => err.into_compile_error().into(),
  }
}
