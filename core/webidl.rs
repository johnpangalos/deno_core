// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.

//! Conversion of JavaScript values into Rust types, following the WebIDL
//! [JavaScript-to-IDL type mapping](https://webidl.spec.whatwg.org/#js-type-mapping).
//!
//! Implement [`WebIdlConverter`] for a type to convert JavaScript values into
//! it. Dictionaries and enums can be derived with `#[derive(WebIDL)]`
//! (re-exported as [`crate::WebIDL`]). Use `#[webidl]` on an `#[op2]` argument
//! to have the op convert the argument with this module; a failed conversion
//! throws a `TypeError`, and an exception thrown by JavaScript during the
//! conversion is rethrown unchanged.
//!
//! Integer converters (`i8` through `u64`) wrap out-of-range values by default,
//! as the WebIDL specification does. `[EnforceRange]` and `[Clamp]` are selected
//! with [`IntOptions`]; when both are set, `enforce_range` wins.
//!
//! # Example
//!
//! ```ignore
//! use deno_core::webidl::IntOptions;
//! use deno_core::webidl::WebIdlConverter;
//!
//! let options = IntOptions { enforce_range: true, ..Default::default() };
//! let port = u16::convert(scope, value, "Failed to execute 'listen'".into(), || "Argument 1".into(), &options)?;
//! ```

use std::borrow::Cow;
use std::collections::HashMap;
use std::fmt;

use crate::v8;

/// Converts a JavaScript value into `Self`, following the WebIDL
/// JavaScript-to-IDL type mapping.
///
/// `prefix` names the operation that is being performed (for example
/// `Failed to execute 'fetch'`). `context` returns the location of the value
/// (for example `Argument 1`); it is only called when an error is produced, and
/// nested conversions extend it (for example `Argument 1, sequence index 0`).
/// `options` tunes the conversion, such as `[EnforceRange]` or `[Clamp]`.
pub trait WebIdlConverter<'a>: Sized {
  /// Options that tune the conversion. Defaults to the WebIDL default behavior.
  type Options: Default;

  /// Converts `value` into `Self`.
  ///
  /// Returns a [`WebIdlError`] if the value does not fit the WebIDL type. If
  /// JavaScript code throws while the value is being converted, a
  /// [`WebIdlError`] is also returned, and the exception stays pending in
  /// `scope`.
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

/// An error produced when a JavaScript value can not be converted to a WebIDL
/// type.
///
/// The message has the form `"{prefix}: {context} {message}"`, for example
/// `Failed to execute 'fetch': Argument 1 is not a finite floating-point value.`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebIdlError {
  prefix: Cow<'static, str>,
  context: Cow<'static, str>,
  message: Cow<'static, str>,
}

impl WebIdlError {
  /// Creates an error. `prefix` names the operation, `context` names where the
  /// value came from, and `message` describes what was wrong with it.
  pub fn new(
    prefix: Cow<'static, str>,
    context: Cow<'static, str>,
    message: impl Into<Cow<'static, str>>,
  ) -> Self {
    Self {
      prefix,
      context,
      message: message.into(),
    }
  }
}

impl fmt::Display for WebIdlError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "{}: {} {}", self.prefix, self.context, self.message)
  }
}

impl std::error::Error for WebIdlError {}

const NOT_FINITE: &str = "is not a finite floating-point value.";
const EXCEPTION: &str =
  "could not be converted because JavaScript threw an exception.";

fn error<C>(
  prefix: &str,
  context: &C,
  message: impl Into<Cow<'static, str>>,
) -> WebIdlError
where
  C: Fn() -> Cow<'static, str>,
{
  WebIdlError::new(Cow::Owned(prefix.to_owned()), context(), message)
}

fn exception_error<C>(prefix: &str, context: &C) -> WebIdlError
where
  C: Fn() -> Cow<'static, str>,
{
  error(prefix, context, EXCEPTION)
}

fn key<'a>(
  scope: &mut v8::HandleScope<'a>,
  name: &str,
) -> v8::Local<'a, v8::Value> {
  v8::String::new(scope, name)
    .expect("WebIDL property names are short ASCII strings")
    .into()
}

fn get_property<'a, C>(
  scope: &mut v8::HandleScope<'a>,
  object: v8::Local<'a, v8::Object>,
  name: v8::Local<'a, v8::Value>,
  prefix: &str,
  context: &C,
) -> Result<v8::Local<'a, v8::Value>, WebIdlError>
where
  C: Fn() -> Cow<'static, str>,
{
  object
    .get(scope, name)
    .ok_or_else(|| exception_error(prefix, context))
}

fn to_number<'a, C>(
  scope: &mut v8::HandleScope<'a>,
  value: v8::Local<'a, v8::Value>,
  prefix: &str,
  context: &C,
) -> Result<f64, WebIdlError>
where
  C: Fn() -> Cow<'static, str>,
{
  if value.is_symbol() {
    return Err(error(
      prefix,
      context,
      "is a Symbol and can not be converted to a number.",
    ));
  }
  if value.is_big_int() {
    return Err(error(
      prefix,
      context,
      "is a BigInt and can not be converted to a number.",
    ));
  }
  value
    .number_value(scope)
    .ok_or_else(|| exception_error(prefix, context))
}

/// Options for the integer converters (`i8` through `u64`).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct IntOptions {
  /// `[Clamp]`: out-of-range values are clamped to the range of the type, NaN
  /// becomes 0, and the result is rounded to the nearest integer with ties to
  /// even.
  pub clamp: bool,
  /// `[EnforceRange]`: NaN and infinities, and values outside the range of the
  /// type, are errors. Values are truncated towards zero.
  pub enforce_range: bool,
}

