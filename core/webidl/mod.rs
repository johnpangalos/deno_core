// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.

//! Conversions from JavaScript values into Rust values, following the WebIDL
//! type mapping (<https://webidl.spec.whatwg.org/#js-type-mapping>).
//!
//! Every converter implements [`WebIdlConverter`]. Failures are reported as a
//! [`WebIdlError`] which renders as `"{prefix}: {context} {what was wrong}"`,
//! for example `Failed to execute 'fetch': Argument 1 is not a finite number`.

use std::borrow::Cow;
use std::error::Error;
use std::fmt;
use std::ops::Deref;

mod compound;
#[cfg(test)]
mod derive_tests;
mod primitive;
#[cfg(test)]
mod tests;

/// A conversion from a V8 value into a Rust value following WebIDL rules.
///
/// `prefix` names the operation being called (for example
/// `Failed to execute 'fetch'`) and `context` describes the value being
/// converted (for example `Argument 1`). `context` is only invoked when an
/// error is produced, so it is cheap to build it from captured state.
pub trait WebIdlConverter<'a>: Sized {
  /// Options that tune how the value is converted.
  type Options: Default;

  /// Converts `value` into `Self`.
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

/// An error produced while converting a value with [`WebIdlConverter`].
///
/// Displays as `"{prefix}: {context} {kind}"`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebIdlError {
  prefix: Cow<'static, str>,
  context: String,
  kind: WebIdlErrorKind,
}

impl WebIdlError {
  /// Creates an error for the operation named by `prefix`. `context` is the
  /// already-built context string, such as `Argument 1` or
  /// `'fooBar' of 'Options' (Argument 1)`.
  pub fn new(
    prefix: Cow<'static, str>,
    context: &str,
    kind: WebIdlErrorKind,
  ) -> Self {
    Self {
      prefix,
      context: context.to_string(),
      kind,
    }
  }

  /// The kind of failure that occurred.
  pub fn kind(&self) -> &WebIdlErrorKind {
    &self.kind
  }
}

impl fmt::Display for WebIdlError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    if self.context.is_empty() {
      write!(f, "{}: {}", self.prefix, self.kind)
    } else {
      write!(f, "{}: {} {}", self.prefix, self.context, self.kind)
    }
  }
}

impl Error for WebIdlError {}

/// The reason a conversion failed. Each variant's [`Display`](fmt::Display)
/// output is the tail of the error message, after the prefix and context.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WebIdlErrorKind {
  /// The value is not an object (used for dictionaries and records).
  NotAnObject,
  /// The value can not be converted to a sequence.
  NotASequence,
  /// A required dictionary member is missing. Holds the dictionary's name and
  /// the member's key.
  Required {
    /// The name of the dictionary that was being converted.
    dictionary: &'static str,
    /// The key of the missing member.
    key: String,
  },
  /// The value is not one of the enum's values. Holds the received string and
  /// the enum name.
  InvalidEnumValue {
    /// The string that was received.
    value: String,
    /// The name of the enum that was being converted.
    enum_name: &'static str,
  },
  /// The value is NaN or infinite where a finite number is required.
  NotFinite,
  /// The value is outside the accepted inclusive range.
  OutOfRange {
    /// The smallest accepted value.
    lower: i128,
    /// The largest accepted value.
    upper: i128,
  },
  /// The string contains a code point above U+00FF. Holds that code point.
  InvalidByteString(u32),
  /// The value is a Symbol and can not be converted. Holds the target type,
  /// such as `a number`.
  Symbol(&'static str),
  /// Running JavaScript for the conversion threw. Holds the exception text.
  Thrown(String),
  /// Any other failure, described by the message.
  Other(Cow<'static, str>),
}

impl fmt::Display for WebIdlErrorKind {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Self::NotAnObject => write!(f, "is not an object"),
      Self::NotASequence => write!(f, "can not be converted to sequence"),
      Self::Required { dictionary, key } => write!(
        f,
        "can not be converted to '{dictionary}' because '{key}' is required in '{dictionary}'"
      ),
      Self::InvalidEnumValue { value, enum_name } => {
        write!(f, "'{value}' is not a valid enum value of '{enum_name}'")
      }
      Self::NotFinite => write!(f, "is not a finite number"),
      Self::OutOfRange { lower, upper } => write!(
        f,
        "is outside the accepted range of {lower} to {upper}, inclusive"
      ),
      Self::InvalidByteString(code_point) => write!(
        f,
        "is not a valid ByteString (U+{code_point:04X} is greater than U+00FF)"
      ),
      Self::Symbol(target) => {
        write!(f, "is a Symbol and can not be converted to {target}")
      }
      Self::Thrown(message) => write!(f, "threw an exception: {message}"),
      Self::Other(message) => write!(f, "{message}"),
    }
  }
}

