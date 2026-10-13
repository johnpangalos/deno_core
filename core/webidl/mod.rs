// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.

//! Conversions from JavaScript values to Rust types, following the WebIDL
//! "Web IDL conversions" algorithms.
//!
//! [`WebIdlConverter`] is implemented for the common WebIDL types. Failures are
//! reported as a [`WebIdlError`] whose message has the form
//! `"{prefix}: {context} {reason}"`, for example:
//!
//! ```text
//! Failed to execute 'fetch': Argument 1 is not a finite number.
//! ```
//!
//! Nested converters extend the context so the message says where the value
//! failed:
//!
//! - sequence element: `"{context}, index {i}"`, e.g. `Argument 1, index 2`
//! - record value: `"{context}, key '{key}'"`, e.g. `Argument 1, key 'a'`
//! - dictionary member: `"'{key}' of '{Dictionary}' ({context})"`, e.g.
//!   `'maxAge' of 'CacheOptions' (Argument 1)`
//!
//! JavaScript exceptions thrown during conversion (a throwing `valueOf`,
//! `toString`, getter, iterator, or Proxy trap) never panic. They are caught
//! and reported as [`WebIdlErrorKind::Thrown`] with the reason
//! `threw an exception: {message}`. The exception is not rethrown, so the
//! isolate stays usable.
//!
//! When an op argument fails to convert, the op throws a `TypeError` (async ops
//! reject with it) whose message is the [`WebIdlError`] message.
//!
//! # Deriving converters
//!
//! `#[derive(deno_core::WebIDL)]` generates a converter for:
//!
//! - a struct with named fields marked `#[webidl(dictionary)]`. Members are
//!   read with `Get`, so prototype properties and getters count, in
//!   lexicographic order of their keys (UTF-16 code units). `undefined` and
//!   `null` make every member missing. The key is the field name in camelCase
//!   (`foo_bar` becomes `fooBar`, and a raw identifier such as `r#type` uses
//!   its name without the `r#`), or the value of `#[webidl(rename = "key")]`.
//!   A missing or `undefined` member uses `#[webidl(default = expr)]` if given,
//!   is `None` if its type is written literally as `Option<T>`, and is
//!   otherwise an error. A type alias to `Option` is not treated as optional,
//!   so it needs a default to be missing.
//!   `#[webidl(required)]` makes a missing member always an error; it cannot
//!   be combined with `default`. `#[options(field = value, ...)]` sets fields
//!   of the member's `Options`, and the rest keep their defaults.
//! - an enum with only unit variants marked `#[webidl(enum)]`. The value is
//!   converted with ToString and must match a variant exactly. The string is
//!   the variant name in kebab-case (`FooBar` becomes `"foo-bar"`), or the
//!   value of `#[webidl(rename = "value")]` on the variant.
//!
//! Misuse, such as a tuple struct marked `dictionary` or an unknown
//! `#[webidl(...)]` key, is a compile error.

use std::borrow::Cow;
use std::fmt;

use crate::v8;

mod containers;
mod primitives;
mod strings;

pub use primitives::IntOptions;
pub use primitives::UnrestrictedDouble;
pub use primitives::UnrestrictedFloat;
pub use strings::BigInt;
pub use strings::ByteString;
pub use strings::StringOptions;

/// Converts a JavaScript value into `Self` following WebIDL conversion rules.
///
/// `prefix` names the operation, e.g. `Failed to execute 'fetch'`, and
/// `context` names the value being converted, e.g. `Argument 1`. Both appear in
/// the error message.
pub trait WebIdlConverter<'a>: Sized {
  /// Options that change how the value is converted. Use `()` if there are none.
  type Options: Default;

  fn convert<C>(
    scope: &mut v8::HandleScope<'a>,
    value: v8::Local<'a, v8::Value>,
    prefix: Cow<'static, str>,
    context: C,
    options: &Self::Options,
  ) -> Result<Self, WebIdlError>
  where
    C: Fn() -> Cow<'static, str>;
}

/// An error produced when a JavaScript value cannot be converted.
///
/// Displays as `"{prefix}: {context} {reason}"`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebIdlError {
  /// Names the operation, e.g. `Failed to execute 'fetch'`.
  pub prefix: Cow<'static, str>,
  /// Names the value, e.g. `Argument 1` or `Argument 1, index 2`.
  pub context: Cow<'static, str>,
  /// Why the conversion failed.
  pub kind: WebIdlErrorKind,
}