fn convert_integer<C>(
  x: f64,
  bits: u32,
  min: f64,
  max: f64,
  options: &IntOptions,
  prefix: &str,
  context: &C,
) -> Result<i128, WebIdlError>
where
  C: Fn() -> Cow<'static, str>,
{
  if options.enforce_range {
    if !x.is_finite() {
      return Err(error(prefix, context, "is not a finite number."));
    }
    let truncated = x.trunc();
    if truncated < min || truncated > max {
      return Err(error(
        prefix,
        context,
        format!("is outside the range [{min}, {max}]."),
      ));
    }
    return Ok(truncated as i128);
  }
  if options.clamp {
    if x.is_nan() {
      return Ok(0);
    }
    return Ok(x.clamp(min, max).round_ties_even() as i128);
  }
  if !x.is_finite() {
    return Ok(0);
  }
  // Reduce modulo 2^bits. `%` on floats is exact, so this is exact even for
  // large values; the caller truncates the result to the target type.
  let modulus = (1u128 << bits) as f64;
  Ok((x.trunc() % modulus) as i128)
}

macro_rules! webidl_integer {
  ($ty:ty, $bits:expr, $min:expr, $max:expr) => {
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
        let x = to_number(scope, value, &prefix, &context)?;
        let v =
          convert_integer(x, $bits, $min, $max, options, &prefix, &context)?;
        Ok(v as $ty)
      }
    }
  };
}

// The `long long` and `unsigned long long` ranges are limited to the integers
// that a JavaScript number can represent exactly.
webidl_integer!(i8, 8, -128.0, 127.0);
webidl_integer!(u8, 8, 0.0, 255.0);
webidl_integer!(i16, 16, -32768.0, 32767.0);
webidl_integer!(u16, 16, 0.0, 65535.0);
webidl_integer!(i32, 32, -2147483648.0, 2147483647.0);
webidl_integer!(u32, 32, 0.0, 4294967295.0);
webidl_integer!(i64, 64, -9007199254740991.0, 9007199254740991.0);
webidl_integer!(u64, 64, 0.0, 9007199254740991.0);

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
      return Err(error(&prefix, &context, NOT_FINITE));
    }
    Ok(x)
  }
}

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
    let x = to_number(scope, value, &prefix, &context)?;
    // A finite value that rounds to infinity as an f32 is out of range too.
    let y = x as f32;
    if !x.is_finite() || !y.is_finite() {
      return Err(error(&prefix, &context, NOT_FINITE));
    }
    Ok(y)
  }
}

/// `unrestricted float`: like `f32`, but NaN and infinities are allowed.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct UnrestrictedFloat(pub f32);

impl std::ops::Deref for UnrestrictedFloat {
  type Target = f32;
  fn deref(&self) -> &f32 {
    &self.0
  }
}

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
    Ok(Self(to_number(scope, value, &prefix, &context)? as f32))
  }
}

/// `unrestricted double`: like `f64`, but NaN and infinities are allowed.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct UnrestrictedDouble(pub f64);

impl std::ops::Deref for UnrestrictedDouble {
  type Target = f64;
  fn deref(&self) -> &f64 {
    &self.0
  }
}

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
    Ok(Self(to_number(scope, value, &prefix, &context)?))
  }
}

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
    Ok(value.boolean_value(scope))
  }
}

/// Returns any JavaScript value unchanged.
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

/// Options for the string converters.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct StringOptions {
  /// `[LegacyNullToEmptyString]`: `null` converts to the empty string instead
  /// of `"null"`.
  pub treat_null_as_empty_string: bool,
}

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
    if value.is_symbol() {
      return Err(error(
        &prefix,
        &context,
        "is a Symbol and can not be converted to a string.",
      ));
    }
    if options.treat_null_as_empty_string && value.is_null() {
      return Ok(String::new());
    }
    let string = value
      .to_string(scope)
      .ok_or_else(|| exception_error(&prefix, &context))?;
    // Lone surrogates are replaced with U+FFFD.
    Ok(string.to_rust_string_lossy(scope))
  }
}

/// `ByteString`: a string whose characters are all in the range U+0000 to
/// U+00FF, so that each character is one byte.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ByteString(pub String);

impl std::ops::Deref for ByteString {
  type Target = str;
  fn deref(&self) -> &str {
    &self.0
  }
}

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
    let string = <String as WebIdlConverter<'a>>::convert(
      scope,
      value,
      prefix.clone(),
      &context,
      options,
    )?;
    if string.chars().any(|c| c > '\u{ff}') {
      return Err(error(
        &prefix,
        &context,
        "is not a valid ByteString because it has a code unit above U+00FF.",
      ));
    }
    Ok(Self(string))
  }
}

/// `bigint`: an arbitrary precision integer.
///
/// The magnitude is stored as 64-bit words, least significant first, with no
/// leading zero words. Zero has no words.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct BigInt {
  /// True for negative values.
  pub sign: bool,
  /// The magnitude, least significant word first.
  pub words: Vec<u64>,
}

fn bigint_from_v8(big: v8::Local<v8::BigInt>) -> BigInt {
  let mut words = vec![0u64; big.word_count()];
  let (sign, _) = big.to_words_array(&mut words);
  BigInt { sign, words }
}

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
    if let Ok(big) = v8::Local::<v8::BigInt>::try_from(value) {
      return Ok(bigint_from_v8(big));
    }
    if value.is_boolean() {
      let words = if value.is_true() { vec![1] } else { vec![] };
      return Ok(BigInt { sign: false, words });
    }
    if value.is_number() || value.is_null_or_undefined() || value.is_symbol() {
      return Err(error(
        &prefix,
        &context,
        "can not be converted to a BigInt.",
      ));
    }
    let big = if value.is_string() {
      // Parsing a string runs no user code, so a failure here is a syntax
      // error to report, not an exception to rethrow.
      let mut tc = v8::TryCatch::new(scope);
      let tc_scope: &mut v8::HandleScope<'a> = tc.as_mut();
      value.to_big_int(tc_scope).ok_or_else(|| {
        error(&prefix, &context, "is not a valid BigInt string.")
      })?
    } else {
      value
        .to_big_int(scope)
        .ok_or_else(|| exception_error(&prefix, &context))?
    };
    Ok(bigint_from_v8(big))
  }
}

