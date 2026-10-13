// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.
#![doc = include_str!("README.md")]

use proc_macro::TokenStream;
use std::error::Error;

mod op2;
mod webidl;

/// Derives `deno_core::webidl::WebIdlConverter` for a struct or an enum.
///
/// - `#[webidl(dictionary)]` on a struct with named fields makes it a WebIDL
///   dictionary. Members are read in the order of their JavaScript keys, each
///   one once, and converted as soon as it is read. `undefined` and `null` make
///   every member missing.
/// - `#[webidl(enum)]` on an enum with only unit variants makes it a WebIDL
///   enumeration. The value is converted to a string and must match a variant's
///   kebab-case name exactly (`FooBar` is `"foo-bar"`).
/// - `#[webidl(rename = "value")]` on a dictionary field sets its JavaScript
///   key, and on an enum variant sets its value. By default a field's key is its
///   name in camelCase (`foo_bar` is `"fooBar"`, and `r#type` is `"type"`).
/// - `#[webidl(default = expr)]` on a dictionary field gives the value used when
///   its member is missing or `undefined`. `expr` is only evaluated in that case.
/// - `#[webidl(required)]` on a dictionary field makes a missing member an
///   error, even when the field is an `Option`. It can not be used with `default`.
/// - `#[options(name = value, ...)]` on a dictionary field sets those fields of
///   the field type's `WebIdlConverter::Options`. The rest take their default.
///
/// A dictionary field whose type is `Option<T>` is `None` when its member is
/// missing, unless it is `required`. Generic and lifetime parameters are
/// rejected, and misuse is reported as a compile error on the item.
#[proc_macro_derive(WebIDL, attributes(webidl, options))]
pub fn webidl(item: TokenStream) -> TokenStream {
  let input = syn::parse_macro_input!(item as syn::DeriveInput);
  match crate::webidl::derive(input) {
    Ok(output) => output.into(),
    Err(err) => err.to_compile_error().into(),
  }
}

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