/// Options for the integer converters.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct IntOptions {
  /// Clamp out-of-range values to the nearest bound instead of wrapping.
  pub clamp: bool,
  /// Reject NaN, infinite and out-of-range values. Checked before `clamp`.
  pub enforce_range: bool,
}

/// Options for the string converters.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct StringOptions {
  /// Convert `null` to the empty string instead of `"null"`.
  pub treat_null_as_empty_string: bool,
}

/// WebIDL `unrestricted float`: a `f32` that may be NaN or infinite.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct UnrestrictedFloat(pub f32);

impl Deref for UnrestrictedFloat {
  type Target = f32;

  fn deref(&self) -> &f32 {
    &self.0
  }
}

/// WebIDL `unrestricted double`: a `f64` that may be NaN or infinite.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct UnrestrictedDouble(pub f64);

impl Deref for UnrestrictedDouble {
  type Target = f64;

  fn deref(&self) -> &f64 {
    &self.0
  }
}

/// WebIDL `ByteString`: a string whose code units are all at most U+00FF.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ByteString(pub String);

impl Deref for ByteString {
  type Target = String;

  fn deref(&self) -> &String {
    &self.0
  }
}

/// A JavaScript `BigInt` as a sign and little-endian 64-bit words.
///
/// Zero has `sign == false` and no words.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct BigInt {
  /// True if the value is negative.
  pub sign: bool,
  /// The magnitude, least significant word first.
  pub words: Vec<u64>,
}

/// Callback used to build a context string lazily.
type ContextFn<'c> = &'c dyn Fn() -> Cow<'static, str>;

/// Builds the context for a dictionary member, for example
/// `'fooBar' of 'Options' (Argument 1)`. Generated code uses this to describe
/// nested failures.
pub fn dictionary_member_context(
  key: &str,
  dictionary: &str,
  context: &str,
) -> String {
  format!("'{key}' of '{dictionary}' ({context})")
}

/// Builds a [`WebIdlError`] from a lazily-evaluated context.
fn fail(
  prefix: &str,
  context: ContextFn<'_>,
  kind: WebIdlErrorKind,
) -> WebIdlError {
  WebIdlError::new(Cow::Owned(prefix.to_string()), &context(), kind)
}

/// Runs a V8 operation that may execute JavaScript. A thrown exception is
/// caught and returned as its string form, so no exception stays pending.
fn js<'a, T>(
  scope: &mut v8::HandleScope<'a>,
  op: impl FnOnce(&mut v8::HandleScope<'a>) -> Option<T>,
) -> Result<T, String> {
  let tc = &mut v8::TryCatch::new(scope);
  if let Some(value) = op(tc) {
    return Ok(value);
  }
  let message = tc
    .exception()
    .and_then(|exception| exception.to_string(tc))
    .map(|string| string.to_rust_string_lossy(tc));
  Err(message.unwrap_or_else(|| "unknown exception".to_string()))
}

/// ToNumber, rejecting Symbols with a clear message.
fn to_number<'a>(
  scope: &mut v8::HandleScope<'a>,
  value: v8::Local<'a, v8::Value>,
  prefix: &str,
  context: ContextFn<'_>,
) -> Result<f64, WebIdlError> {
  if value.is_symbol() {
    return Err(fail(prefix, context, WebIdlErrorKind::Symbol("a number")));
  }
  js(scope, |s| value.to_number(s).map(|n| n.value()))
    .map_err(|e| fail(prefix, context, WebIdlErrorKind::Thrown(e)))
}

