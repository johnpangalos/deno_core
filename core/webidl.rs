// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.

//! Conversion of JavaScript values into Rust values, following the WebIDL
//! [JavaScript type mapping](https://webidl.spec.whatwg.org/#js-type-mapping).
//!
//! Implement [`WebIdlConverter`] by hand, or derive it for dictionaries and
//! enums with [`#[derive(WebIDL)]`](crate::WebIDL). Use `#[webidl]` on an
//! [`op2`](crate::op2) argument to convert it with the default options before
//! the op body runs.

use crate::v8;
use std::borrow::Cow;
use std::collections::HashMap;
use std::fmt;

/// Converts a JavaScript value into a Rust value using WebIDL rules.
///
/// `prefix` names the API being called (for example `"Failed to execute
/// 'fetch'"`) and `context` describes the value being converted (for example
/// `"Argument 1"`). Errors are reported as `"{prefix}: {context} {reason}"`.
pub trait WebIdlConverter<'a>: Sized {
  /// Options that change how the value is converted, such as clamping for
  /// integers. Use `()` when there are none.
  type Options: Default;

  /// Converts `value`, returning a [`WebIdlError`] if it cannot be represented.
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

/// An error produced while converting a JavaScript value.
///
/// The message is `"{prefix}: {context} {reason}"`, which is also the message
/// of the `TypeError` thrown to JavaScript when an op argument fails.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebIdlError {
  prefix: Cow<'static, str>,
  context: Cow<'static, str>,
  reason: Cow<'static, str>,
}

impl WebIdlError {
  /// Creates a new error. The message is `"{prefix}: {context} {reason}"`.
  pub fn new(
    prefix: impl Into<Cow<'static, str>>,
    context: impl Into<Cow<'static, str>>,
    reason: impl Into<Cow<'static, str>>,
  ) -> Self {
    Self {
      prefix: prefix.into(),
      context: context.into(),
      reason: reason.into(),
    }
  }
}

impl fmt::Display for WebIdlError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "{}: {} {}", self.prefix, self.context, self.reason)
  }
}

impl std::error::Error for WebIdlError {}

/// Options for integer conversions.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct IntOptions {
  /// Clamp out-of-range values to the type's bounds instead of wrapping.
  pub clamp: bool,
  /// Throw on NaN, infinities and out-of-range values.
  pub enforce_range: bool,
}

/// Options for string conversions.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct StringOptions {
  /// Convert `null` to the empty string instead of `"null"`.
  pub treat_null_as_empty_string: bool,
}

/// An `unrestricted float`, which allows NaN and infinities.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct UnrestrictedFloat(pub f32);

impl std::ops::Deref for UnrestrictedFloat {
  type Target = f32;

  fn deref(&self) -> &f32 {
    &self.0
  }
}

/// An `unrestricted double`, which allows NaN and infinities.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct UnrestrictedDouble(pub f64);

impl std::ops::Deref for UnrestrictedDouble {
  type Target = f64;

  fn deref(&self) -> &f64 {
    &self.0
  }
}

/// A `ByteString`: a string whose code units are all at most U+00FF.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ByteString(pub String);

impl std::ops::Deref for ByteString {
  type Target = String;

  fn deref(&self) -> &String {
    &self.0
  }
}

/// A `bigint`, stored as a sign and little-endian 64-bit magnitude words.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct BigInt {
  /// True if the value is negative.
  pub sign: bool,
  /// The magnitude, least significant word first.
  pub words: Vec<u64>,
}

/// Runs `f` inside a `TryCatch`, so a JavaScript exception thrown during the
/// conversion becomes a [`WebIdlError`] and never stays pending on the isolate.
fn catch_js<'a, T, C>(
  scope: &mut v8::HandleScope<'a>,
  prefix: &str,
  context: &C,
  f: impl FnOnce(&mut v8::HandleScope<'a>) -> Option<T>,
) -> Result<T, WebIdlError>
where
  C: Fn() -> Cow<'static, str>,
{
  let tc = &mut v8::TryCatch::new(scope);
  match f(tc) {
    Some(value) => Ok(value),
    None => {
      let exception = tc
        .exception()
        .map(|e| e.to_rust_string_lossy(tc))
        .unwrap_or_default();
      Err(WebIdlError::new(
        prefix.to_owned(),
        context(),
        format!("threw an exception: {exception}"),
      ))
    }
  }
}

fn str_key<'a>(
  scope: &mut v8::HandleScope<'a>,
  name: &str,
) -> Option<v8::Local<'a, v8::Value>> {
  v8::String::new(scope, name).map(|s| s.into())
}

