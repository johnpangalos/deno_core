// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.

//! Conversions from JavaScript values to Rust values following the WebIDL
//! JS-to-IDL type mapping: <https://webidl.spec.whatwg.org/#js-type-mapping>.
//!
//! The central item is the [`WebIdlConverter`] trait. Every converter takes a
//! `prefix` and a `context`. The prefix names the operation being performed
//! (for example `Failed to execute 'fetch'`) and the context names the
//! argument or member being converted (for example `Argument 1`). Together
//! they form the message of a [`WebIdlError`]:
//!
//! ```text
//! Failed to execute 'fetch': Argument 1 is not a finite number.
//! ```
//!
//! The context is a closure so that the (possibly allocating) description is
//! only built when an error actually occurs.
//!
//! Strings are Rust `String`s, which cannot hold lone surrogates. Unlike the
//! spec, lone surrogates in a DOMString are replaced with U+FFFD instead of
//! being kept as-is.
//!
//! # Derive
//!
//! `#[derive(WebIDL)]` (re-exported as [`deno_core::WebIDL`][crate::WebIDL])
//! implements [`WebIdlConverter`] for dictionaries (`#[webidl(dictionary)]`
//! on a struct with named fields) and enums (`#[webidl(enum)]` on an enum
//! with only unit variants). See the `deno_ops` crate docs for the
//! attributes.
//!
//! ```ignore
//! #[derive(deno_core::WebIDL)]
//! #[webidl(dictionary)]
//! struct RequestInit {
//!   #[webidl(default = String::from("GET"))]
//!   method: String,
//!   #[options(enforce_range = true)]
//!   redirect_limit: u8,
//!   body: Option<String>,
//! }
//! ```

use std::borrow::Cow;
use std::collections::HashMap;
use std::fmt;
use std::ops::Deref;

use crate::v8;

/// A conversion from a JavaScript value to a Rust value, following the
/// WebIDL JS-to-IDL algorithms.
///
/// `prefix` describes the operation (e.g. `Failed to execute 'fetch'`) and
/// `context` describes the value being converted (e.g. `Argument 1`). Both
/// are used only to build a [`WebIdlError`] when the conversion fails.
///
/// Converters whose WebIDL type has no options use `type Options = ()`.
pub trait WebIdlConverter<'a>: Sized {
  /// The options that control the conversion (e.g. `[Clamp]`).
  type Options: Default;

  /// Converts `value` to `Self`.
  ///
  /// Exceptions thrown by JavaScript during the conversion (for example by
  /// `valueOf` or a getter) are caught and returned as a [`WebIdlError`].
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
/// The message is `"{prefix}: {context} {problem}"`, for example
/// `Failed to execute 'fetch': Argument 1 is not a finite number.`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebIdlError {
  prefix: Cow<'static, str>,
  context: Cow<'static, str>,
  problem: Cow<'static, str>,
}

impl WebIdlError {
  /// Creates a new error from its prefix, context and problem description.
  pub fn new(
    prefix: impl Into<Cow<'static, str>>,
    context: impl Into<Cow<'static, str>>,
    problem: impl Into<Cow<'static, str>>,
  ) -> Self {
    Self {
      prefix: prefix.into(),
      context: context.into(),
      problem: problem.into(),
    }
  }
}

impl fmt::Display for WebIdlError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "{}: {} {}", self.prefix, self.context, self.problem)
  }
}

impl std::error::Error for WebIdlError {}

/// Options for the integer converters (`[Clamp]` and `[EnforceRange]`).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct IntOptions {
  /// Clamp out-of-range values to the range of the type, rounding to the
  /// nearest integer (ties to even). Ignored if `enforce_range` is set.
  pub clamp: bool,
  /// Throw on NaN, infinities and out-of-range values instead of wrapping.
  pub enforce_range: bool,
}

/// Options for the string converters (`[LegacyNullToEmptyString]`).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct StringOptions {
  /// Convert `null` to the empty string instead of `"null"`.
  pub treat_null_as_empty_string: bool,
}

/// An `unrestricted float`: NaN and infinities are allowed.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct UnrestrictedFloat(pub f32);

impl Deref for UnrestrictedFloat {
  type Target = f32;

  fn deref(&self) -> &f32 {
    &self.0
  }
}

/// An `unrestricted double`: NaN and infinities are allowed.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct UnrestrictedDouble(pub f64);

impl Deref for UnrestrictedDouble {
  type Target = f64;

  fn deref(&self) -> &f64 {
    &self.0
  }
}

/// A `ByteString`: a string whose code units are all at most U+00FF.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ByteString(pub String);

impl Deref for ByteString {
  type Target = String;

  fn deref(&self) -> &String {
    &self.0
  }
}

/// An arbitrary precision integer, as sign and magnitude.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct BigInt {
  /// `true` if the value is negative.
  pub sign: bool,
  /// The magnitude as little-endian 64-bit words.
  pub words: Vec<u64>,
}

const DEFAULT_EXCEPTION_MESSAGE: &str = "an exception was thrown";

fn error(
  prefix: &str,
  context: &dyn Fn() -> Cow<'static, str>,
  problem: impl Into<Cow<'static, str>>,
) -> WebIdlError {
  WebIdlError::new(prefix.to_owned(), context(), problem)
}

/// Runs a V8 operation that may call into JavaScript. If it fails, the
/// pending exception is captured so nothing stays pending on the isolate, and
/// it is returned as a [`WebIdlError`].
fn try_js<'a, T>(
  scope: &mut v8::HandleScope<'a>,
  prefix: &str,
  context: &dyn Fn() -> Cow<'static, str>,
  operation: impl FnOnce(&mut v8::HandleScope<'a>) -> Option<T>,
) -> Result<T, WebIdlError> {
  let exception = {
    let tc = &mut v8::TryCatch::new(scope);
    match operation(tc) {
      Some(value) => return Ok(value),
      None => tc.exception(),
    }
  };
  let problem = exception_message(scope, exception);
  Err(error(prefix, context, problem))
}