impl<'a, T> WebIdlConverter<'a> for Option<T>
where
  T: WebIdlConverter<'a>,
{
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
    if value.is_null_or_undefined() {
      return Ok(None);
    }
    T::convert(scope, value, prefix, context, options).map(Some)
  }
}

impl<'a, T> WebIdlConverter<'a> for Vec<T>
where
  T: WebIdlConverter<'a>,
{
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
    let Ok(object) = v8::Local::<v8::Object>::try_from(value) else {
      return Err(error(
        &prefix,
        &context,
        "can not be converted to a sequence because it is not an object.",
      ));
    };
    let iterator_symbol: v8::Local<v8::Value> =
      v8::Symbol::get_iterator(scope).into();
    let method =
      get_property(scope, object, iterator_symbol, &prefix, &context)?;
    let Ok(method) = v8::Local::<v8::Function>::try_from(method) else {
      return Err(error(
        &prefix,
        &context,
        "can not be converted to a sequence because it is not iterable.",
      ));
    };
    let iterator = method
      .call(scope, value, &[])
      .ok_or_else(|| exception_error(&prefix, &context))?;
    let Ok(iterator) = v8::Local::<v8::Object>::try_from(iterator) else {
      return Err(error(
        &prefix,
        &context,
        "can not be converted to a sequence because its iterator is not an object.",
      ));
    };
    let next_key = key(scope, "next");
    let next = get_property(scope, iterator, next_key, &prefix, &context)?;
    let Ok(next) = v8::Local::<v8::Function>::try_from(next) else {
      return Err(error(
        &prefix,
        &context,
        "can not be converted to a sequence because its iterator has no next method.",
      ));
    };
    let done_key = key(scope, "done");
    let value_key = key(scope, "value");

    let mut items = Vec::new();
    loop {
      let result = next
        .call(scope, iterator.into(), &[])
        .ok_or_else(|| exception_error(&prefix, &context))?;
      let Ok(result) = v8::Local::<v8::Object>::try_from(result) else {
        return Err(error(
          &prefix,
          &context,
          "can not be converted to a sequence because an iterator result is not an object.",
        ));
      };
      let done = get_property(scope, result, done_key, &prefix, &context)?;
      if done.boolean_value(scope) {
        break;
      }
      let item = get_property(scope, result, value_key, &prefix, &context)?;
      let index = items.len();
      let item = T::convert(
        scope,
        item,
        prefix.clone(),
        || Cow::Owned(format!("{}, sequence index {index}", context())),
        options,
      )?;
      items.push(item);
    }
    Ok(items)
  }
}

impl<'a, T> WebIdlConverter<'a> for HashMap<String, T>
where
  T: WebIdlConverter<'a>,
{
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
    let Ok(object) = v8::Local::<v8::Object>::try_from(value) else {
      return Err(error(
        &prefix,
        &context,
        "can not be converted to a record because it is not an object.",
      ));
    };
    // Own keys in `Reflect.ownKeys` order, including Symbols so that enumerable
    // Symbol keys can be reported.
    let keys = object
      .get_own_property_names(
        scope,
        v8::GetPropertyNamesArgs {
          mode: v8::KeyCollectionMode::OwnOnly,
          property_filter: v8::PropertyFilter::ALL_PROPERTIES,
          index_filter: v8::IndexFilter::IncludeIndices,
          key_conversion: v8::KeyConversionMode::ConvertToString,
        },
      )
      .ok_or_else(|| exception_error(&prefix, &context))?;

    let mut map = HashMap::new();
    for index in 0..keys.length() {
      let Some(property_key) = keys.get_index(scope, index) else {
        return Err(exception_error(&prefix, &context));
      };
      let name_key = v8::Local::<v8::Name>::try_from(property_key)
        .expect("own property keys are strings or symbols");
      // An exception from a descriptor trap or getter is an error. An undefined
      // descriptor means the property has gone, for example because an earlier
      // member deleted it, so the key is skipped.
      let descriptor = object
        .get_own_property_descriptor(scope, name_key)
        .ok_or_else(|| exception_error(&prefix, &context))?;
      if descriptor.is_undefined() {
        continue;
      }
      let descriptor = descriptor
        .to_object(scope)
        .ok_or_else(|| exception_error(&prefix, &context))?;
      let enumerable_key = key(scope, "enumerable");
      let enumerable =
        get_property(scope, descriptor, enumerable_key, &prefix, &context)?
          .boolean_value(scope);
      if !enumerable {
        continue;
      }
      if property_key.is_symbol() {
        return Err(error(
          &prefix,
          &context,
          "can not be converted to a record because it has an enumerable Symbol key.",
        ));
      }
      let name = property_key
        .to_string(scope)
        .ok_or_else(|| exception_error(&prefix, &context))?
        .to_rust_string_lossy(scope);
      let property =
        get_property(scope, object, property_key, &prefix, &context)?;
      let converted = T::convert(
        scope,
        property,
        prefix.clone(),
        || Cow::Owned(format!("{}, record key '{name}'", context())),
        options,
      )?;
      map.insert(name, converted);
    }
    Ok(map)
  }
}