fn to_object<'a, C>(
  value: v8::Local<'a, v8::Value>,
  prefix: &str,
  context: &C,
) -> Result<v8::Local<'a, v8::Object>, WebIdlError>
where
  C: Fn() -> Cow<'static, str>,
{
  v8::Local::<v8::Object>::try_from(value).map_err(|_| {
    WebIdlError::new(prefix.to_owned(), context(), "is not an object")
  })
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
  catch_js(scope, prefix, context, |scope| {
    value.to_number(scope).map(|n| n.value())
  })
}

/// The ConvertToInt algorithm. Returns the integer as its two's complement
/// 64-bit pattern, which the caller truncates to the target type.
fn convert_to_int(
  x: f64,
  lower: f64,
  upper: f64,
  options: &IntOptions,
) -> Result<u64, String> {
  if options.enforce_range {
    if !x.is_finite() {
      return Err("is not a finite number".into());
    }
    let x = x.trunc();
    if x < lower || x > upper {
      return Err(format!("is outside the range [{lower}, {upper}]"));
    }
    Ok(x as i64 as u64)
  } else if options.clamp {
    if x.is_nan() {
      return Ok(0);
    }
    Ok(round_half_to_even(x.clamp(lower, upper)) as i64 as u64)
  } else {
    if !x.is_finite() {
      return Ok(0);
    }
    // fmod is exact, so this keeps the value modulo 2^64 without rounding.
    let m = x.trunc() % 18446744073709551616.0;
    Ok((m as i128).rem_euclid(1i128 << 64) as u64)
  }
}

fn round_half_to_even(x: f64) -> f64 {
  let floor = x.floor();
  let diff = x - floor;
  if diff < 0.5 {
    floor
  } else if diff > 0.5 {
    floor + 1.0
  } else if floor % 2.0 == 0.0 {
    floor
  } else {
    floor + 1.0
  }
}

macro_rules! impl_int {
  ($ty:ty, $lower:expr, $upper:expr) => {
    impl<'a> WebIdlConverter<'a> for $ty {
      type Options = IntOptions;

      fn convert<C>(
        scope: &mut v8::HandleScope<'a>,
        value: v8::Local<'a, v8::Value>,
        prefix: Cow<'static, str>,
        context: C,
        options: &IntOptions,
      ) -> Result<Self, WebIdlError>
      where
        C: Fn() -> Cow<'static, str>,
      {
        let x = to_number(scope, value, &prefix, &context)?;
        match convert_to_int(x, $lower, $upper, options) {
          Ok(bits) => Ok(bits as $ty),
          Err(reason) => Err(WebIdlError::new(prefix, context(), reason)),
        }
      }
    }
  };
}

impl_int!(i8, -128.0, 127.0);
impl_int!(u8, 0.0, 255.0);
impl_int!(i16, -32768.0, 32767.0);
impl_int!(u16, 0.0, 65535.0);
impl_int!(i32, -2147483648.0, 2147483647.0);
impl_int!(u32, 0.0, 4294967295.0);
// 64-bit clamping and enforcement use the largest integer that is exact in a
// double.
impl_int!(i64, -9007199254740991.0, 9007199254740991.0);
impl_int!(u64, 0.0, 9007199254740991.0);

impl<'a> WebIdlConverter<'a> for f64 {
  type Options = ();

  fn convert<C>(
    scope: &mut v8::HandleScope<'a>,
    value: v8::Local<'a, v8::Value>,
    prefix: Cow<'static, str>,
    context: C,
    _options: &(),
  ) -> Result<Self, WebIdlError>
  where
    C: Fn() -> Cow<'static, str>,
  {
    let x = to_number(scope, value, &prefix, &context)?;
    if !x.is_finite() {
      return Err(WebIdlError::new(
        prefix,
        context(),
        "is not a finite number",
      ));
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
    _options: &(),
  ) -> Result<Self, WebIdlError>
  where
    C: Fn() -> Cow<'static, str>,
  {
    let x = to_number(scope, value, &prefix, &context)?;
    if !x.is_finite() {
      return Err(WebIdlError::new(
        prefix,
        context(),
        "is not a finite number",
      ));
    }
    let y = x as f32;
    if !y.is_finite() {
      return Err(WebIdlError::new(
        prefix,
        context(),
        "is outside the range of a 32-bit float",
      ));
    }
    Ok(y)
  }
}

impl<'a> WebIdlConverter<'a> for UnrestrictedFloat {
  type Options = ();