/// Returns the message of a thrown value: the `message` property if it is a
/// string (as for `Error` objects), otherwise its string form.
fn exception_message<'a>(
  scope: &mut v8::HandleScope<'a>,
  exception: Option<v8::Local<'a, v8::Value>>,
) -> Cow<'static, str> {
  let Some(exception) = exception else {
    return DEFAULT_EXCEPTION_MESSAGE.into();
  };
  let tc = &mut v8::TryCatch::new(scope);
  let message = v8::Local::<v8::Object>::try_from(exception)
    .ok()
    .and_then(|object| {
      let key = v8::String::new(tc, "message")?;
      object.get(tc, key.into())
    })
    .filter(|message| message.is_string());
  let text = match message {
    Some(message) => v8::Local::<v8::String>::try_from(message).ok(),
    None => exception.to_string(tc),
  };
  match text {
    Some(text) => text.to_rust_string_lossy(tc).into(),
    None => DEFAULT_EXCEPTION_MESSAGE.into(),
  }
}

fn to_object<'a>(
  value: v8::Local<'a, v8::Value>,
  prefix: &str,
  context: &dyn Fn() -> Cow<'static, str>,
  problem: &'static str,
) -> Result<v8::Local<'a, v8::Object>, WebIdlError> {
  v8::Local::<v8::Object>::try_from(value)
    .map_err(|_| error(prefix, context, problem))
}

fn get_property<'a>(
  scope: &mut v8::HandleScope<'a>,
  object: v8::Local<'a, v8::Object>,
  name: &str,
  prefix: &str,
  context: &dyn Fn() -> Cow<'static, str>,
) -> Result<v8::Local<'a, v8::Value>, WebIdlError> {
  try_js(scope, prefix, context, |scope| {
    let key = v8::String::new(scope, name)?;
    object.get(scope, key.into())
  })
}

fn to_number<'a>(
  scope: &mut v8::HandleScope<'a>,
  value: v8::Local<'a, v8::Value>,
  prefix: &str,
  context: &dyn Fn() -> Cow<'static, str>,
) -> Result<f64, WebIdlError> {
  try_js(scope, prefix, context, |scope| {
    value.to_number(scope).map(|number| number.value())
  })
}

fn int_bounds(bits: u32, signed: bool) -> (i128, i128) {
  if bits == 64 {
    // The bounds for 64-bit integers are limited to the exact integers of
    // a double, as in the spec.
    let max = (1i128 << 53) - 1;
    if signed {
      (-max, max)
    } else {
      (0, max)
    }
  } else if signed {
    (-(1i128 << (bits - 1)), (1i128 << (bits - 1)) - 1)
  } else {
    (0, (1i128 << bits) - 1)
  }
}

/// The spec's ConvertToInt algorithm. Returns the integer value, which is
/// always within the range of the target type.
fn convert_int<'a>(
  scope: &mut v8::HandleScope<'a>,
  value: v8::Local<'a, v8::Value>,
  prefix: &str,
  context: &dyn Fn() -> Cow<'static, str>,
  options: &IntOptions,
  bits: u32,
  signed: bool,
) -> Result<i128, WebIdlError> {
  let x = to_number(scope, value, prefix, context)?;
  let (lower, upper) = int_bounds(bits, signed);
  if options.enforce_range {
    if !x.is_finite() {
      return Err(error(prefix, context, "is not a finite number."));
    }
    let x = x.trunc();
    if x < lower as f64 || x > upper as f64 {
      return Err(error(
        prefix,
        context,
        format!(
          "is outside the accepted range of {lower} to {upper}, inclusive."
        ),
      ));
    }
    return Ok(x as i128);
  }
  if options.clamp {
    if x.is_nan() {
      return Ok(0);
    }
    let clamped = x.clamp(lower as f64, upper as f64);
    return Ok(clamped.round_ties_even() as i128);
  }
  if !x.is_finite() || x == 0.0 {
    return Ok(0);
  }
  // `fmod` is exact, so the remainder is an exact integer in (-2^64, 2^64).
  let modulus = 2f64.powi(bits as i32);
  let remainder = (x.trunc() % modulus) as i128;
  let mut result = remainder.rem_euclid(1i128 << bits);
  if signed && result >= 1i128 << (bits - 1) {
    result -= 1i128 << bits;
  }
  Ok(result)
}

macro_rules! impl_int_converter {
  ($($ty:ty => ($bits:expr, $signed:expr);)*) => {
    $(
      impl<'a> WebIdlConverter<'a> for $ty {
        type Options = IntOptions;

        fn convert<C>(
          scope: &mut v8::HandleScope<'a>,
          value: v8::Local<'a, v8::Value>,
          prefix: Cow<'static, str>,
          context: C,
          options: &Self::Options,
        ) -> Result<Self, WebIdlError>
        where
          C: Fn() -> Cow<'static, str>,
        {
          let value = convert_int(
            scope, value, &prefix, &context, options, $bits, $signed,
          )?;
          Ok(value as $ty)
        }
      }
    )*
  };
}

impl_int_converter! {
  i8 => (8, true);
  u8 => (8, false);
  i16 => (16, true);
  u16 => (16, false);
  i32 => (32, true);
  u32 => (32, false);
  i64 => (64, true);
  u64 => (64, false);
}

/// `boolean`: JavaScript's ToBoolean. Never fails.
impl<'a> WebIdlConverter<'a> for bool {
  type Options = ();

  fn convert<C>(
    scope: &mut v8::HandleScope<'a>,
    value: v8::Local<'a, v8::Value>,
    _prefix: Cow<'static, str>,
    _context: C,
    _options: &Self::Options,
  ) -> Result<Self, WebIdlError>
  where
    C: Fn() -> Cow<'static, str>,
  {
    Ok(value.to_boolean(scope).is_true())
  }
}

/// `any`: the value is returned unchanged.
impl<'a> WebIdlConverter<'a> for v8::Local<'a, v8::Value> {
  type Options = ();

  fn convert<C>(
    _scope: &mut v8::HandleScope<'a>,
    value: v8::Local<'a, v8::Value>,
    _prefix: Cow<'static, str>,
    _context: C,
    _options: &Self::Options,
  ) -> Result<Self, WebIdlError>
  where
    C: Fn() -> Cow<'static, str>,
  {
    Ok(value)
  }
}

/// `double`: ToNumber, which must be finite.
impl<'a> WebIdlConverter<'a> for f64 {
  type Options = ();