/// Converts the argument of an `#[op2]` op that has the `#[webidl]` attribute.
///
/// `prefix` names the op and `position` is the 1-based JavaScript argument
/// position. If JavaScript threw during the conversion, that exception is
/// returned unchanged. Otherwise the error is returned as a `TypeError`.
#[doc(hidden)]
pub fn convert_op_arg<'a, T>(
  scope: &mut v8::HandleScope<'a>,
  value: v8::Local<'a, v8::Value>,
  prefix: &'static str,
  position: usize,
) -> Result<T, v8::Local<'a, v8::Value>>
where
  T: WebIdlConverter<'a>,
{
  let mut tc = v8::TryCatch::new(scope);
  let tc_scope: &mut v8::HandleScope<'a> = tc.as_mut();
  let result = T::convert(
    tc_scope,
    value,
    Cow::Borrowed(prefix),
    || Cow::Owned(format!("Argument {position}")),
    &Default::default(),
  );
  match result {
    Ok(value) => Ok(value),
    Err(err) => match tc.exception() {
      Some(exception) => Err(exception),
      None => {
        let message = err.to_string();
        let message = v8::String::new(&mut tc, &message)
          .unwrap_or_else(|| v8::String::empty(&mut tc));
        Err(v8::Exception::type_error(&mut tc, message))
      }
    },
  }
}

/// Returns the members object of a dictionary value. Returns `None` for
/// `undefined` and `null`, which treat every member as missing.
#[doc(hidden)]
pub fn dictionary_object<'a, C>(
  value: v8::Local<'a, v8::Value>,
  prefix: &str,
  context: &C,
  name: &'static str,
) -> Result<Option<v8::Local<'a, v8::Object>>, WebIdlError>
where
  C: Fn() -> Cow<'static, str>,
{
  if value.is_null_or_undefined() {
    return Ok(None);
  }
  match v8::Local::<v8::Object>::try_from(value) {
    Ok(object) => Ok(Some(object)),
    Err(_) => Err(error(
      prefix,
      context,
      format!("can not be converted to '{name}' because it is not an object."),
    )),
  }
}

/// Reads a dictionary member with `Get`. Returns `None` if the member is
/// missing or `undefined`.
#[doc(hidden)]
pub fn dictionary_member<'a, C>(
  scope: &mut v8::HandleScope<'a>,
  dictionary: Option<v8::Local<'a, v8::Object>>,
  member: &'static str,
  prefix: &str,
  context: &C,
) -> Result<Option<v8::Local<'a, v8::Value>>, WebIdlError>
where
  C: Fn() -> Cow<'static, str>,
{
  let Some(dictionary) = dictionary else {
    return Ok(None);
  };
  let name = key(scope, member);
  let value = get_property(scope, dictionary, name, prefix, context)?;
  if value.is_undefined() {
    Ok(None)
  } else {
    Ok(Some(value))
  }
}