  fn convert<C>(
    scope: &mut v8::HandleScope<'a>,
    value: v8::Local<'a, v8::Value>,
    prefix: Cow<'static, str>,
    context: C,
    _options: &(),
  ) -> Result<Self, WebIdlError>
  where
    C: Fn() -> Cow<'static, str>,
  {
    let x = to_number(scope, value, &prefix, &context)?;
    Ok(UnrestrictedFloat(x as f32))
  }
}

impl<'a> WebIdlConverter<'a> for UnrestrictedDouble {
  type Options = ();

  fn convert<C>(
    scope: &mut v8::HandleScope<'a>,
    value: v8::Local<'a, v8::Value>,
    prefix: Cow<'static, str>,
    context: C,
    _options: &(),
  ) -> Result<Self, WebIdlError>
  where
    C: Fn() -> Cow<'static, str>,
  {
    let x = to_number(scope, value, &prefix, &context)?;
    Ok(UnrestrictedDouble(x))
  }
}

impl<'a> WebIdlConverter<'a> for bool {
  type Options = ();

  fn convert<C>(
    scope: &mut v8::HandleScope<'a>,
    value: v8::Local<'a, v8::Value>,
    _prefix: Cow<'static, str>,
    _context: C,
    _options: &(),
  ) -> Result<Self, WebIdlError>
  where
    C: Fn() -> Cow<'static, str>,
  {
    Ok(value.boolean_value(scope))
  }
}

impl<'a> WebIdlConverter<'a> for v8::Local<'a, v8::Value> {
  type Options = ();

  fn convert<C>(
    _scope: &mut v8::HandleScope<'a>,
    value: v8::Local<'a, v8::Value>,
    _prefix: Cow<'static, str>,
    _context: C,
    _options: &(),
  ) -> Result<Self, WebIdlError>
  where
    C: Fn() -> Cow<'static, str>,
  {
    Ok(value)
  }
}

/// The DOMString conversion, shared by `String` and `ByteString`.
fn dom_string<'a, C>(
  scope: &mut v8::HandleScope<'a>,
  value: v8::Local<'a, v8::Value>,
  prefix: &str,
  context: &C,
  options: &StringOptions,
) -> Result<String, WebIdlError>
where
  C: Fn() -> Cow<'static, str>,
{
  if value.is_symbol() {
    return Err(WebIdlError::new(
      prefix.to_owned(),
      context(),
      "is a Symbol and cannot be converted to a string",
    ));
  }
  if options.treat_null_as_empty_string && value.is_null() {
    return Ok(String::new());
  }
  catch_js(scope, prefix, context, |scope| {
    let string = value.to_string(scope)?;
    Some(string.to_rust_string_lossy(scope))
  })
}

/// A `DOMString`. Rust strings cannot hold lone UTF-16 surrogates, so each one
/// is replaced with U+FFFD.
impl<'a> WebIdlConverter<'a> for String {
  type Options = StringOptions;

  fn convert<C>(
    scope: &mut v8::HandleScope<'a>,
    value: v8::Local<'a, v8::Value>,
    prefix: Cow<'static, str>,
    context: C,
    options: &StringOptions,
  ) -> Result<Self, WebIdlError>
  where
    C: Fn() -> Cow<'static, str>,
  {
    dom_string(scope, value, &prefix, &context, options)
  }
}

impl<'a> WebIdlConverter<'a> for ByteString {
  type Options = StringOptions;

  fn convert<C>(
    scope: &mut v8::HandleScope<'a>,
    value: v8::Local<'a, v8::Value>,
    prefix: Cow<'static, str>,
    context: C,
    options: &StringOptions,
  ) -> Result<Self, WebIdlError>
  where
    C: Fn() -> Cow<'static, str>,
  {
    let string = dom_string(scope, value, &prefix, &context, options)?;
    if string.chars().any(|c| c as u32 > 0xFF) {
      return Err(WebIdlError::new(
        prefix,
        context(),
        "contains a code point greater than U+00FF",
      ));
    }
    Ok(ByteString(string))
  }
}

impl<'a> WebIdlConverter<'a> for BigInt {
  type Options = ();