  fn convert<C>(
    scope: &mut v8::HandleScope<'a>,
    value: v8::Local<'a, v8::Value>,
    prefix: Cow<'static, str>,
    context: C,
    _options: &Self::Options,
  ) -> Result<Self, WebIdlError>
  where
    C: Fn() -> Cow<'static, str>,
  {
    let x = to_number(scope, value, &prefix, &context)?;
    if !x.is_finite() {
      return Err(error(&prefix, &context, "is not a finite number."));
    }
    Ok(x)
  }
}

/// `float`: ToNumber, which must be finite as a 32-bit float.
impl<'a> WebIdlConverter<'a> for f32 {
  type Options = ();

  fn convert<C>(
    scope: &mut v8::HandleScope<'a>,
    value: v8::Local<'a, v8::Value>,
    prefix: Cow<'static, str>,
    context: C,
    _options: &Self::Options,
  ) -> Result<Self, WebIdlError>
  where
    C: Fn() -> Cow<'static, str>,
  {
    let x = to_number(scope, value, &prefix, &context)? as f32;
    if !x.is_finite() {
      return Err(error(&prefix, &context, "is not a finite number."));
    }
    Ok(x)
  }
}

/// `unrestricted double`: ToNumber, NaN and infinities are allowed.
impl<'a> WebIdlConverter<'a> for UnrestrictedDouble {
  type Options = ();

  fn convert<C>(
    scope: &mut v8::HandleScope<'a>,
    value: v8::Local<'a, v8::Value>,
    prefix: Cow<'static, str>,
    context: C,
    _options: &Self::Options,
  ) -> Result<Self, WebIdlError>
  where
    C: Fn() -> Cow<'static, str>,
  {
    Ok(UnrestrictedDouble(to_number(
      scope, value, &prefix, &context,
    )?))
  }
}

/// `unrestricted float`: ToNumber rounded to 32 bits, NaN and infinities are
/// allowed.
impl<'a> WebIdlConverter<'a> for UnrestrictedFloat {
  type Options = ();

  fn convert<C>(
    scope: &mut v8::HandleScope<'a>,
    value: v8::Local<'a, v8::Value>,
    prefix: Cow<'static, str>,
    context: C,
    _options: &Self::Options,
  ) -> Result<Self, WebIdlError>
  where
    C: Fn() -> Cow<'static, str>,
  {
    let x = to_number(scope, value, &prefix, &context)?;
    Ok(UnrestrictedFloat(x as f32))
  }
}

/// `DOMString`: ToString. Lone surrogates become U+FFFD.
impl<'a> WebIdlConverter<'a> for String {
  type Options = StringOptions;

  fn convert<C>(
    scope: &mut v8::HandleScope<'a>,
    value: v8::Local<'a, v8::Value>,
    prefix: Cow<'static, str>,
    context: C,
    options: &Self::Options,
  ) -> Result<Self, WebIdlError>
  where
    C: Fn() -> Cow<'static, str>,
  {
    if options.treat_null_as_empty_string && value.is_null() {
      return Ok(String::new());
    }
    let string =
      try_js(scope, &prefix, &context, |scope| value.to_string(scope))?;
    Ok(string.to_rust_string_lossy(scope))
  }
}

/// `ByteString`: ToString, where every code unit must be at most U+00FF.
impl<'a> WebIdlConverter<'a> for ByteString {
  type Options = StringOptions;

  fn convert<C>(
    scope: &mut v8::HandleScope<'a>,
    value: v8::Local<'a, v8::Value>,
    prefix: Cow<'static, str>,
    context: C,
    options: &Self::Options,
  ) -> Result<Self, WebIdlError>
  where
    C: Fn() -> Cow<'static, str>,
  {
    let string =
      String::convert(scope, value, prefix.clone(), &context, options)?;
    // Lone surrogates were already replaced with U+FFFD, which is above U+00FF.
    if string.chars().any(|c| c > '\u{FF}') {
      return Err(error(
        &prefix,
        &context,
        "contains a character that is not a valid ByteString code unit.",
      ));
    }
    Ok(ByteString(string))
  }
}

/// `bigint`: ToBigInt.
impl<'a> WebIdlConverter<'a> for BigInt {
  type Options = ();

  fn convert<C>(
    scope: &mut v8::HandleScope<'a>,
    value: v8::Local<'a, v8::Value>,
    prefix: Cow<'static, str>,
    context: C,
    _options: &Self::Options,
  ) -> Result<Self, WebIdlError>
  where
    C: Fn() -> Cow<'static, str>,
  {
    let big_int =
      try_js(scope, &prefix, &context, |scope| value.to_big_int(scope))?;
    let mut words = vec![0u64; big_int.word_count()];
    let (sign, words) = big_int.to_words_array(&mut words);
    Ok(BigInt {
      sign,
      words: words.to_vec(),
    })
  }
}

/// `T?`: `undefined` and `null` become `None`, anything else is converted
/// with `T` and its options.
impl<'a, T: WebIdlConverter<'a>> WebIdlConverter<'a> for Option<T> {
  type Options = T::Options;

  fn convert<C>(
    scope: &mut v8::HandleScope<'a>,
    value: v8::Local<'a, v8::Value>,
    prefix: Cow<'static, str>,
    context: C,
    options: &Self::Options,
  ) -> Result<Self, WebIdlError>
  where
    C: Fn() -> Cow<'static, str>,
  {
    if value.is_undefined() || value.is_null() {
      return Ok(None);
    }
    T::convert(scope, value, prefix, context, options).map(Some)
  }
}