/// The reason a [`WebIdlError`] was produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WebIdlErrorKind {
  /// The value has the wrong type, e.g. it is a Symbol or is not iterable.
  InvalidType(Cow<'static, str>),
  /// The value is not finite, or is outside the range of the target type.
  OutOfRange(Cow<'static, str>),
  /// A JavaScript exception was thrown during conversion. Holds its message.
  Thrown(String),
}

impl fmt::Display for WebIdlError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match &self.kind {
      WebIdlErrorKind::InvalidType(reason)
      | WebIdlErrorKind::OutOfRange(reason) => {
        write!(f, "{}: {} {}", self.prefix, self.context, reason)
      }
      WebIdlErrorKind::Thrown(message) => {
        write!(
          f,
          "{}: {} threw an exception: {}",
          self.prefix, self.context, message
        )
      }
    }
  }
}

impl std::error::Error for WebIdlError {}

pub(crate) fn error(
  prefix: &str,
  context: &dyn Fn() -> Cow<'static, str>,
  kind: WebIdlErrorKind,
) -> WebIdlError {
  WebIdlError {
    prefix: Cow::Owned(prefix.to_owned()),
    context: context(),
    kind,
  }
}

pub(crate) fn invalid(
  prefix: &str,
  context: &dyn Fn() -> Cow<'static, str>,
  reason: &'static str,
) -> WebIdlError {
  error(
    prefix,
    context,
    WebIdlErrorKind::InvalidType(Cow::Borrowed(reason)),
  )
}

pub(crate) fn out_of_range(
  prefix: &str,
  context: &dyn Fn() -> Cow<'static, str>,
  reason: &'static str,
) -> WebIdlError {
  error(
    prefix,
    context,
    WebIdlErrorKind::OutOfRange(Cow::Borrowed(reason)),
  )
}

fn exception_message(
  scope: &mut v8::HandleScope,
  exception: v8::Local<v8::Value>,
) -> String {
  // Symbols cannot be converted with ToString, so render them by hand.
  if let Ok(symbol) = exception.try_cast::<v8::Symbol>() {
    let description = symbol.description(scope);
    let description = if description.is_undefined() {
      String::new()
    } else {
      description
        .to_string(scope)
        .map(|s| s.to_rust_string_lossy(scope))
        .unwrap_or_default()
    };
    return format!("Symbol({description})");
  }
  match exception.to_string(scope) {
    Some(message) => message.to_rust_string_lossy(scope),
    None => "unknown exception".to_string(),
  }
}

/// Runs `op` under a `TryCatch`. If it returns `None`, a JavaScript exception
/// is pending and becomes a [`WebIdlErrorKind::Thrown`] error. The exception is
/// dropped with the `TryCatch`, so the isolate is left usable.
pub(crate) fn js<'a, T>(
  scope: &mut v8::HandleScope<'a>,
  prefix: &str,
  context: &dyn Fn() -> Cow<'static, str>,
  op: impl FnOnce(&mut v8::TryCatch<v8::HandleScope<'a>>) -> Option<T>,
) -> Result<T, WebIdlError> {
  let tc = &mut v8::TryCatch::new(scope);
  match op(&mut *tc) {
    Some(value) => Ok(value),
    None => {
      let message = match tc.exception() {
        Some(exception) => exception_message(tc, exception),
        None => "unknown exception".to_string(),
      };
      Err(error(prefix, context, WebIdlErrorKind::Thrown(message)))
    }
  }
}

/// `ToNumber`, rejecting Symbols and BigInts with a clear message.
pub(crate) fn to_number<'a>(
  scope: &mut v8::HandleScope<'a>,
  value: v8::Local<'a, v8::Value>,
  prefix: &str,
  context: &dyn Fn() -> Cow<'static, str>,
) -> Result<f64, WebIdlError> {
  if value.is_symbol() {
    return Err(invalid(
      prefix,
      context,
      "is a Symbol and cannot be converted to a number.",
    ));
  }
  if value.is_big_int() {
    return Err(invalid(
      prefix,
      context,
      "is a BigInt and cannot be converted to a number.",
    ));
  }
  js(scope, prefix, context, |tc| {
    value.to_number(tc).map(|number| number.value())
  })
}