/// The error for a dictionary member that is required, or has no default and
/// is not an `Option`, and is missing.
#[doc(hidden)]
pub fn required_member_error<C>(
  prefix: &str,
  context: &C,
  member: &str,
  name: &str,
) -> WebIdlError
where
  C: Fn() -> Cow<'static, str>,
{
  error(
    prefix,
    context,
    format!(
      "can not be converted to '{name}' because '{member}' is required in '{name}'."
    ),
  )
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::JsRuntime;
  use crate::RuntimeOptions;

  fn prefix() -> Cow<'static, str> {
    Cow::Borrowed("Failed to execute 'test'")
  }

  fn context() -> Cow<'static, str> {
    Cow::Borrowed("Argument 1")
  }

  fn eval<'a>(
    scope: &mut v8::HandleScope<'a>,
    source: &str,
  ) -> v8::Local<'a, v8::Value> {
    let code = v8::String::new(scope, source).unwrap();
    let script = v8::Script::compile(scope, code, None).unwrap();
    script.run(scope).unwrap()
  }

  fn message<T>(result: Result<T, WebIdlError>) -> String {
    match result {
      Ok(_) => panic!("expected a conversion error"),
      Err(err) => err.to_string(),
    }
  }

  #[derive(crate::WebIDL)]
  #[webidl(dictionary)]
  #[allow(dead_code)]
  struct Maybe {
    a: Option<u8>,
    b_c: Option<String>,
  }

  #[derive(crate::WebIDL)]
  #[webidl(dictionary)]
  #[allow(dead_code)]
  struct RequiredMember {
    #[webidl(required)]
    foo_bar: Option<u8>,
  }

  #[derive(crate::WebIDL)]
  #[webidl(dictionary)]
  #[allow(dead_code)]
  struct MissingMember {
    foo_bar: u8,
  }

  #[derive(crate::WebIDL)]
  #[webidl(dictionary)]
  struct Defaults {
    #[webidl(default = 7)]
    level: u8,
    #[webidl(default = Some(3))]
    maybe: Option<u8>,
  }

  #[derive(crate::WebIDL)]
  #[webidl(dictionary)]
  struct Ordered {
    #[webidl(default = 0)]
    zeta: u8,
    #[webidl(default = 0)]
    alpha: u8,
    #[webidl(default = 0)]
    mid_value: u8,
  }

  #[derive(crate::WebIDL)]
  #[webidl(dictionary)]
  struct Clamped {
    #[options(clamp = true)]
    #[webidl(default = 0)]
    level: u8,
    #[options(treat_null_as_empty_string = true)]
    #[webidl(default = String::new())]
    name: String,
  }

  #[derive(crate::WebIDL)]
  #[webidl(enum)]
  enum Choice {
    FooBar,
    #[webidl(rename = "custom")]
    Renamed,
  }

  #[test]
  fn dictionaries() {
    let mut runtime = JsRuntime::new(RuntimeOptions::default());
    let scope = &mut runtime.handle_scope();
    let v = eval(scope, "undefined");
    let m = Maybe::convert(scope, v, prefix(), context, &()).unwrap();
    assert!(m.a.is_none() && m.b_c.is_none());
    let v = eval(scope, "null");
    let m = Maybe::convert(scope, v, prefix(), context, &()).unwrap();
    assert!(m.a.is_none() && m.b_c.is_none());
    let v = eval(scope, "({a: null, bC: 'x'})");
    let m = Maybe::convert(scope, v, prefix(), context, &()).unwrap();
    assert!(m.a.is_none());
    assert_eq!(m.b_c.as_deref(), Some("x"));
    let v = eval(scope, "5");
    assert_eq!(
      message(Maybe::convert(scope, v, prefix(), context, &())),
      "Failed to execute 'test': Argument 1 can not be converted to 'Maybe' because it is not an object."
    );
    let v = eval(scope, "({})");
    assert_eq!(
      message(RequiredMember::convert(scope, v, prefix(), context, &())),
      "Failed to execute 'test': Argument 1 can not be converted to 'RequiredMember' because 'fooBar' is required in 'RequiredMember'."
    );
    let v = eval(scope, "({fooBar: undefined})");
    assert_eq!(
      message(RequiredMember::convert(scope, v, prefix(), context, &())),
      "Failed to execute 'test': Argument 1 can not be converted to 'RequiredMember' because 'fooBar' is required in 'RequiredMember'."
    );
    let v = eval(scope, "({fooBar: null})");
    assert!(RequiredMember::convert(scope, v, prefix(), context, &())
      .unwrap()
      .foo_bar
      .is_none());
    let v = eval(scope, "({})");
    assert_eq!(
      message(MissingMember::convert(scope, v, prefix(), context, &())),
      "Failed to execute 'test': Argument 1 can not be converted to 'MissingMember' because 'fooBar' is required in 'MissingMember'."
    );
    // Defaults apply to missing and undefined members, but not to null.
    let v = eval(scope, "({})");
    let d = Defaults::convert(scope, v, prefix(), context, &()).unwrap();
    assert_eq!((d.level, d.maybe), (7, Some(3)));
    let v = eval(scope, "({maybe: null})");
    let d = Defaults::convert(scope, v, prefix(), context, &()).unwrap();
    assert_eq!((d.level, d.maybe), (7, None));
    // Falsy values are present members, not missing ones.
    let v = eval(scope, "({level: 0, maybe: false})");
    let d = Defaults::convert(scope, v, prefix(), context, &()).unwrap();
    assert_eq!((d.level, d.maybe), (0, Some(0)));
    let v = eval(scope, "({level: 300})");
    let d = Clamped::convert(scope, v, prefix(), context, &()).unwrap();
    assert_eq!(d.level, 255);
    assert_eq!(d.name, "");
    let v = eval(scope, "({name: null})");
    let d = Clamped::convert(scope, v, prefix(), context, &()).unwrap();
    assert_eq!(d.name, "");
    let v = eval(scope, "({name: 'x'})");
    let d = Clamped::convert(scope, v, prefix(), context, &()).unwrap();
    assert_eq!(d.name, "x");
  }

  #[test]
  fn dictionary_members_are_read_in_key_order() {
    let mut runtime = JsRuntime::new(RuntimeOptions::default());
    let scope = &mut runtime.handle_scope();
    eval(
      scope,
      r#"
        globalThis.log = [];
        globalThis.o = {
          get zeta() { log.push("zeta"); return 1; },
          get alpha() { log.push("alpha"); return 2; },
          get midValue() { log.push("midValue"); return 3; },
        };
      "#,
    );
    let v = eval(scope, "o");
    let d = Ordered::convert(scope, v, prefix(), context, &()).unwrap();
    assert_eq!((d.alpha, d.mid_value, d.zeta), (2, 3, 1));
    let log = eval(scope, "log.join(',')");
    assert_eq!(log.to_rust_string_lossy(scope), "alpha,midValue,zeta");
  }

  #[derive(crate::WebIDL)]
  #[webidl(dictionary)]
  #[allow(dead_code)]
  struct Outer {
    nested: Option<Vec<Maybe>>,
    by_kind: HashMap<String, Choice>,
  }

  #[test]
  fn nested_dictionaries_compose() {
    let mut runtime = JsRuntime::new(RuntimeOptions::default());
    let scope = &mut runtime.handle_scope();
    let v = eval(
      scope,
      "({nested: [{a: 1}, {bC: 'y'}], byKind: {x: 'foo-bar'}})",
    );
    let outer = Outer::convert(scope, v, prefix(), context, &()).unwrap();
    assert_eq!(outer.nested.as_ref().map(Vec::len), Some(2));
    assert!(matches!(outer.by_kind.get("x"), Some(Choice::FooBar)));
    let v = eval(scope, "({byKind: {x: 'nope'}})");
    assert_eq!(
      message(Outer::convert(scope, v, prefix(), context, &())),
      "Failed to execute 'test': Argument 1, member 'byKind', record key 'x' is not a valid value of enum 'Choice': 'nope'."
    );
  }

  #[test]
  fn enums() {
    let mut runtime = JsRuntime::new(RuntimeOptions::default());
    let scope = &mut runtime.handle_scope();
    let v = eval(scope, "'foo-bar'");
    assert!(matches!(
      Choice::convert(scope, v, prefix(), context, &()),
      Ok(Choice::FooBar)
    ));
    let v = eval(scope, "'custom'");
    assert!(matches!(
      Choice::convert(scope, v, prefix(), context, &()),
      Ok(Choice::Renamed)
    ));
    let v = eval(scope, "({toString() { return 'foo-bar'; }})");
    assert!(matches!(
      Choice::convert(scope, v, prefix(), context, &()),
      Ok(Choice::FooBar)
    ));
    for source in [
      "'FooBar'",
      "'Foo-Bar'",
      "''",
      "'foo-bar '",
      "1",
      "'Renamed'",
    ] {
      let v = eval(scope, source);
      let expected = format!(
        "Failed to execute 'test': Argument 1 is not a valid value of enum 'Choice': '{}'.",
        match source {
          "1" => "1".to_string(),
          _ => source.trim_matches('\'').to_string(),
        }
      );
      assert_eq!(
        message(Choice::convert(scope, v, prefix(), context, &())),
        expected,
        "{source}"
      );
    }
    let v = eval(scope, "Symbol()");
    assert_eq!(
      message(Choice::convert(scope, v, prefix(), context, &())),
      "Failed to execute 'test': Argument 1 is a Symbol and can not be converted to a string."
    );
  }

  #[test]
  fn options_on_members_are_applied() {
    let mut runtime = JsRuntime::new(RuntimeOptions::default());
    let scope = &mut runtime.handle_scope();
    let v = eval(scope, "({level: 256, name: null})");
    let d = Clamped::convert(scope, v, prefix(), context, &()).unwrap();
    assert_eq!(d.level, 255);
  }

  fn int_opts(clamp: bool, enforce_range: bool) -> IntOptions {
    IntOptions {
      clamp,
      enforce_range,
    }
  }

  #[test]
  fn integers_default_wrap() {
    let mut runtime = JsRuntime::new(RuntimeOptions::default());
    let scope = &mut runtime.handle_scope();
    let d = IntOptions::default();
    let v = eval(scope, "256");
    assert_eq!(u8::convert(scope, v, prefix(), context, &d).unwrap(), 0);
    let v = eval(scope, "-1");
    assert_eq!(u8::convert(scope, v, prefix(), context, &d).unwrap(), 255);
    let v = eval(scope, "255.9");
    assert_eq!(u8::convert(scope, v, prefix(), context, &d).unwrap(), 255);
    let v = eval(scope, "128");
    assert_eq!(i8::convert(scope, v, prefix(), context, &d).unwrap(), -128);
    let v = eval(scope, "NaN");
    assert_eq!(u8::convert(scope, v, prefix(), context, &d).unwrap(), 0);
    let v = eval(scope, "-3");
    assert_eq!(
      u64::convert(scope, v, prefix(), context, &d).unwrap(),
      18446744073709551613
    );
    let v = eval(scope, "2 ** 64");
    assert_eq!(u64::convert(scope, v, prefix(), context, &d).unwrap(), 0);
    let v = eval(scope, "'  12  '");
    assert_eq!(u32::convert(scope, v, prefix(), context, &d).unwrap(), 12);
    let v = eval(scope, "-0");
    assert_eq!(i32::convert(scope, v, prefix(), context, &d).unwrap(), 0);
  }

  #[test]
  fn integers_clamp() {
    let mut runtime = JsRuntime::new(RuntimeOptions::default());
    let scope = &mut runtime.handle_scope();
    let o = int_opts(true, false);
    let cases = [
      ("300", 255),
      ("2.5", 2),
      ("3.5", 4),
      ("-0.5", 0),
      ("NaN", 0),
    ];
    for (src, expected) in cases {
      let v = eval(scope, src);
      assert_eq!(
        u8::convert(scope, v, prefix(), context, &o).unwrap(),
        expected,
        "{src}"
      );
    }
  }

  #[test]
  fn integers_enforce_range() {
    let mut runtime = JsRuntime::new(RuntimeOptions::default());
    let scope = &mut runtime.handle_scope();
    let o = int_opts(false, true);
    let v = eval(scope, "-0");
    assert_eq!(u8::convert(scope, v, prefix(), context, &o).unwrap(), 0);
    let v = eval(scope, "NaN");
    assert_eq!(
      message(u8::convert(scope, v, prefix(), context, &o)),
      "Failed to execute 'test': Argument 1 is not a finite number."
    );
    let v = eval(scope, "Infinity");
    assert!(u8::convert(scope, v, prefix(), context, &o).is_err());
    let v = eval(scope, "256");
    assert_eq!(
      message(u8::convert(scope, v, prefix(), context, &o)),
      "Failed to execute 'test': Argument 1 is outside the range [0, 255]."
    );
    let v = eval(scope, "2 ** 53");
    assert!(u64::convert(scope, v, prefix(), context, &o).is_err());
    let v = eval(scope, "-(2 ** 53 - 1)");
    assert_eq!(
      i64::convert(scope, v, prefix(), context, &o).unwrap(),
      -9007199254740991
    );
  }

  #[test]
  fn integers_reject_symbol_and_bigint() {
    let mut runtime = JsRuntime::new(RuntimeOptions::default());
    let scope = &mut runtime.handle_scope();
    let o = IntOptions::default();
    let v = eval(scope, "Symbol()");
    assert_eq!(
      message(u8::convert(scope, v, prefix(), context, &o)),
      "Failed to execute 'test': Argument 1 is a Symbol and can not be converted to a number."
    );
    let v = eval(scope, "10n");
    assert_eq!(
      message(u8::convert(scope, v, prefix(), context, &o)),
      "Failed to execute 'test': Argument 1 is a BigInt and can not be converted to a number."
    );
  }

  #[test]
  fn floats() {
    let mut runtime = JsRuntime::new(RuntimeOptions::default());
    let scope = &mut runtime.handle_scope();
    let v = eval(scope, "NaN");
    assert_eq!(
      message(f64::convert(scope, v, prefix(), context, &())),
      "Failed to execute 'test': Argument 1 is not a finite floating-point value."
    );
    let v = eval(scope, "Infinity");
    assert!(f64::convert(scope, v, prefix(), context, &()).is_err());
    let v = eval(scope, "1e40");
    assert!(f32::convert(scope, v, prefix(), context, &()).is_err());
    let v = eval(scope, "1.5");
    assert_eq!(f32::convert(scope, v, prefix(), context, &()).unwrap(), 1.5);
    let v = eval(scope, "1e40");
    assert_eq!(
      UnrestrictedFloat::convert(scope, v, prefix(), context, &())
        .unwrap()
        .0,
      f32::INFINITY
    );
    let v = eval(scope, "NaN");
    assert!(
      UnrestrictedDouble::convert(scope, v, prefix(), context, &())
        .unwrap()
        .is_nan()
    );
  }

  #[test]
  fn strings() {
    let mut runtime = JsRuntime::new(RuntimeOptions::default());
    let scope = &mut runtime.handle_scope();
    let d = StringOptions::default();
    let t = StringOptions {
      treat_null_as_empty_string: true,
    };
    let v = eval(scope, "null");
    assert_eq!(
      String::convert(scope, v, prefix(), context, &d).unwrap(),
      "null"
    );
    assert_eq!(
      String::convert(scope, v, prefix(), context, &t).unwrap(),
      ""
    );
    let v = eval(scope, "Symbol('x')");
    assert_eq!(
      message(String::convert(scope, v, prefix(), context, &d)),
      "Failed to execute 'test': Argument 1 is a Symbol and can not be converted to a string."
    );
    let v = eval(scope, "'\\u0100'");
    assert!(ByteString::convert(scope, v, prefix(), context, &d).is_err());
    let v = eval(scope, "'\\u00ff'");
    assert_eq!(
      ByteString::convert(scope, v, prefix(), context, &d)
        .unwrap()
        .0,
      "\u{ff}"
    );
    let v = eval(scope, "'\\ud800'");
    assert_eq!(
      String::convert(scope, v, prefix(), context, &d).unwrap(),
      "\u{fffd}"
    );
  }

  #[test]
  fn bigints() {
    let mut runtime = JsRuntime::new(RuntimeOptions::default());
    let scope = &mut runtime.handle_scope();
    let v = eval(scope, "0n");
    assert_eq!(
      BigInt::convert(scope, v, prefix(), context, &()).unwrap(),
      BigInt {
        sign: false,
        words: vec![]
      }
    );
    let v = eval(scope, "-1n");
    assert_eq!(
      BigInt::convert(scope, v, prefix(), context, &()).unwrap(),
      BigInt {
        sign: true,
        words: vec![1]
      }
    );
    let v = eval(scope, "2n ** 64n + 1n");
    assert_eq!(
      BigInt::convert(scope, v, prefix(), context, &()).unwrap(),
      BigInt {
        sign: false,
        words: vec![1, 1]
      }
    );
    let v = eval(scope, "-(2n ** 128n)");
    assert_eq!(
      BigInt::convert(scope, v, prefix(), context, &()).unwrap(),
      BigInt {
        sign: true,
        words: vec![0, 0, 1]
      }
    );
    let v = eval(scope, "true");
    assert_eq!(
      BigInt::convert(scope, v, prefix(), context, &()).unwrap(),
      BigInt {
        sign: false,
        words: vec![1]
      }
    );
    let v = eval(scope, "'0x10'");
    assert_eq!(
      BigInt::convert(scope, v, prefix(), context, &()).unwrap(),
      BigInt {
        sign: false,
        words: vec![16]
      }
    );
    let v = eval(scope, "'abc'");
    assert_eq!(
      message(BigInt::convert(scope, v, prefix(), context, &())),
      "Failed to execute 'test': Argument 1 is not a valid BigInt string."
    );
    let v = eval(scope, "5");
    assert_eq!(
      message(BigInt::convert(scope, v, prefix(), context, &())),
      "Failed to execute 'test': Argument 1 can not be converted to a BigInt."
    );
  }

  #[test]
  fn options_and_sequences() {
    let mut runtime = JsRuntime::new(RuntimeOptions::default());
    let scope = &mut runtime.handle_scope();
    let v = eval(scope, "null");
    assert_eq!(
      Option::<u8>::convert(
        scope,
        v,
        prefix(),
        context,
        &IntOptions::default()
      )
      .unwrap(),
      None
    );
    let v = eval(scope, "[1, undefined, 3]");
    assert_eq!(
      Vec::<Option<u8>>::convert(
        scope,
        v,
        prefix(),
        context,
        &IntOptions::default()
      )
      .unwrap(),
      vec![Some(1), None, Some(3)]
    );
    let v = eval(scope, "new Set([1, 2])");
    assert_eq!(
      Vec::<u8>::convert(scope, v, prefix(), context, &IntOptions::default())
        .unwrap(),
      vec![1, 2]
    );
    let v = eval(scope, "(function* () { yield 4; yield 5; })()");
    assert_eq!(
      Vec::<u8>::convert(scope, v, prefix(), context, &IntOptions::default())
        .unwrap(),
      vec![4, 5]
    );
    let v = eval(scope, "'abc'");
    assert_eq!(
      message(Vec::<u8>::convert(scope, v, prefix(), context, &IntOptions::default())),
      "Failed to execute 'test': Argument 1 can not be converted to a sequence because it is not an object."
    );
    let v = eval(scope, "({length: 2})");
    assert_eq!(
      message(Vec::<u8>::convert(scope, v, prefix(), context, &IntOptions::default())),
      "Failed to execute 'test': Argument 1 can not be converted to a sequence because it is not iterable."
    );
    let v = eval(scope, "[1, 'x']");
    let enforce = int_opts(false, true);
    assert_eq!(
      message(Vec::<u8>::convert(scope, v, prefix(), context, &enforce)),
      "Failed to execute 'test': Argument 1, sequence index 1 is not a finite number."
    );
  }

  #[test]
  fn records() {
    let mut runtime = JsRuntime::new(RuntimeOptions::default());
    let scope = &mut runtime.handle_scope();
    let o = IntOptions::default();
    let v = eval(
      scope,
      "Object.create({inherited: 1}, {own: {value: 2, enumerable: true}, hidden: {value: 3}})",
    );
    let map =
      HashMap::<String, u8>::convert(scope, v, prefix(), context, &o).unwrap();
    assert_eq!(map.len(), 1);
    assert_eq!(map["own"], 2);
    let v = eval(scope, "null");
    assert!(
      HashMap::<String, u8>::convert(scope, v, prefix(), context, &o).is_err()
    );
    let v = eval(scope, "({[Symbol()]: 1})");
    assert_eq!(
      message(HashMap::<String, u8>::convert(scope, v, prefix(), context, &o)),
      "Failed to execute 'test': Argument 1 can not be converted to a record because it has an enumerable Symbol key."
    );
    let v = eval(
      scope,
      "new Proxy({}, {ownKeys() { return ['b', 'a']; }, getOwnPropertyDescriptor() { return {value: 1, enumerable: true, configurable: true}; }})",
    );
    let map =
      HashMap::<String, u8>::convert(scope, v, prefix(), context, &o).unwrap();
    assert_eq!(map.len(), 2);
    // Record members are read in ownKeys order, which a Proxy can observe.
    let v = eval(
      scope,
      "(() => { globalThis.seen = []; return new Proxy({}, {ownKeys() { return ['b', 'a']; }, getOwnPropertyDescriptor() { return {value: 1, enumerable: true, configurable: true}; }, get(t, k) { seen.push(k); return 1; }}); })()",
    );
    HashMap::<String, u8>::convert(scope, v, prefix(), context, &o).unwrap();
    let seen = eval(scope, "seen.join(',')");
    assert_eq!(seen.to_rust_string_lossy(scope), "b,a");
    let v = eval(scope, "({a: 'x'})");
    assert_eq!(
      message(HashMap::<String, u8>::convert(scope, v, prefix(), context, &int_opts(false, true))),
      "Failed to execute 'test': Argument 1, record key 'a' is not a finite number."
    );
  }

  #[test]
  fn convert_op_arg_rethrows_js_exception() {
    let mut runtime = JsRuntime::new(RuntimeOptions::default());
    let scope = &mut runtime.handle_scope();
    // A getter that throws is rethrown unchanged, not replaced by a TypeError.
    let v = eval(scope, "({get x() { throw new RangeError('boom'); }})");
    let thrown =
      convert_op_arg::<HashMap<String, u8>>(scope, v, "op", 1).unwrap_err();
    let text = thrown.to_string(scope).unwrap().to_rust_string_lossy(scope);
    assert_eq!(text, "RangeError: boom");
    // A plain conversion failure becomes a TypeError naming the op and argument.
    let v = eval(scope, "'abc'");
    let error = convert_op_arg::<Vec<u8>>(scope, v, "op", 2).unwrap_err();
    let text = error.to_string(scope).unwrap().to_rust_string_lossy(scope);
    assert_eq!(
      text,
      "TypeError: op: Argument 2 can not be converted to a sequence because it is not an object."
    );
  }

  #[derive(crate::WebIDL)]
  #[webidl(dictionary)]
  #[allow(dead_code)]
  struct Pair {
    a: u8,
    b: u8,
  }

  #[derive(crate::WebIDL)]
  #[webidl(dictionary)]
  #[allow(dead_code)]
  struct Shadowing {
    context: u8,
    scope: u8,
    prefix: u8,
    value: u8,
  }

  #[test]
  fn dictionary_members_are_converted_before_the_next_is_read() {
    let mut runtime = JsRuntime::new(RuntimeOptions::default());
    let scope = &mut runtime.handle_scope();
    eval(
      scope,
      r#"
        globalThis.log = [];
        globalThis.o = {
          get a() { log.push("get a"); return { valueOf() { log.push("valueOf a"); return 1; } }; },
          get b() { log.push("get b"); return 2; },
        };
      "#,
    );
    let v = eval(scope, "o");
    let pair = Pair::convert(scope, v, prefix(), context, &()).unwrap();
    assert_eq!((pair.a, pair.b), (1, 2));
    let log = eval(scope, "log.join(',')");
    assert_eq!(log.to_rust_string_lossy(scope), "get a,valueOf a,get b");
    // A member that fails stops the conversion before the next member is read.
    eval(
      scope,
      r#"
        globalThis.log = [];
        globalThis.o = {
          a: Symbol(),
          get b() { log.push("get b"); throw new RangeError("b"); },
        };
      "#,
    );
    let v = eval(scope, "o");
    assert_eq!(
      message(Pair::convert(scope, v, prefix(), context, &())),
      "Failed to execute 'test': Argument 1, member 'a' is a Symbol and can not be converted to a number."
    );
    let log = eval(scope, "log.join(',')");
    assert_eq!(log.to_rust_string_lossy(scope), "");
  }

  #[test]
  fn dictionary_fields_named_like_generated_locals() {
    let mut runtime = JsRuntime::new(RuntimeOptions::default());
    let scope = &mut runtime.handle_scope();
    let v = eval(scope, "({context: 1, scope: 2, prefix: 3, value: 4})");
    let s = Shadowing::convert(scope, v, prefix(), context, &()).unwrap();
    assert_eq!((s.context, s.scope, s.prefix, s.value), (1, 2, 3, 4));
  }

  #[test]
  fn records_rethrow_descriptor_exceptions_and_skip_missing_keys() {
    let mut runtime = JsRuntime::new(RuntimeOptions::default());
    let scope = &mut runtime.handle_scope();
    let o = IntOptions::default();
    // A throwing descriptor trap rethrows its exception unchanged.
    let v = eval(
      scope,
      "new Proxy({}, {ownKeys() { return ['a']; }, getOwnPropertyDescriptor() { throw new RangeError('gopd'); }, get() { return 7; }})",
    );
    let thrown =
      convert_op_arg::<HashMap<String, u8>>(scope, v, "op", 1).unwrap_err();
    let text = thrown.to_string(scope).unwrap().to_rust_string_lossy(scope);
    assert_eq!(text, "RangeError: gopd");
    // An undefined descriptor skips the key, so the getter is never read.
    let v = eval(
      scope,
      "new Proxy({}, {ownKeys() { return ['a']; }, getOwnPropertyDescriptor() { return undefined; }, get() { return 7; }})",
    );
    let map =
      HashMap::<String, u8>::convert(scope, v, prefix(), context, &o).unwrap();
    assert!(map.is_empty());
    // A property deleted by an earlier getter is skipped, not read as 0.
    let v = eval(scope, "({get a() { delete this.b; return 1; }, b: 2})");
    let map =
      HashMap::<String, u8>::convert(scope, v, prefix(), context, &o).unwrap();
    assert_eq!(map.len(), 1);
    assert_eq!(map["a"], 1);
  }
}