/// `sequence<T>`: any iterable object, converted item by item.
impl<'a, T: WebIdlConverter<'a>> WebIdlConverter<'a> for Vec<T> {
  type Options = T::Options;

  fn convert<C>(
    scope: &mut v8::HandleScope<'a>,
    value: v8::Local<'a, v8::Value>,
    prefix: Cow<'static, str>,
    context: C,
    options: &Self::Options,
  ) -> Result<Self, WebIdlError>
  where
    C: Fn() -> Cow<'static, str>,
  {
    let object = to_object(value, &prefix, &context, "is not an object.")?;
    let iterator_key: v8::Local<v8::Value> =
      v8::Symbol::get_iterator(scope).into();
    let method = try_js(scope, &prefix, &context, |scope| {
      object.get(scope, iterator_key)
    })?;
    let method = v8::Local::<v8::Function>::try_from(method)
      .map_err(|_| error(&prefix, &context, "is not iterable."))?;
    let iterator = try_js(scope, &prefix, &context, |scope| {
      method.call(scope, value, &[])
    })?;
    let iterator = to_object(
      iterator,
      &prefix,
      &context,
      "is not iterable: the iterator is not an object.",
    )?;
    let next = get_property(scope, iterator, "next", &prefix, &context)?;
    let next = v8::Local::<v8::Function>::try_from(next).map_err(|_| {
      error(
        &prefix,
        &context,
        "is not iterable: 'next' is not callable.",
      )
    })?;

    let mut items = Vec::new();
    loop {
      let result = try_js(scope, &prefix, &context, |scope| {
        next.call(scope, iterator.into(), &[])
      })?;
      let result = to_object(
        result,
        &prefix,
        &context,
        "is not iterable: an iterator result is not an object.",
      )?;
      let done = get_property(scope, result, "done", &prefix, &context)?;
      if done.to_boolean(scope).is_true() {
        break;
      }
      let item = get_property(scope, result, "value", &prefix, &context)?;
      let index = items.len();
      let item_context =
        || Cow::Owned(format!("{}, index {}", context(), index));
      items.push(T::convert(
        scope,
        item,
        prefix.clone(),
        item_context,
        options,
      )?);
    }
    Ok(items)
  }
}

/// `record<DOMString, T>`: the enumerable own properties of an object, with
/// string and symbol keys read from [[OwnPropertyKeys]]. Each key's
/// enumerability is checked when it is reached, so a property deleted by an
/// earlier getter is skipped. A symbol key that is enumerable is an error.
/// Keys are converted like `DOMString`, so lone surrogates become U+FFFD and
/// two keys that differ only in lone surrogates map to the same entry, the
/// later one winning.
impl<'a, T: WebIdlConverter<'a>> WebIdlConverter<'a> for HashMap<String, T> {
  type Options = T::Options;

  fn convert<C>(
    scope: &mut v8::HandleScope<'a>,
    value: v8::Local<'a, v8::Value>,
    prefix: Cow<'static, str>,
    context: C,
    options: &Self::Options,
  ) -> Result<Self, WebIdlError>
  where
    C: Fn() -> Cow<'static, str>,
  {
    let object = to_object(value, &prefix, &context, "is not an object.")?;
    let keys = try_js(scope, &prefix, &context, |scope| {
      object.get_own_property_names(
        scope,
        v8::GetPropertyNamesArgs {
          mode: v8::KeyCollectionMode::OwnOnly,
          property_filter: v8::PropertyFilter::ALL_PROPERTIES,
          index_filter: v8::IndexFilter::IncludeIndices,
          key_conversion: v8::KeyConversionMode::ConvertToString,
        },
      )
    })?;

    let mut map = HashMap::new();
    for index in 0..keys.length() {
      let key = try_js(scope, &prefix, &context, |scope| {
        keys.get_index(scope, index)
      })?;
      let enumerable = try_js(scope, &prefix, &context, |scope| {
        let name = v8::Local::<v8::Name>::try_from(key).ok()?;
        let descriptor = object.get_own_property_descriptor(scope, name)?;
        if descriptor.is_undefined() {
          return Some(false);
        }
        let descriptor = v8::Local::<v8::Object>::try_from(descriptor).ok()?;
        let key = v8::String::new(scope, "enumerable")?;
        let enumerable = descriptor.get(scope, key.into())?;
        Some(enumerable.to_boolean(scope).is_true())
      })?;
      if !enumerable {
        continue;
      }
      if key.is_symbol() {
        return Err(error(
          &prefix,
          &context,
          "can not be converted to a record because it has a symbol key.",
        ));
      }
      let name = String::convert(
        scope,
        key,
        prefix.clone(),
        &context,
        &StringOptions::default(),
      )?;
      let member =
        try_js(scope, &prefix, &context, |scope| object.get(scope, key))?;
      let member_context =
        || Cow::Owned(format!("{}, property '{}'", context(), name));
      let converted =
        T::convert(scope, member, prefix.clone(), member_context, options)?;
      map.insert(name, converted);
    }
    Ok(map)
  }
}

/// Helpers used by the code generated by `#[derive(WebIDL)]`. Not intended
/// for direct use.
#[doc(hidden)]
pub mod derive_support {
  use super::*;