/// ToString for a DOMString, with `null` handling from [`StringOptions`].
/// Lone surrogates are replaced with U+FFFD.
fn to_dom_string<'a>(
  scope: &mut v8::HandleScope<'a>,
  value: v8::Local<'a, v8::Value>,
  prefix: &str,
  context: ContextFn<'_>,
  options: &StringOptions,
) -> Result<String, WebIdlError> {
  if value.is_symbol() {
    let kind = WebIdlErrorKind::Symbol("a string");
    return Err(fail(prefix, context, kind));
  }
  if options.treat_null_as_empty_string && value.is_null() {
    return Ok(String::new());
  }
  let string = js(scope, |s| value.to_string(s))
    .map_err(|e| fail(prefix, context, WebIdlErrorKind::Thrown(e)))?;
  Ok(string.to_rust_string_lossy(scope))
}

/// ToBigInt, which parses strings and converts booleans. Numbers, `undefined`,
/// `null` and Symbols are rejected.
fn to_big_int<'a>(
  scope: &mut v8::HandleScope<'a>,
  value: v8::Local<'a, v8::Value>,
  prefix: &str,
  context: ContextFn<'_>,
) -> Result<BigInt, WebIdlError> {
  if value.is_symbol() {
    let kind = WebIdlErrorKind::Symbol("a BigInt");
    return Err(fail(prefix, context, kind));
  }
  let big = js(scope, |s| value.to_big_int(s))
    .map_err(|e| fail(prefix, context, WebIdlErrorKind::Thrown(e)))?;
  let mut words = vec![0u64; big.word_count()];
  let (sign, words) = big.to_words_array(&mut words);
  Ok(BigInt {
    sign,
    words: words.to_vec(),
  })
}

/// Returns the object that a dictionary reads its members from. `undefined`
/// and `null` give `None`, which makes every member missing. Other
/// non-objects are an error.
///
/// Used by the code generated by `#[derive(WebIDL)]`.
#[doc(hidden)]
pub fn dictionary_object<'a, C>(
  value: v8::Local<'a, v8::Value>,
  prefix: &str,
  context: C,
) -> Result<Option<v8::Local<'a, v8::Object>>, WebIdlError>
where
  C: Fn() -> Cow<'static, str>,
{
  if value.is_null_or_undefined() {
    return Ok(None);
  }
  match v8::Local::<v8::Object>::try_from(value) {
    Ok(object) => Ok(Some(object)),
    Err(_) => Err(fail(prefix, &context, WebIdlErrorKind::NotAnObject)),
  }
}

/// Reads a dictionary member with `Get`, catching exceptions from getters.
/// Returns `None` when there is no object or the member is `undefined`.
///
/// Used by the code generated by `#[derive(WebIDL)]`.
#[doc(hidden)]
pub fn dictionary_member<'a, C>(
  scope: &mut v8::HandleScope<'a>,
  object: Option<v8::Local<'a, v8::Object>>,
  key: &str,
  dictionary: &str,
  prefix: &str,
  context: C,
) -> Result<Option<v8::Local<'a, v8::Value>>, WebIdlError>
where
  C: Fn() -> Cow<'static, str>,
{
  let Some(object) = object else {
    return Ok(None);
  };
  let member_context =
    || Cow::Owned(dictionary_member_context(key, dictionary, &context()));
  let value = js(scope, |s| {
    let name = v8::String::new(s, key)?;
    object.get(s, name.into())
  })
  .map_err(|e| fail(prefix, &member_context, WebIdlErrorKind::Thrown(e)))?;
  Ok((!value.is_undefined()).then_some(value))
}

/// Converts the value of a WebIDL enum to a string, which is then matched
/// against the enum's values.
///
/// Used by the code generated by `#[derive(WebIDL)]`.
#[doc(hidden)]
pub fn enum_string<'a, C>(
  scope: &mut v8::HandleScope<'a>,
  value: v8::Local<'a, v8::Value>,
  prefix: &str,
  context: C,
) -> Result<String, WebIdlError>
where
  C: Fn() -> Cow<'static, str>,
{
  to_dom_string(scope, value, prefix, &context, &StringOptions::default())
}