  fn convert<C>(
    scope: &mut v8::HandleScope<'a>,
    value: v8::Local<'a, v8::Value>,
    prefix: Cow<'static, str>,
    context: C,
    _options: &(),
  ) -> Result<Self, WebIdlError>
  where
    C: Fn() -> Cow<'static, str>,
  {
    let big =
      catch_js(scope, &prefix, &context, |scope| value.to_big_int(scope))?;
    let mut words = vec![0u64; big.word_count()];
    let (sign, _) = big.to_words_array(&mut words);
    Ok(BigInt { sign, words })
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
    let object = to_object(value, &prefix, &context)?;
    let not_iterable =
      || WebIdlError::new(prefix.clone(), context(), "is not iterable");
    let method = catch_js(scope, &prefix, &context, |scope| {
      let key = v8::Symbol::get_iterator(scope);
      object.get(scope, key.into())
    })?;
    let method = v8::Local::<v8::Function>::try_from(method)
      .map_err(|_| not_iterable())?;
    let iterator = catch_js(scope, &prefix, &context, |scope| {
      method.call(scope, value, &[])
    })?;
    let iterator = v8::Local::<v8::Object>::try_from(iterator)
      .map_err(|_| not_iterable())?;
    let next = catch_js(scope, &prefix, &context, |scope| {
      let key = str_key(scope, "next")?;
      iterator.get(scope, key)
    })?;
    let next =
      v8::Local::<v8::Function>::try_from(next).map_err(|_| not_iterable())?;

    let mut out = Vec::new();
    loop {
      let result = catch_js(scope, &prefix, &context, |scope| {
        next.call(scope, iterator.into(), &[])
      })?;
      let result = to_object(result, &prefix, &context)?;
      let done = catch_js(scope, &prefix, &context, |scope| {
        let key = str_key(scope, "done")?;
        result.get(scope, key)
      })?;
      if done.boolean_value(scope) {
        break;
      }
      let item = catch_js(scope, &prefix, &context, |scope| {
        let key = str_key(scope, "value")?;
        result.get(scope, key)
      })?;
      let index = out.len();
      let element_context =
        || Cow::Owned(format!("{}, index {}", context(), index));
      match T::convert(scope, item, prefix.clone(), element_context, options) {
        Ok(element) => out.push(element),
        Err(err) => {
          // Close the iterator, but the original conversion error wins.
          let _ = catch_js(scope, &prefix, &context, |scope| {
            let key = str_key(scope, "return")?;
            let ret =
              v8::Local::<v8::Function>::try_from(iterator.get(scope, key)?)
                .ok()?;
            ret.call(scope, iterator.into(), &[])
          });
          return Err(err);
        }
      }
    }
    Ok(out)
  }
}

/// A `record<DOMString, T>`. Keys are converted like `String`, so distinct keys
/// that differ only in lone surrogates map to the same entry, and the later
/// one wins.
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
    let object = to_object(value, &prefix, &context)?;
    // The keys are collected up front, but each one is checked again when it
    // is reached, since an earlier getter may delete or hide a later property.
    let keys = catch_js(scope, &prefix, &context, |scope| {
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
    for i in 0..keys.length() {
      let key =
        catch_js(scope, &prefix, &context, |scope| keys.get_index(scope, i))?;
      let enumerable = catch_js(scope, &prefix, &context, |scope| {
        is_own_enumerable(scope, object, key)
      })?;
      if !enumerable {
        continue;
      }
      if key.is_symbol() {
        return Err(WebIdlError::new(
          prefix.clone(),
          context(),
          "has a Symbol key, which is not allowed in a record",
        ));
      }
      let name = key.to_rust_string_lossy(scope);
      let item =
        catch_js(scope, &prefix, &context, |scope| object.get(scope, key))?;
      let key_context = || Cow::Owned(format!("{}, key '{}'", context(), name));
      let converted =
        T::convert(scope, item, prefix.clone(), key_context, options)?;
      map.insert(name, converted);
    }
    Ok(map)
  }
}

/// Whether `object` has an own property `key` that is enumerable, which is the
/// check the record conversion makes for each key (`O.[[GetOwnProperty]]`).
fn is_own_enumerable<'a>(
  scope: &mut v8::HandleScope<'a>,
  object: v8::Local<'a, v8::Object>,
  key: v8::Local<'a, v8::Value>,
) -> Option<bool> {
  let name = v8::Local::<v8::Name>::try_from(key).ok()?;
  let desc = object.get_own_property_descriptor(scope, name)?;
  if desc.is_undefined() {
    return Some(false);
  }
  let desc = v8::Local::<v8::Object>::try_from(desc).ok()?;
  let enumerable = str_key(scope, "enumerable")?;
  Some(desc.get(scope, enumerable)?.boolean_value(scope))
}