  /// Returns the object for a dictionary value. `undefined` and `null` give
  /// `None`, which is an empty dictionary.
  pub fn dictionary_object<'a>(
    value: v8::Local<'a, v8::Value>,
    dictionary: &str,
    prefix: &str,
    context: &dyn Fn() -> Cow<'static, str>,
  ) -> Result<Option<v8::Local<'a, v8::Object>>, WebIdlError> {
    if value.is_undefined() || value.is_null() {
      return Ok(None);
    }
    let problem = format!(
      "can not be converted to '{dictionary}' because it is not an object."
    );
    let object = v8::Local::<v8::Object>::try_from(value)
      .map_err(|_| error(prefix, context, problem))?;
    Ok(Some(object))
  }

  /// Reads a dictionary member. Gives `undefined` for an empty dictionary.
  pub fn dictionary_member<'a>(
    scope: &mut v8::HandleScope<'a>,
    object: Option<v8::Local<'a, v8::Object>>,
    key: &'static str,
    prefix: &str,
    context: &dyn Fn() -> Cow<'static, str>,
  ) -> Result<v8::Local<'a, v8::Value>, WebIdlError> {
    match object {
      Some(object) => get_property(scope, object, key, prefix, context),
      None => Ok(v8::undefined(scope).into()),
    }
  }

  /// Converts a value to a string for an enum, with the DOMString rules.
  pub fn enum_string<'a>(
    scope: &mut v8::HandleScope<'a>,
    value: v8::Local<'a, v8::Value>,
    prefix: &str,
    context: &dyn Fn() -> Cow<'static, str>,
  ) -> Result<String, WebIdlError> {
    String::convert(
      scope,
      value,
      prefix.to_owned().into(),
      context,
      &StringOptions::default(),
    )
  }

  /// The context of a dictionary member, given the context of the dictionary.
  pub fn member_context(
    key: &str,
    dictionary: &str,
    context: Cow<'static, str>,
  ) -> Cow<'static, str> {
    Cow::Owned(format!("'{key}' of '{dictionary}' ({context})"))
  }

  /// The error for a required dictionary member that is missing.
  pub fn required_member(
    prefix: Cow<'static, str>,
    context: Cow<'static, str>,
    dictionary: &str,
    key: &str,
  ) -> WebIdlError {
    WebIdlError::new(
      prefix,
      context,
      format!(
        "can not be converted to '{dictionary}' because '{key}' is required in '{dictionary}'."
      ),
    )
  }

  /// The error for a string that is not a value of an enum.
  pub fn invalid_enum_value(
    prefix: Cow<'static, str>,
    context: Cow<'static, str>,
    value: &str,
    enum_name: &str,
  ) -> WebIdlError {
    WebIdlError::new(
      prefix,
      context,
      format!(
        "The provided value '{value}' is not a valid enum value of type {enum_name}."
      ),
    )
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::JsRuntime;
  use crate::RuntimeOptions;

  // The derive refers to `::deno_core`, which is this crate under its own name.
  #[derive(Debug, PartialEq, Eq, crate::WebIDL)]
  #[webidl(dictionary)]
  struct TestDict {
    #[webidl(default = 1)]
    foo_bar: u8,
    #[webidl(rename = "custom-key", default = String::from("x"))]
    renamed: String,
    #[webidl(default = 0)]
    r#type: u8,
    #[webidl(default = 0)]
    #[options(enforce_range = true)]
    ranged: u8,
    maybe: Option<u8>,
    #[webidl(default = Inner { value: 7 })]
    inner: Inner,
  }

  #[derive(Debug, PartialEq, Eq, crate::WebIDL)]
  #[webidl(dictionary)]
  struct Inner {
    #[webidl(default = 7)]
    value: u8,
  }

  #[derive(Debug, PartialEq, Eq, crate::WebIDL)]
  #[webidl(dictionary)]
  struct RequiredDict {
    #[webidl(required)]
    must: u8,
  }

  #[derive(Debug, PartialEq, Eq, crate::WebIDL)]
  #[webidl(enum)]
  enum TestEnum {
    FooBar,
    Foo,
    #[webidl(rename = "Custom Value")]
    Renamed,
  }

  fn eval<'a>(
    scope: &mut v8::HandleScope<'a>,
    source: &str,
  ) -> v8::Local<'a, v8::Value> {
    let source = v8::String::new(scope, source).unwrap();
    let script = v8::Script::compile(scope, source, None).unwrap();
    script.run(scope).unwrap()
  }

  /// Converts the result of `source` with the default options.
  fn convert<T>(source: &str) -> Result<T, WebIdlError>
  where
    T: for<'a> WebIdlConverter<'a>,
  {
    with_scope(|scope| {
      let value = eval(scope, source);
      <T as WebIdlConverter>::convert(
        scope,
        value,
        Cow::Borrowed("Failed"),
        || Cow::Borrowed("Argument 1"),
        &Default::default(),
      )
    })
  }

  fn convert_with<T, O>(source: &str, options: O) -> Result<T, WebIdlError>
  where
    T: for<'a> WebIdlConverter<'a, Options = O>,
  {
    with_scope(|scope| {
      let value = eval(scope, source);
      <T as WebIdlConverter>::convert(
        scope,
        value,
        Cow::Borrowed("Failed"),
        || Cow::Borrowed("Argument 1"),
        &options,
      )
    })
  }

  fn with_scope<R>(f: impl FnOnce(&mut v8::HandleScope) -> R) -> R {
    let mut runtime = JsRuntime::new(RuntimeOptions::default());
    let mut scope = runtime.handle_scope();
    f(&mut scope)
  }

  fn message<T: fmt::Debug>(result: Result<T, WebIdlError>) -> String {
    result.unwrap_err().to_string()
  }

  #[test]
  fn error_display_format() {
    let error = WebIdlError::new(
      "Failed to execute 'fetch'",
      "Argument 1",
      "is not a finite number.",
    );
    assert_eq!(
      error.to_string(),
      "Failed to execute 'fetch': Argument 1 is not a finite number."
    );
    fn assert_error<E: std::error::Error + Send + Sync + 'static>() {}
    assert_error::<WebIdlError>();
  }

  #[test]
  fn context_is_only_built_on_error() {
    let mut runtime = JsRuntime::new(RuntimeOptions::default());
    let scope = &mut runtime.handle_scope();
    let value = eval(scope, "1");
    let result = <u8 as WebIdlConverter>::convert(
      scope,
      value,
      Cow::Borrowed("Failed"),
      || panic!("context must not be built on success"),
      &Default::default(),
    );
    assert_eq!(result, Ok(1));
  }

  #[test]
  fn any_returns_value_unchanged() {
    let mut runtime = JsRuntime::new(RuntimeOptions::default());
    let scope = &mut runtime.handle_scope();
    for source in ["undefined", "null", "({a: 1})"] {
      let value = eval(scope, source);
      let result = <v8::Local<v8::Value> as WebIdlConverter>::convert(
        scope,
        value,
        Cow::Borrowed("Failed"),
        || Cow::Borrowed("Argument 1"),
        &(),
      )
      .unwrap();
      assert!(result.strict_equals(value), "{source}");
    }
  }

  #[test]
  fn boolean_is_to_boolean() {
    for (source, expected) in [
      ("''", false),
      ("'false'", true),
      ("0n", false),
      ("NaN", false),
      ("({})", true),
      ("new Boolean(false)", true),
      ("undefined", false),
    ] {
      assert_eq!(convert::<bool>(source), Ok(expected), "{source}");
    }
  }

  #[test]
  fn integer_default_wraps() {
    assert_eq!(convert::<u8>("300"), Ok(44));
    assert_eq!(convert::<u8>("-1"), Ok(255));
    assert_eq!(convert::<i8>("128"), Ok(-128));
    assert_eq!(convert::<u32>("2 ** 32 + 5"), Ok(5));
    assert_eq!(convert::<i32>("2 ** 31"), Ok(i32::MIN));
    assert_eq!(convert::<u64>("2 ** 64"), Ok(0));
    assert_eq!(convert::<u64>("-1"), Ok(u64::MAX));
    assert_eq!(convert::<i64>("2 ** 63"), Ok(i64::MIN));
    assert_eq!(convert::<u16>("1.9"), Ok(1));
    assert_eq!(convert::<i16>("-1.9"), Ok(-1));
  }

  #[test]
  fn integer_default_special_values_are_zero() {
    for source in ["NaN", "Infinity", "-0", "'abc'", "null", "undefined", "[]"]
    {
      assert_eq!(convert::<i32>(source), Ok(0), "{source}");
    }
    assert_eq!(convert::<u8>("'12'"), Ok(12));
  }

  #[test]
  fn integer_enforce_range() {
    let options = IntOptions {
      enforce_range: true,
      clamp: false,
    };
    assert_eq!(convert_with::<u8, _>("255.9", options), Ok(255));
    assert_eq!(convert_with::<u8, _>("-0.9", options), Ok(0));
    assert_eq!(
      message(convert_with::<u8, _>("256", options)),
      "Failed: Argument 1 is outside the accepted range of 0 to 255, inclusive."
    );
    assert_eq!(
      message(convert_with::<u8, _>("-1", options)),
      "Failed: Argument 1 is outside the accepted range of 0 to 255, inclusive."
    );
    assert_eq!(
      message(convert_with::<u8, _>("NaN", options)),
      "Failed: Argument 1 is not a finite number."
    );
    assert_eq!(
      message(convert_with::<i32, _>("Infinity", options)),
      "Failed: Argument 1 is not a finite number."
    );
    assert_eq!(
      message(convert_with::<u64, _>("2 ** 53", options)),
      "Failed: Argument 1 is outside the accepted range of 0 to 9007199254740991, inclusive."
    );
  }

  #[test]
  fn integer_clamp() {
    let options = IntOptions {
      enforce_range: false,
      clamp: true,
    };
    assert_eq!(convert_with::<u8, _>("2.5", options), Ok(2));
    assert_eq!(convert_with::<u8, _>("3.5", options), Ok(4));
    assert_eq!(convert_with::<u8, _>("-1", options), Ok(0));
    assert_eq!(convert_with::<u8, _>("1e10", options), Ok(255));
    assert_eq!(convert_with::<u8, _>("Infinity", options), Ok(255));
    assert_eq!(convert_with::<u8, _>("NaN", options), Ok(0));
    assert_eq!(convert_with::<i8, _>("-1e10", options), Ok(-128));
  }

  #[test]
  fn integer_enforce_range_wins_over_clamp() {
    let options = IntOptions {
      enforce_range: true,
      clamp: true,
    };
    assert!(convert_with::<u8, _>("256", options).is_err());
    assert_eq!(convert_with::<u8, _>("2.5", options), Ok(2));
  }

  #[test]
  fn integer_rejects_bigint_and_symbol() {
    assert!(convert::<u8>("10n").is_err());
    assert!(convert::<u8>("Symbol()").is_err());
  }

  #[test]
  fn float_and_double() {
    assert_eq!(convert::<f64>("'1.5'"), Ok(1.5));
    assert_eq!(
      message(convert::<f64>("undefined")),
      "Failed: Argument 1 is not a finite number."
    );
    assert_eq!(
      message(convert::<f32>("3.5e38")),
      "Failed: Argument 1 is not a finite number."
    );
    assert!(convert::<f64>("1n").is_err());
    assert!(convert::<f64>("Symbol()").is_err());
    assert_eq!(convert::<f32>("1.5"), Ok(1.5));
  }

  #[test]
  fn unrestricted_float_and_double() {
    assert!(convert::<UnrestrictedDouble>("NaN").unwrap().is_nan());
    assert_eq!(
      convert::<UnrestrictedDouble>("-Infinity").unwrap().0,
      f64::NEG_INFINITY
    );
    assert_eq!(
      convert::<UnrestrictedFloat>("3.5e38").unwrap().0,
      f32::INFINITY
    );
    assert!(convert::<UnrestrictedFloat>("NaN").unwrap().is_nan());
    let negative_zero = convert::<UnrestrictedDouble>("-0").unwrap();
    assert!(negative_zero.0.is_sign_negative());
    let negative_zero = convert::<f32>("-0").unwrap();
    assert!(negative_zero.is_sign_negative());
  }

  #[test]
  fn domstring() {
    assert_eq!(convert::<String>("null"), Ok("null".to_string()));
    assert_eq!(convert::<String>("undefined"), Ok("undefined".to_string()));
    assert_eq!(convert::<String>("1"), Ok("1".to_string()));
    assert_eq!(convert::<String>("''"), Ok(String::new()));
    assert_eq!(convert::<String>("'a\\0b'"), Ok("a\0b".to_string()));
    assert_eq!(convert::<String>("'\\uD800'"), Ok("\u{FFFD}".to_string()));
    assert!(convert::<String>("Symbol()").is_err());
    let big = convert::<String>("'x'.repeat(1024 * 1024)").unwrap();
    assert_eq!(big.len(), 1024 * 1024);
  }

  #[test]
  fn domstring_treat_null_as_empty_string() {
    let options = StringOptions {
      treat_null_as_empty_string: true,
    };
    assert_eq!(
      convert_with::<String, _>("null", options),
      Ok(String::new())
    );
    assert_eq!(
      convert_with::<String, _>("undefined", options),
      Ok("undefined".to_string())
    );
  }

  #[test]
  fn bytestring() {
    assert_eq!(
      convert::<ByteString>("'\\xFF'"),
      Ok(ByteString("\u{FF}".to_string()))
    );
    assert_eq!(
      convert::<ByteString>("'\\xE9'"),
      Ok(ByteString("\u{E9}".to_string()))
    );
    assert_eq!(convert::<ByteString>("''"), Ok(ByteString(String::new())));
    assert_eq!(
      message(convert::<ByteString>("'\\u20AC'")),
      "Failed: Argument 1 contains a character that is not a valid ByteString code unit."
    );
    assert!(convert::<ByteString>("'\\uD800'").is_err());
  }

  #[test]
  fn bigint() {
    assert_eq!(
      convert::<BigInt>("2n ** 200n"),
      Ok(BigInt {
        sign: false,
        words: vec![0, 0, 0, 1 << 8],
      })
    );
    assert_eq!(
      convert::<BigInt>("-(2n ** 64n) - 1n"),
      Ok(BigInt {
        sign: true,
        words: vec![1, 1],
      })
    );
    assert_eq!(
      convert::<BigInt>("-1n"),
      Ok(BigInt {
        sign: true,
        words: vec![1],
      })
    );
    assert_eq!(
      convert::<BigInt>("0n"),
      Ok(BigInt {
        sign: false,
        words: vec![],
      })
    );
    assert_eq!(
      convert::<BigInt>("true"),
      Ok(BigInt {
        sign: false,
        words: vec![1],
      })
    );
    assert_eq!(convert::<BigInt>("'0x10'").unwrap().words, vec![16]);
    assert_eq!(convert::<BigInt>("' 42 '").unwrap().words, vec![42]);
    assert_eq!(convert::<BigInt>("''").unwrap().words, Vec::<u64>::new());
    assert!(convert::<BigInt>("1").is_err());
    assert!(convert::<BigInt>("undefined").is_err());
    assert!(convert::<BigInt>("null").is_err());
    assert!(convert::<BigInt>("Symbol()").is_err());
    assert!(convert::<BigInt>("'1.5'").is_err());
    assert!(convert::<BigInt>("'abc'").is_err());
  }

  #[test]
  fn optional() {
    assert_eq!(convert::<Option<u8>>("undefined"), Ok(None));
    assert_eq!(convert::<Option<u8>>("null"), Ok(None));
    assert_eq!(convert::<Option<u8>>("5"), Ok(Some(5)));
    assert_eq!(convert::<Option<u8>>("'x'"), Ok(Some(0)));
    assert_eq!(
      message(convert::<Option<f64>>("NaN")),
      "Failed: Argument 1 is not a finite number."
    );
  }

  #[test]
  fn sequence() {
    assert_eq!(convert::<Vec<u8>>("[1, 'x', 3]"), Ok(vec![1, 0, 3]));
    assert_eq!(convert::<Vec<u8>>("[1,,3]"), Ok(vec![1, 0, 3]));
    assert_eq!(
      convert::<Vec<Vec<u8>>>("[[1], [2, 3], []]"),
      Ok(vec![vec![1], vec![2, 3], vec![]])
    );
    assert_eq!(convert::<Vec<u8>>("new Set([4, 5])"), Ok(vec![4, 5]));
    assert_eq!(
      convert::<Vec<u8>>("(function* () { yield 6; yield 7; })()"),
      Ok(vec![6, 7])
    );
    assert_eq!(convert::<Vec<u8>>("new Uint8Array([8, 9])"), Ok(vec![8, 9]));
    assert_eq!(
      convert::<Vec<u32>>("Array.from({ length: 100000 }, (_, i) => i)")
        .unwrap()
        .len(),
      100000
    );
    assert_eq!(
      message(convert::<Vec<Vec<u8>>>("[[1], 'x']")),
      "Failed: Argument 1, index 1 is not an object."
    );
  }

  #[test]
  fn sequence_errors() {
    for source in [
      "'abc'",
      "1",
      "null",
      "undefined",
      "({ length: 2, 0: 1, 1: 2 })",
    ] {
      assert!(convert::<Vec<u8>>(source).is_err(), "{source}");
    }
    assert_eq!(
      message(convert::<Vec<u8>>("({ length: 2, 0: 1, 1: 2 })")),
      "Failed: Argument 1 is not iterable."
    );
    assert_eq!(
      message(convert::<Vec<u8>>(
        "({ [Symbol.iterator]() { return { next() { return 1 } } } })"
      )),
      "Failed: Argument 1 is not iterable: an iterator result is not an object."
    );
    assert!(convert::<Vec<u8>>(
      "({ [Symbol.iterator]() { return { next() { throw 1 } } } })"
    )
    .is_err());
  }

  #[test]
  fn record() {
    assert_eq!(
      convert::<HashMap<String, u8>>("({a: 'x'})"),
      Ok(HashMap::from([("a".to_string(), 0)]))
    );
    assert_eq!(
      convert::<HashMap<String, u8>>("Object.create({inherited: 1})"),
      Ok(HashMap::new())
    );
    assert_eq!(
      convert::<HashMap<String, u8>>(
        "Object.defineProperty({}, 'x', { value: 1, enumerable: false })"
      ),
      Ok(HashMap::new())
    );
    assert_eq!(
      convert::<HashMap<String, u8>>("({ 1: 2, b: 3 })"),
      Ok(HashMap::from([("1".to_string(), 2), ("b".to_string(), 3)]))
    );
    assert_eq!(
      convert::<HashMap<String, u8>>(
        "new Proxy({ p: 4 }, { ownKeys(t) { return Reflect.ownKeys(t) } })"
      ),
      Ok(HashMap::from([("p".to_string(), 4)]))
    );
    assert!(convert::<HashMap<String, u8>>("({[Symbol()]: 1})").is_err());
    assert_eq!(
      message(convert::<HashMap<String, u8>>("null")),
      "Failed: Argument 1 is not an object."
    );
    assert!(convert::<HashMap<String, u8>>("'abc'").is_err());
    assert_eq!(
      convert::<HashMap<String, u8>>(
        "Object.defineProperty({}, Symbol('s'), { value: 1, enumerable: false })"
      ),
      Ok(HashMap::new())
    );
  }

  #[test]
  fn record_checks_enumerability_when_each_key_is_reached() {
    // The getter for `b` deletes `c` before `c` is reached, so `c` is skipped.
    assert_eq!(
      convert::<HashMap<String, u8>>(
        "({a: 1, get b() { delete this.c; return 2 }, c: 3})"
      ),
      Ok(HashMap::from([("a".to_string(), 1), ("b".to_string(), 2)]))
    );
  }

  #[test]
  fn js_exceptions_become_errors() {
    assert_eq!(
      message(convert::<u8>("({ valueOf() { throw new Error('boom') } })")),
      "Failed: Argument 1 boom"
    );
    assert_eq!(
      message(convert::<u8>("({ valueOf() { throw 1 } })")),
      "Failed: Argument 1 1"
    );
    assert_eq!(
      message(convert::<u8>(
        "({ valueOf() { throw { toString() { throw 1 } } } })"
      )),
      "Failed: Argument 1 an exception was thrown"
    );
    assert_eq!(
      message(convert::<Vec<u8>>(
        "({ [Symbol.iterator]() { return { next() { throw 1 } } } })"
      )),
      "Failed: Argument 1 1"
    );
    assert_eq!(
      message(convert::<HashMap<String, u8>>(
        "new Proxy({}, { ownKeys() { throw new Error('trap') } })"
      )),
      "Failed: Argument 1 trap"
    );
  }

  // Member names that match the names used inside the generated `convert`.
  #[derive(Debug, PartialEq, Eq, crate::WebIDL)]
  #[webidl(dictionary)]
  struct ShadowingDict {
    #[webidl(default = 0)]
    scope: u8,
    #[webidl(default = 0)]
    context: u8,
    #[webidl(default = 0)]
    prefix: u8,
    #[webidl(default = 0)]
    value: u8,
    #[webidl(default = 0)]
    options: u8,
    #[webidl(default = 0)]
    __object: u8,
  }

  #[test]
  fn dictionary_members_named_like_generated_locals() {
    assert_eq!(
      convert::<ShadowingDict>(
        "({scope: 1, context: 2, prefix: 3, value: 4, options: 5})"
      ),
      Ok(ShadowingDict {
        scope: 1,
        context: 2,
        prefix: 3,
        value: 4,
        options: 5,
        __object: 0,
      })
    );
    assert_eq!(
      convert::<ShadowingDict>("({})"),
      Ok(ShadowingDict {
        scope: 0,
        context: 0,
        prefix: 0,
        value: 0,
        options: 0,
        __object: 0,
      })
    );
  }

  #[test]
  fn dictionary_camel_case_rename_and_defaults() {
    assert_eq!(
      convert::<TestDict>("({ fooBar: 2, 'custom-key': 'y', type: 3, ranged: 4, maybe: 5, inner: { value: 9 } })"),
      Ok(TestDict {
        foo_bar: 2,
        renamed: "y".to_string(),
        r#type: 3,
        ranged: 4,
        maybe: Some(5),
        inner: Inner { value: 9 },
      })
    );
    assert_eq!(
      convert::<TestDict>("({ ranged: 1, maybe: null })"),
      Ok(TestDict {
        foo_bar: 1,
        renamed: "x".to_string(),
        r#type: 0,
        ranged: 1,
        maybe: None,
        inner: Inner { value: 7 },
      })
    );
  }

  #[test]
  fn dictionary_undefined_and_null_are_empty() {
    for source in ["undefined", "null"] {
      assert_eq!(
        convert::<TestDict>(source),
        Ok(TestDict {
          foo_bar: 1,
          renamed: "x".to_string(),
          r#type: 0,
          ranged: 0,
          maybe: None,
          inner: Inner { value: 7 },
        }),
        "{source}"
      );
    }
    assert_eq!(
      message(convert::<RequiredDict>("undefined")),
      "Failed: Argument 1 can not be converted to 'RequiredDict' because 'must' is required in 'RequiredDict'."
    );
  }

  #[test]
  fn dictionary_non_object_is_error() {
    for source in ["1", "'x'", "true", "Symbol()", "1n"] {
      assert_eq!(
        message(convert::<TestDict>(source)),
        "Failed: Argument 1 can not be converted to 'TestDict' because it is not an object.",
        "{source}"
      );
    }
  }

  #[test]
  fn dictionary_required_member() {
    assert_eq!(
      message(convert::<RequiredDict>("({})")),
      "Failed: Argument 1 can not be converted to 'RequiredDict' because 'must' is required in 'RequiredDict'."
    );
    assert_eq!(
      convert::<RequiredDict>("({ must: 3 })"),
      Ok(RequiredDict { must: 3 })
    );
  }

  #[test]
  fn dictionary_member_errors_name_the_member() {
    assert_eq!(
      message(convert::<TestDict>("({ ranged: 256, fooBar: 1 })")),
      "Failed: 'ranged' of 'TestDict' (Argument 1) is outside the accepted range of 0 to 255, inclusive."
    );
    assert_eq!(
      message(convert::<TestDict>(
        "({ get inner() { throw new Error('getter') } })"
      )),
      "Failed: 'inner' of 'TestDict' (Argument 1) getter"
    );
    assert_eq!(
      message(convert::<TestDict>("({ inner: 1 })")),
      "Failed: 'inner' of 'TestDict' (Argument 1) can not be converted to 'Inner' because it is not an object."
    );
  }

  #[test]
  fn dictionary_members_are_read_in_lexicographic_order() {
    let source = r#"
      (() => {
        const log = [];
        const dict = {};
        for (const key of ["renamed", "maybe", "inner", "ranged", "type", "fooBar"]) {
          Object.defineProperty(dict, key, { get() { log.push(key); return undefined }, enumerable: true });
        }
        Object.defineProperty(dict, "custom-key", { get() { log.push("custom-key"); return undefined }, enumerable: true });
        globalThis.__log = log;
        return dict;
      })()
    "#;
    let mut runtime = JsRuntime::new(RuntimeOptions::default());
    let scope = &mut runtime.handle_scope();
    let value = eval(scope, source);
    let result = <TestDict as WebIdlConverter>::convert(
      scope,
      value,
      Cow::Borrowed("Failed"),
      || Cow::Borrowed("Argument 1"),
      &(),
    );
    assert!(result.is_ok());
    let log = eval(scope, "globalThis.__log.join(',')");
    let log = log.to_rust_string_lossy(scope);
    assert_eq!(log, "custom-key,fooBar,inner,maybe,ranged,type");
  }

  #[test]
  fn enum_kebab_case_rename_and_invalid_values() {
    assert_eq!(convert::<TestEnum>("'foo-bar'"), Ok(TestEnum::FooBar));
    assert_eq!(convert::<TestEnum>("'foo'"), Ok(TestEnum::Foo));
    assert_eq!(convert::<TestEnum>("'Custom Value'"), Ok(TestEnum::Renamed));
    assert_eq!(
      message(convert::<TestEnum>("'bar'")),
      "Failed: Argument 1 The provided value 'bar' is not a valid enum value of type TestEnum."
    );
    for source in ["'Foo-Bar'", "'foo-bar '", "''", "undefined"] {
      assert!(convert::<TestEnum>(source).is_err(), "{source}");
    }
    assert!(convert::<TestEnum>("Symbol()").is_err());
  }
}