/// `Get(object, key)`, with JavaScript exceptions reported as errors.
pub(crate) fn get_property<'a>(
  scope: &mut v8::HandleScope<'a>,
  object: v8::Local<'a, v8::Object>,
  key: &str,
  prefix: &str,
  context: &dyn Fn() -> Cow<'static, str>,
) -> Result<v8::Local<'a, v8::Value>, WebIdlError> {
  let key = v8::String::new(scope, key).unwrap();
  js(scope, prefix, context, |tc| object.get(tc, key.into()))
}

/// Helpers used by the code generated by `#[derive(WebIDL)]`. Not public API.
#[doc(hidden)]
pub mod _derive {
  use super::*;

  /// Returns `None` for `null` and `undefined`, which make every member missing.
  pub fn dictionary_object<'a>(
    value: v8::Local<'a, v8::Value>,
    prefix: &str,
    context: &dyn Fn() -> Cow<'static, str>,
  ) -> Result<Option<v8::Local<'a, v8::Object>>, WebIdlError> {
    if value.is_null_or_undefined() {
      return Ok(None);
    }
    match value.try_cast::<v8::Object>() {
      Ok(object) => Ok(Some(object)),
      Err(_) => Err(invalid(prefix, context, "is not an object.")),
    }
  }

  /// The context of a dictionary member: `'{key}' of '{dictionary}' ({context})`.
  pub fn member_context(
    key: &str,
    dictionary: &str,
    context: &dyn Fn() -> Cow<'static, str>,
  ) -> Cow<'static, str> {
    Cow::Owned(format!("'{key}' of '{dictionary}' ({})", context()))
  }

  /// Reads a member. A missing dictionary yields `undefined`.
  pub fn dictionary_member<'a>(
    scope: &mut v8::HandleScope<'a>,
    object: Option<v8::Local<'a, v8::Object>>,
    key: &str,
    prefix: &str,
    context: &dyn Fn() -> Cow<'static, str>,
  ) -> Result<v8::Local<'a, v8::Value>, WebIdlError> {
    match object {
      Some(object) => get_property(scope, object, key, prefix, context),
      None => Ok(v8::undefined(scope).into()),
    }
  }

  pub fn required_member(
    prefix: &str,
    context: &dyn Fn() -> Cow<'static, str>,
  ) -> WebIdlError {
    invalid(prefix, context, "is required.")
  }

  pub fn invalid_enum_value(
    prefix: &str,
    context: &dyn Fn() -> Cow<'static, str>,
    value: &str,
    name: &str,
  ) -> WebIdlError {
    error(
      prefix,
      context,
      WebIdlErrorKind::InvalidType(Cow::Owned(format!(
        "has the value '{value}', which is not one of the values of enum '{name}'."
      ))),
    )
  }
}

/// Runs `source` as a script in a fresh runtime and passes the resulting value
/// to `f`.
#[cfg(test)]
pub(crate) fn with_js_value<R>(
  source: &str,
  f: impl for<'a> FnOnce(&mut v8::HandleScope<'a>, v8::Local<'a, v8::Value>) -> R,
) -> R {
  let mut runtime = crate::JsRuntime::new(crate::RuntimeOptions::default());
  let scope = &mut runtime.handle_scope();
  let code = v8::String::new(scope, source).unwrap();
  let script = v8::Script::compile(scope, code, None).unwrap();
  let value = script.run(scope).unwrap();
  f(scope, value)
}

#[cfg(test)]
mod tests {
  use super::*;

  fn thrown(source: &str) -> WebIdlErrorKind {
    with_js_value(source, |scope, value| {
      BigInt::convert(
        scope,
        value,
        Cow::Borrowed("P"),
        || Cow::Borrowed("C"),
        &(),
      )
      .unwrap_err()
      .kind
    })
  }

  #[test]
  fn thrown_symbol_is_rendered_with_its_description() {
    assert_eq!(
      thrown("({valueOf(){throw Symbol('x')}})"),
      WebIdlErrorKind::Thrown("Symbol(x)".into())
    );
    assert_eq!(
      thrown("({valueOf(){throw Symbol()}})"),
      WebIdlErrorKind::Thrown("Symbol()".into())
    );
  }
}