/// Reads a dictionary member for the code generated by `#[derive(WebIDL)]`.
///
/// `undefined` and `null` read as `undefined` for every member.
#[doc(hidden)]
pub fn get_dictionary_member<'a, C>(
  scope: &mut v8::HandleScope<'a>,
  value: v8::Local<'a, v8::Value>,
  key: &str,
  prefix: &str,
  context: &C,
) -> Result<v8::Local<'a, v8::Value>, WebIdlError>
where
  C: Fn() -> Cow<'static, str>,
{
  if value.is_null_or_undefined() {
    return Ok(v8::undefined(scope).into());
  }
  let object = to_object(value, prefix, context)?;
  catch_js(scope, prefix, context, |scope| {
    let key = str_key(scope, key)?;
    object.get(scope, key)
  })
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::JsRuntime;
  use crate::RuntimeOptions;
  use pretty_assertions::assert_eq;

  /// Evaluates `code` in a fresh runtime and runs `f` on the result.
  fn with_value<T>(
    code: &str,
    f: impl for<'a> FnOnce(&mut v8::HandleScope<'a>, v8::Local<'a, v8::Value>) -> T,
  ) -> T {
    let mut runtime = JsRuntime::new(RuntimeOptions::default());
    let global = runtime.execute_script("", code.to_string()).unwrap();
    let scope = &mut runtime.handle_scope();
    let value = v8::Local::new(scope, global);
    f(scope, value)
  }

  /// Converts the result of `code` with the prefix "P" and context "C". The
  /// error is returned as its message so tests can compare it exactly.
  fn convert<T, O>(code: &str, options: &O) -> Result<T, String>
  where
    T: for<'a> WebIdlConverter<'a, Options = O>,
  {
    with_value(code, |scope, value| {
      T::convert(
        scope,
        value,
        Cow::Borrowed("P"),
        || Cow::Borrowed("C"),
        options,
      )
      .map_err(|e| e.to_string())
    })
  }

  fn int<T>(code: &str) -> Result<T, String>
  where
    T: for<'a> WebIdlConverter<'a, Options = IntOptions>,
  {
    convert(code, &IntOptions::default())
  }

  fn clamp<T>(code: &str) -> Result<T, String>
  where
    T: for<'a> WebIdlConverter<'a, Options = IntOptions>,
  {
    convert(
      code,
      &IntOptions {
        clamp: true,
        enforce_range: false,
      },
    )
  }

  fn enforce<T>(code: &str) -> Result<T, String>
  where
    T: for<'a> WebIdlConverter<'a, Options = IntOptions>,
  {
    convert(
      code,
      &IntOptions {
        clamp: false,
        enforce_range: true,
      },
    )
  }

  #[test]
  fn error_message_format() {
    let err = WebIdlError::new(
      "Failed to execute 'fetch'",
      "Argument 1",
      "is not a finite number",
    );
    assert_eq!(
      err.to_string(),
      "Failed to execute 'fetch': Argument 1 is not a finite number"
    );
    let _: &dyn std::error::Error = &err;
  }

  #[test]
  fn int_default_mode_wraps() {
    assert_eq!(int::<u8>("256"), Ok(0));
    assert_eq!(int::<u8>("257"), Ok(1));
    assert_eq!(int::<u8>("-1"), Ok(255));
    assert_eq!(int::<u8>("3.9"), Ok(3));
    assert_eq!(int::<u8>("-3.9"), Ok(253));
    assert_eq!(int::<u8>("Infinity"), Ok(0));
    assert_eq!(int::<u8>("NaN"), Ok(0));
    assert_eq!(int::<u8>("'12'"), Ok(12));
    assert_eq!(int::<u8>("'abc'"), Ok(0));
    assert_eq!(int::<u8>("null"), Ok(0));
    assert_eq!(int::<u8>("undefined"), Ok(0));
    assert_eq!(int::<u8>("true"), Ok(1));
    assert_eq!(int::<i8>("128"), Ok(-128));
    assert_eq!(int::<i32>("2 ** 31"), Ok(i32::MIN));
    assert_eq!(int::<u64>("-1"), Ok(u64::MAX));
    assert_eq!(int::<u64>("2 ** 64"), Ok(0));
    assert_eq!(int::<i64>("2 ** 63"), Ok(i64::MIN));
    assert_eq!(int::<i64>("1e300"), Ok(0));
  }

  #[test]
  fn int_clamp_mode() {
    assert_eq!(clamp::<u8>("300"), Ok(255));
    assert_eq!(clamp::<u8>("-5"), Ok(0));
    assert_eq!(clamp::<u8>("2.5"), Ok(2));
    assert_eq!(clamp::<u8>("3.5"), Ok(4));
    assert_eq!(clamp::<u8>("NaN"), Ok(0));
    assert_eq!(clamp::<u8>("-0"), Ok(0));
    assert_eq!(clamp::<i64>("1e300"), Ok(9007199254740991));
    assert_eq!(clamp::<i64>("-1e300"), Ok(-9007199254740991));
    assert_eq!(clamp::<u64>("-1e300"), Ok(0));
  }

  #[test]
  fn int_enforce_range_mode() {
    assert_eq!(enforce::<u8>("255.9"), Ok(255));
    assert_eq!(
      enforce::<u8>("256"),
      Err("P: C is outside the range [0, 255]".to_string())
    );
    assert_eq!(
      enforce::<u8>("-1"),
      Err("P: C is outside the range [0, 255]".to_string())
    );
    assert_eq!(enforce::<u8>("-0.9"), Ok(0));
    assert_eq!(
      enforce::<u8>("NaN"),
      Err("P: C is not a finite number".to_string())
    );
    assert_eq!(
      enforce::<u8>("Infinity"),
      Err("P: C is not a finite number".to_string())
    );
    assert_eq!(enforce::<u64>("2 ** 53 - 1"), Ok(9007199254740991));
    assert!(enforce::<u64>("2 ** 53").is_err());
    assert!(enforce::<i64>("-(2 ** 53)").is_err());
    assert_eq!(enforce::<i8>("-128"), Ok(-128));
  }

  #[test]
  fn int_bigint_and_symbol_input_error() {
    assert_eq!(
      int::<u8>("1n"),
      Err(
        "P: C threw an exception: TypeError: Cannot convert a BigInt value to a number"
          .to_string()
      )
    );
    assert!(int::<i8>("1n").is_err());
    assert!(int::<u16>("1n").is_err());
    assert!(int::<i16>("1n").is_err());
    assert!(int::<u32>("1n").is_err());
    assert!(int::<i32>("1n").is_err());
    assert!(int::<u64>("1n").is_err());
    assert!(int::<i64>("1n").is_err());
    assert!(int::<u8>("Symbol()").is_err());
    assert!(clamp::<u8>("({ valueOf() { throw new Error('boom') } })").is_err());
  }

  #[test]
  fn float_conversions() {
    assert_eq!(convert::<f64, _>("1.5", &()), Ok(1.5));
    assert_eq!(
      convert::<f64, _>("NaN", &()),
      Err("P: C is not a finite number".to_string())
    );
    assert_eq!(
      convert::<f64, _>("-Infinity", &()),
      Err("P: C is not a finite number".to_string())
    );
    assert_eq!(
      convert::<f32, _>("1e39", &()),
      Err("P: C is outside the range of a 32-bit float".to_string())
    );
    assert_eq!(convert::<f32, _>("1.5", &()), Ok(1.5));
    let neg_zero = convert::<f32, _>("-0", &()).unwrap();
    assert_eq!(neg_zero.to_bits(), (-0.0f32).to_bits());
    assert!(convert::<f32, _>("NaN", &()).is_err());
    assert!(convert::<f64, _>("1n", &()).is_err());
    assert!(convert::<UnrestrictedFloat, _>("1n", &()).is_err());
    assert!(convert::<UnrestrictedDouble, _>("1n", &()).is_err());
    let nan = convert::<UnrestrictedFloat, _>("NaN", &()).unwrap();
    assert!(nan.is_nan());
    assert_eq!(
      *convert::<UnrestrictedFloat, _>("1e39", &()).unwrap(),
      f32::INFINITY
    );
    assert_eq!(
      *convert::<UnrestrictedDouble, _>("-Infinity", &()).unwrap(),
      f64::NEG_INFINITY
    );
  }

  #[test]
  fn bool_and_passthrough() {
    assert_eq!(convert::<bool, _>("0", &()), Ok(false));
    assert_eq!(convert::<bool, _>("'x'", &()), Ok(true));
    assert_eq!(convert::<bool, _>("undefined", &()), Ok(false));
    let same = with_value("({a: 1})", |scope, value| {
      let converted = <v8::Local<v8::Value> as WebIdlConverter>::convert(
        scope,
        value,
        Cow::Borrowed("P"),
        || Cow::Borrowed("C"),
        &(),
      )
      .unwrap();
      converted == value
    });
    assert!(same);
  }

  #[test]
  fn string_conversions() {
    let opts = StringOptions::default();
    assert_eq!(convert::<String, _>("1.5", &opts), Ok("1.5".to_string()));
    assert_eq!(convert::<String, _>("null", &opts), Ok("null".to_string()));
    assert_eq!(
      convert::<String, _>("undefined", &opts),
      Ok("undefined".to_string())
    );
    assert_eq!(
      convert::<String, _>("({ toString() { return 'custom' } })", &opts),
      Ok("custom".to_string())
    );
    assert_eq!(
      convert::<String, _>("Symbol('s')", &opts),
      Err("P: C is a Symbol and cannot be converted to a string".to_string())
    );
    assert_eq!(
      convert::<String, _>(
        "({ toString() { throw new Error('boom') } })",
        &opts
      ),
      Err("P: C threw an exception: Error: boom".to_string())
    );
    let treat = StringOptions {
      treat_null_as_empty_string: true,
    };
    assert_eq!(convert::<String, _>("null", &treat), Ok(String::new()));
    assert_eq!(
      convert::<String, _>("undefined", &treat),
      Ok("undefined".to_string())
    );
  }

  #[test]
  fn byte_string_conversions() {
    let opts = StringOptions::default();
    assert_eq!(
      convert::<ByteString, _>("'\u{e9}'", &opts),
      Ok(ByteString("\u{e9}".to_string()))
    );
    let err = Err("P: C contains a code point greater than U+00FF".to_string());
    assert_eq!(convert::<ByteString, _>("'\u{20ac}'", &opts), err);
    assert_eq!(convert::<ByteString, _>("'\u{100}'", &opts), err);
    assert!(convert::<ByteString, _>("Symbol()", &opts).is_err());
  }

  #[test]
  fn bigint_conversions() {
    let big = |sign, words: Vec<u64>| Ok(BigInt { sign, words });
    assert_eq!(convert::<BigInt, _>("1n", &()), big(false, vec![1]));
    assert_eq!(
      convert::<BigInt, _>("2n ** 64n", &()),
      big(false, vec![0, 1])
    );
    assert_eq!(convert::<BigInt, _>("-5n", &()), big(true, vec![5]));
    let zero = convert::<BigInt, _>("0n", &()).unwrap();
    assert!(!zero.sign);
    assert!(zero.words.iter().all(|w| *w == 0));
    assert_eq!(convert::<BigInt, _>("true", &()), big(false, vec![1]));
    assert_eq!(convert::<BigInt, _>("false", &()), big(false, vec![]));
    assert_eq!(convert::<BigInt, _>("'123'", &()), big(false, vec![123]));
    assert_eq!(convert::<BigInt, _>("'0x10'", &()), big(false, vec![16]));
    for code in ["'abc'", "1", "undefined", "null", "Symbol()"] {
      assert!(convert::<BigInt, _>(code, &()).is_err(), "{code}");
    }
  }

  #[test]
  fn option_conversions() {
    let opts = IntOptions::default();
    assert_eq!(convert::<Option<u8>, _>("null", &opts), Ok(None));
    assert_eq!(convert::<Option<u8>, _>("undefined", &opts), Ok(None));
    assert_eq!(convert::<Option<u8>, _>("5", &opts), Ok(Some(5)));
  }

  #[test]
  fn vec_conversions() {
    let opts = IntOptions::default();
    assert_eq!(convert::<Vec<u8>, _>("[1, 2, 3]", &opts), Ok(vec![1, 2, 3]));
    assert_eq!(
      convert::<Vec<u8>, _>("new Set([4, 5])", &opts),
      Ok(vec![4, 5])
    );
    assert_eq!(
      convert::<Vec<u8>, _>("(function* () { yield 7; })()", &opts),
      Ok(vec![7])
    );
    assert_eq!(convert::<Vec<u8>, _>("[,1]", &opts), Ok(vec![0, 1]));
    assert_eq!(convert::<Vec<u8>, _>("[]", &opts), Ok(vec![]));
    assert_eq!(
      convert::<Vec<u8>, _>("'123'", &opts),
      Err("P: C is not an object".to_string())
    );
    assert_eq!(
      convert::<Vec<u8>, _>("null", &opts),
      Err("P: C is not an object".to_string())
    );
    assert_eq!(
      convert::<Vec<u8>, _>("undefined", &opts),
      Err("P: C is not an object".to_string())
    );
    assert_eq!(
      convert::<Vec<u8>, _>("({})", &opts),
      Err("P: C is not iterable".to_string())
    );
    assert_eq!(
      convert::<Vec<u8>, _>("({length: 1, 0: 1})", &opts),
      Err("P: C is not iterable".to_string())
    );
    assert_eq!(
      convert::<Vec<u8>, _>("[1, 2, Symbol()]", &opts),
      Err(
        "P: C, index 2 threw an exception: TypeError: Cannot convert a Symbol value to a number"
          .to_string()
      )
    );
    assert_eq!(
      convert::<Vec<u8>, _>(
        "({ [Symbol.iterator]() { throw new Error('boom') } })",
        &opts
      ),
      Err("P: C threw an exception: Error: boom".to_string())
    );
    assert_eq!(
      convert::<Vec<u8>, _>(
        "({ [Symbol.iterator]() { return { next() { throw new Error('next') } } } })",
        &opts
      ),
      Err("P: C threw an exception: Error: next".to_string())
    );
  }

  #[test]
  fn hashmap_conversions() {
    let opts = IntOptions::default();
    let map = |pairs: &[(&str, u8)]| {
      Ok(
        pairs
          .iter()
          .map(|(k, v)| (k.to_string(), *v))
          .collect::<HashMap<String, u8>>(),
      )
    };
    assert_eq!(
      convert::<HashMap<String, u8>, _>("({a: 1, b: 300})", &opts),
      map(&[("a", 1), ("b", 44)])
    );
    assert_eq!(
      convert::<HashMap<String, u8>, _>("[5]", &opts),
      map(&[("0", 5)])
    );
    assert_eq!(
      convert::<HashMap<String, u8>, _>(
        "Object.defineProperty({}, 'x', { value: 1, enumerable: false })",
        &opts
      ),
      map(&[])
    );
    assert_eq!(
      convert::<HashMap<String, u8>, _>(
        "Object.create({ inherited: 1 })",
        &opts
      ),
      map(&[])
    );
    assert_eq!(
      convert::<HashMap<String, u8>, _>("({[Symbol('s')]: 1})", &opts),
      Err(
        "P: C has a Symbol key, which is not allowed in a record".to_string()
      )
    );
    for code in ["'x'", "null", "undefined", "1"] {
      assert_eq!(
        convert::<HashMap<String, u8>, _>(code, &opts),
        Err("P: C is not an object".to_string()),
        "{code}"
      );
    }
    assert_eq!(
      convert::<HashMap<String, u8>, _>("({a: Symbol()})", &opts),
      Err(
        "P: C, key 'a' threw an exception: TypeError: Cannot convert a Symbol value to a number"
          .to_string()
      )
    );
  }

  #[test]
  fn hashmap_checks_each_key_when_reached() {
    let opts = IntOptions::default();
    // A getter that deletes a later key means that key is absent.
    assert_eq!(
      convert::<HashMap<String, u8>, _>(
        "({get a() { delete this.b; return 1 }, b: 2})",
        &opts
      ),
      Ok(HashMap::from([("a".to_string(), 1)]))
    );
    // A getter that makes a later key non-enumerable also removes it.
    assert_eq!(
      convert::<HashMap<String, u8>, _>(
        "({get a() { Object.defineProperty(this, 'b', { enumerable: false }); return 1 }, b: 2})",
        &opts
      ),
      Ok(HashMap::from([("a".to_string(), 1)]))
    );
    // Non-enumerable symbol keys are skipped rather than rejected.
    assert_eq!(
      convert::<HashMap<String, u8>, _>(
        "Object.defineProperty({}, Symbol('s'), { value: 1, enumerable: false })",
        &opts
      ),
      Ok(HashMap::new())
    );
  }

  use crate::WebIDL;

  #[derive(Debug, PartialEq, WebIDL)]
  #[webidl(dictionary)]
  #[allow(dead_code)]
  struct Utf16Order {
    #[webidl(rename = "\u{FF61}")]
    halfwidth: u8,
    #[webidl(rename = "\u{1F600}")]
    emoji: u8,
  }

  #[test]
  fn dictionary_members_sorted_by_utf16_code_units() {
    // The emoji key starts with 0xD83D in UTF-16, which sorts before 0xFF61,
    // so it is read first. Both getters return a Symbol, which fails on the
    // first member read.
    assert_eq!(
      convert::<Utf16Order, _>(
        "({ get '\u{FF61}'() { return Symbol() }, get '\u{1F600}'() { return Symbol() } })",
        &()
      ),
      Err(
        "P: C, member '\u{1F600}' of 'Utf16Order' threw an exception: TypeError: Cannot convert a Symbol value to a number"
          .to_string()
      )
    );
  }
}
