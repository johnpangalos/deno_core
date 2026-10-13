// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.

//! Conversions from JavaScript values to Rust types, following the WebIDL
//! type mapping at <https://webidl.spec.whatwg.org/#js-type-mapping>.
//!
//! Each converter implements [`WebIdlConverter`]. The `prefix` is the name of
//! the API being called (e.g. `Failed to execute 'fetch'`) and `context`
//! describes the value being converted (e.g. `Argument 1`). Both are only
//! used to build a [`WebIdlError`], so `context` is only invoked on error.

use std::borrow::Cow;
use std::collections::HashMap;
use std::fmt;
use std::ops::Deref;

use crate::v8;

/// Converts a V8 value into a Rust type using WebIDL semantics.
pub trait WebIdlConverter<'a>: Sized {
  /// Options that tweak the conversion, e.g. [`IntOptions`].
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

/// An error produced while converting a value.
///
/// Displays as `"{prefix}: {context} {problem}"`, e.g.
/// `Failed to execute 'fetch': Argument 1 is not a finite number`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebIdlError {
  prefix: Cow<'static, str>,
  context: Cow<'static, str>,
  problem: Cow<'static, str>,
  /// True when the failure was caused by a JavaScript exception that is still
  /// pending in the scope. In that case the original exception should be
  /// propagated instead of a new error.
  thrown: bool,
}

impl WebIdlError {
  /// Creates an error. `context` is only invoked here, so callers can pass a
  /// closure that is expensive to evaluate.
  pub fn new<C>(
    prefix: Cow<'static, str>,
    context: &C,
    problem: impl Into<Cow<'static, str>>,
  ) -> Self
  where
    C: Fn() -> Cow<'static, str>,
  {
    Self {
      prefix,
      context: context(),
      problem: problem.into(),
      thrown: false,
    }
  }

  /// Returns true if a JavaScript exception was thrown during conversion and
  /// is still pending. Callers should return from the op without throwing a
  /// new error so the original exception is observed by JavaScript.
  pub fn is_js_exception(&self) -> bool {
    self.thrown
  }

  fn js_exception<C>(prefix: Cow<'static, str>, context: &C) -> Self
  where
    C: Fn() -> Cow<'static, str>,
  {
    Self {
      thrown: true,
      ..Self::new(prefix, context, "threw an exception")
    }
  }
}

impl fmt::Display for WebIdlError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "{}: {} {}", self.prefix, self.context, self.problem)
  }
}

impl std::error::Error for WebIdlError {}

/// Options for integer conversions.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct IntOptions {
  /// Round to nearest with ties to even, clamping to the type's range.
  pub clamp: bool,
  /// Throw for NaN, infinities and out-of-range values.
  pub enforce_range: bool,
}

/// Options for string conversions.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct StringOptions {
  /// Convert `null` to the empty string instead of `"null"`.
  pub treat_null_as_empty_string: bool,
}

fn own(prefix: &str) -> Cow<'static, str> {
  Cow::Owned(prefix.to_owned())
}

/// Converts a value to a JavaScript-compatible string via ToString. Used by
/// generated enum converters.
#[doc(hidden)]
pub fn string_value<'a, C>(
  scope: &mut v8::HandleScope<'a>,
  value: v8::Local<'a, v8::Value>,
  prefix: &str,
  context: &C,
) -> Result<String, WebIdlError>
where
  C: Fn() -> Cow<'static, str>,
{
  if value.is_symbol() {
    return Err(WebIdlError::new(
      own(prefix),
      context,
      "is a Symbol and can not be converted to a string",
    ));
  }
  let Some(string) = value.to_string(scope) else {
    return Err(WebIdlError::js_exception(own(prefix), context));
  };
  Ok(string.to_rust_string_lossy(scope))
}

fn number_value<'a, C>(
  scope: &mut v8::HandleScope<'a>,
  value: v8::Local<'a, v8::Value>,
  prefix: &str,
  context: &C,
) -> Result<f64, WebIdlError>
where
  C: Fn() -> Cow<'static, str>,
{
  if value.is_symbol() {
    return Err(WebIdlError::new(
      own(prefix),
      context,
      "is a Symbol and can not be converted to a number",
    ));
  }
  if value.is_big_int() {
    return Err(WebIdlError::new(
      own(prefix),
      context,
      "is a BigInt and can not be converted to a number",
    ));
  }
  let Some(number) = value.to_number(scope) else {
    return Err(WebIdlError::js_exception(own(prefix), context));
  };
  Ok(number.value())
}

/// Returns the value as a `Local<Object>`, or `None` for `undefined`/`null`.
/// Used by generated dictionary converters.
#[doc(hidden)]
pub fn dictionary_object<'a, C>(
  value: v8::Local<'a, v8::Value>,
  dictionary_name: &'static str,
  prefix: &str,
  context: &C,
) -> Result<Option<v8::Local<'a, v8::Object>>, WebIdlError>
where
  C: Fn() -> Cow<'static, str>,
{
  if value.is_null() || value.is_undefined() {
    return Ok(None);
  }
  match v8::Local::<v8::Object>::try_from(value) {
    Ok(object) => Ok(Some(object)),
    Err(_) => Err(WebIdlError::new(
      own(prefix),
      context,
      format!("can not be converted to '{dictionary_name}' because it is not an object"),
    )),
  }
}

/// Reads and converts a dictionary member. Returns `None` if the dictionary
/// is absent or the member is `undefined`. Used by generated dictionary
/// converters.
#[doc(hidden)]
#[allow(clippy::too_many_arguments)]
pub fn dictionary_member<'a, T, C>(
  scope: &mut v8::HandleScope<'a>,
  dictionary: Option<v8::Local<'a, v8::Object>>,
  dictionary_name: &'static str,
  key: &'static str,
  prefix: Cow<'static, str>,
  context: &C,
  options: &T::Options,
) -> Result<Option<T>, WebIdlError>
where
  T: WebIdlConverter<'a>,
  C: Fn() -> Cow<'static, str>,
{
  let Some(dictionary) = dictionary else {
    return Ok(None);
  };
  let key_name = v8::String::new(scope, key).unwrap();
  let Some(value) = dictionary.get(scope, key_name.into()) else {
    return Err(WebIdlError::js_exception(prefix, context));
  };
  if value.is_undefined() {
    return Ok(None);
  }
  let member_context = || {
    Cow::Owned(format!(
      "{} ('{}' of '{}')",
      context(),
      key,
      dictionary_name
    ))
  };
  T::convert(scope, value, prefix, member_context, options).map(Some)
}

/// Error for a dictionary member that is missing and has no default.
#[doc(hidden)]
pub fn dictionary_missing_member<C>(
  prefix: Cow<'static, str>,
  context: &C,
  dictionary_name: &'static str,
  key: &'static str,
) -> WebIdlError
where
  C: Fn() -> Cow<'static, str>,
{
  WebIdlError::new(
    prefix,
    context,
    format!(
      "can not be converted to '{dictionary_name}' because '{key}' is required"
    ),
  )
}

/// Integer range of a type: `(bits, signed, enforce/clamp lower, enforce/clamp upper)`.
type IntRange = (u32, bool, f64, f64);

fn convert_integer<'a, C>(
  scope: &mut v8::HandleScope<'a>,
  value: v8::Local<'a, v8::Value>,
  prefix: &str,
  context: &C,
  options: &IntOptions,
  range: IntRange,
) -> Result<i128, WebIdlError>
where
  C: Fn() -> Cow<'static, str>,
{
  let (bits, signed, lo, hi) = range;
  let n = number_value(scope, value, prefix, context)?;
  if options.enforce_range {
    if !n.is_finite() {
      return Err(WebIdlError::new(
        own(prefix),
        context,
        "is not a finite number",
      ));
    }
    let x = n.trunc();
    if x < lo || x > hi {
      return Err(WebIdlError::new(
        own(prefix),
        context,
        format!("is outside the range {lo} to {hi}"),
      ));
    }
    return Ok(x as i128);
  }
  if options.clamp {
    if n.is_nan() {
      return Ok(0);
    }
    return Ok(n.clamp(lo, hi).round_ties_even() as i128);
  }
  if !n.is_finite() {
    return Ok(0);
  }
  // Modulo 2^bits, computed exactly: fmod is exact and the result fits in i128.
  let modulus: i128 = 1 << bits;
  let remainder = (n.trunc() % (modulus as f64)) as i128;
  let mut wrapped = remainder.rem_euclid(modulus);
  if signed && wrapped >= modulus / 2 {
    wrapped -= modulus;
  }
  Ok(wrapped)
}

macro_rules! int_converter {
  ($($t:ty => ($bits:expr, $signed:expr, $lo:expr, $hi:expr)),* $(,)?) => {$(
    impl<'a> WebIdlConverter<'a> for $t {
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
        convert_integer(
          scope,
          value,
          &prefix,
          &context,
          options,
          ($bits, $signed, $lo, $hi),
        )
        .map(|v| v as $t)
      }
    }
  )*};
}

// The enforce/clamp bounds of i64 and u64 are limited to the integers that
// are exactly representable as f64.
int_converter! {
  i8 => (8, true, -128.0, 127.0),
  u8 => (8, false, 0.0, 255.0),
  i16 => (16, true, -32768.0, 32767.0),
  u16 => (16, false, 0.0, 65535.0),
  i32 => (32, true, -2147483648.0, 2147483647.0),
  u32 => (32, false, 0.0, 4294967295.0),
  i64 => (64, true, -9007199254740991.0, 9007199254740991.0),
  u64 => (64, false, 0.0, 9007199254740991.0),
}

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
    let n = number_value(scope, value, &prefix, &context)?;
    if !n.is_finite() {
      return Err(WebIdlError::new(prefix, &context, "is not a finite number"));
    }
    Ok(n)
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
    let n = number_value(scope, value, &prefix, &context)?;
    if !n.is_finite() {
      return Err(WebIdlError::new(prefix, &context, "is not a finite number"));
    }
    // Round to nearest, ties to even. Values that round up to 2^128 overflow
    // to infinity, which is out of range.
    let x = n as f32;
    if x.is_infinite() {
      return Err(WebIdlError::new(
        prefix,
        &context,
        "is outside the range of a float",
      ));
    }
    Ok(x)
  }
}

/// WebIDL `unrestricted float`: accepts NaN and infinities.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct UnrestrictedFloat(pub f32);

impl Deref for UnrestrictedFloat {
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
    _options: &(),
  ) -> Result<Self, WebIdlError>
  where
    C: Fn() -> Cow<'static, str>,
  {
    let n = number_value(scope, value, &prefix, &context)?;
    Ok(UnrestrictedFloat(n as f32))
  }
}

/// WebIDL `unrestricted double`: accepts NaN and infinities.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct UnrestrictedDouble(pub f64);

impl Deref for UnrestrictedDouble {
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
    _options: &(),
  ) -> Result<Self, WebIdlError>
  where
    C: Fn() -> Cow<'static, str>,
  {
    let n = number_value(scope, value, &prefix, &context)?;
    Ok(UnrestrictedDouble(n))
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

/// Passes the value through unchanged.
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

/// WebIDL `DOMString`.
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
    if options.treat_null_as_empty_string && value.is_null() {
      return Ok(String::new());
    }
    string_value(scope, value, &prefix, &context)
  }
}

/// WebIDL `ByteString`: a string whose code units are all at most U+00FF.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ByteString(pub String);

impl Deref for ByteString {
  type Target = String;
  fn deref(&self) -> &String {
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
    options: &StringOptions,
  ) -> Result<Self, WebIdlError>
  where
    C: Fn() -> Cow<'static, str>,
  {
    let string =
      String::convert(scope, value, prefix.clone(), &context, options)?;
    // Lone surrogates are already U+FFFD here, which is above U+00FF.
    if string.chars().any(|c| c as u32 > 0xFF) {
      return Err(WebIdlError::new(
        prefix,
        &context,
        "is not a valid ByteString because it contains a code unit greater than U+00FF",
      ));
    }
    Ok(ByteString(string))
  }
}

/// WebIDL `bigint`, stored as its sign and little-endian 64-bit words.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BigInt {
  /// True if the value is negative.
  pub sign: bool,
  /// Magnitude, least significant word first. Empty for zero.
  pub words: Vec<u64>,
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
    let big_int = match v8::Local::<v8::BigInt>::try_from(value) {
      Ok(big_int) => big_int,
      // Objects run user code (valueOf, toString), so their exceptions are
      // propagated as they are.
      Err(_) if value.is_object() => match value.to_big_int(scope) {
        Some(big_int) => big_int,
        None => return Err(WebIdlError::js_exception(prefix, &context)),
      },
      Err(_) => {
        // ToBigInt throws for most other non-BigInt values. Swallow that
        // exception and report a WebIdl error instead.
        let mut tc = v8::TryCatch::new(scope);
        let Some(big_int) = value.to_big_int(&mut tc) else {
          return Err(WebIdlError::new(
            prefix,
            &context,
            "can not be converted to a BigInt",
          ));
        };
        big_int
      }
    };
    let mut words = vec![0u64; big_int.word_count()];
    let (sign, _) = big_int.to_words_array(&mut words);
    Ok(BigInt { sign, words })
  }
}

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
    if value.is_null() || value.is_undefined() {
      return Ok(None);
    }
    T::convert(scope, value, prefix, context, options).map(Some)
  }
}

/// Calls `obj[key]` and requires it to be a function.
fn get_function<'a, C>(
  scope: &mut v8::HandleScope<'a>,
  object: v8::Local<'a, v8::Object>,
  key: v8::Local<'a, v8::Value>,
  prefix: &str,
  context: &C,
  problem: &'static str,
) -> Result<v8::Local<'a, v8::Function>, WebIdlError>
where
  C: Fn() -> Cow<'static, str>,
{
  let Some(value) = object.get(scope, key) else {
    return Err(WebIdlError::js_exception(own(prefix), context));
  };
  v8::Local::<v8::Function>::try_from(value)
    .map_err(|_| WebIdlError::new(own(prefix), context, problem))
}

/// Calls `function` with `this`, mapping an exception to a js_exception error.
fn call_function<'a, C>(
  scope: &mut v8::HandleScope<'a>,
  function: v8::Local<'a, v8::Function>,
  this: v8::Local<'a, v8::Value>,
  prefix: &str,
  context: &C,
) -> Result<v8::Local<'a, v8::Value>, WebIdlError>
where
  C: Fn() -> Cow<'static, str>,
{
  function
    .call(scope, this, &[])
    .ok_or_else(|| WebIdlError::js_exception(own(prefix), context))
}

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
    let Ok(object) = v8::Local::<v8::Object>::try_from(value) else {
      return Err(WebIdlError::new(
        prefix,
        &context,
        "can not be converted to a sequence because it is not an object",
      ));
    };
    let iterator_key: v8::Local<v8::Value> =
      v8::Symbol::get_iterator(scope).into();
    let iterator_fn = get_function(
      scope,
      object,
      iterator_key,
      &prefix,
      &context,
      "can not be converted to a sequence because it is not iterable",
    )?;
    let iterator =
      call_function(scope, iterator_fn, object.into(), &prefix, &context)?;
    let Ok(iterator) = v8::Local::<v8::Object>::try_from(iterator) else {
      return Err(WebIdlError::new(
        prefix,
        &context,
        "can not be converted to a sequence because its iterator is not an object",
      ));
    };
    let next_key: v8::Local<v8::Value> =
      v8::String::new(scope, "next").unwrap().into();
    let next_fn = get_function(
      scope,
      iterator,
      next_key,
      &prefix,
      &context,
      "can not be converted to a sequence because its iterator has no next method",
    )?;
    let done_key: v8::Local<v8::Value> =
      v8::String::new(scope, "done").unwrap().into();
    let value_key: v8::Local<v8::Value> =
      v8::String::new(scope, "value").unwrap().into();

    let mut out = Vec::new();
    loop {
      let result =
        call_function(scope, next_fn, iterator.into(), &prefix, &context)?;
      let Ok(result) = v8::Local::<v8::Object>::try_from(result) else {
        return Err(WebIdlError::new(
          prefix,
          &context,
          "can not be converted to a sequence because an iterator result is not an object",
        ));
      };
      let Some(done) = result.get(scope, done_key) else {
        return Err(WebIdlError::js_exception(prefix, &context));
      };
      if done.boolean_value(scope) {
        break;
      }
      let Some(item) = result.get(scope, value_key) else {
        return Err(WebIdlError::js_exception(prefix, &context));
      };
      let index = out.len();
      let item_context =
        || Cow::Owned(format!("{} (element {})", context(), index));
      out.push(T::convert(
        scope,
        item,
        prefix.clone(),
        item_context,
        options,
      )?);
    }
    Ok(out)
  }
}

/// WebIDL `record<DOMString, T>`: own enumerable string-keyed properties.
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
    let Ok(object) = v8::Local::<v8::Object>::try_from(value) else {
      return Err(WebIdlError::new(
        prefix,
        &context,
        "can not be converted to a record because it is not an object",
      ));
    };
    let args = v8::GetPropertyNamesArgsBuilder::new()
      .mode(v8::KeyCollectionMode::OwnOnly)
      .property_filter(v8::PropertyFilter::ONLY_ENUMERABLE)
      .key_conversion(v8::KeyConversionMode::ConvertToString)
      .build();
    let Some(keys) = object.get_own_property_names(scope, args) else {
      return Err(WebIdlError::js_exception(prefix, &context));
    };
    let mut out = HashMap::new();
    for i in 0..keys.length() {
      let Some(key_value) = keys.get_index(scope, i) else {
        return Err(WebIdlError::js_exception(prefix, &context));
      };
      if key_value.is_symbol() {
        return Err(WebIdlError::new(
          prefix,
          &context,
          "can not be converted to a record because it has an enumerable Symbol key",
        ));
      }
      // Read with the original key: a lossy Rust string may name another
      // property (e.g. a lone surrogate becomes U+FFFD).
      let Some(item) = object.get(scope, key_value) else {
        return Err(WebIdlError::js_exception(prefix, &context));
      };
      let key = string_value(scope, key_value, &prefix, &context)?;
      let item_context =
        || Cow::Owned(format!("{} (key '{}')", context(), key));
      let converted =
        T::convert(scope, item, prefix.clone(), item_context, options)?;
      out.insert(key, converted);
    }
    Ok(out)
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::JsRuntime;
  use crate::RuntimeOptions;

  /// Evaluates `$code` in a fresh runtime and converts the result to `$t`.
  macro_rules! convert_with {
    ($t:ty, $code:expr, $options:expr $(,)?) => {{
      let mut runtime = JsRuntime::new(RuntimeOptions::default());
      let mut handle = runtime.handle_scope();
      let scope = &mut handle;
      let source = v8::String::new(scope, $code).unwrap();
      let script = v8::Script::compile(scope, source, None).unwrap();
      let value = script.run(scope).unwrap();
      let result = <$t as WebIdlConverter>::convert(
        scope,
        value,
        Cow::Borrowed("P"),
        || Cow::Borrowed("C"),
        &$options,
      );
      result
    }};
  }

  fn msg<T: fmt::Debug>(result: Result<T, WebIdlError>) -> String {
    result.unwrap_err().to_string()
  }

  #[test]
  fn error_display_order() {
    assert_eq!(
      msg(convert_with!(
        u8,
        "NaN",
        IntOptions {
          enforce_range: true,
          ..Default::default()
        }
      )),
      "P: C is not a finite number"
    );
    assert_eq!(
      msg(convert_with!(String, "Symbol()", StringOptions::default())),
      "P: C is a Symbol and can not be converted to a string"
    );
    assert_eq!(
      msg(convert_with!(Vec<u8>, "'abc'", IntOptions::default())),
      "P: C can not be converted to a sequence because it is not an object"
    );
    assert_eq!(
      msg(convert_with!(HashMap<String, u8>, "null", IntOptions::default())),
      "P: C can not be converted to a record because it is not an object"
    );
  }

  #[test]
  fn any_passes_value_through() {
    let mut runtime = JsRuntime::new(RuntimeOptions::default());
    let scope = &mut runtime.handle_scope();
    let source = v8::String::new(scope, "({a: 1})").unwrap();
    let script = v8::Script::compile(scope, source, None).unwrap();
    let value = script.run(scope).unwrap();
    let out = v8::Local::<v8::Value>::convert(
      scope,
      value,
      Cow::Borrowed("P"),
      || Cow::Borrowed("C"),
      &(),
    )
    .unwrap();
    assert!(out.strict_equals(value));
  }

  #[test]
  fn bool_to_boolean() {
    for code in ["0", "-0", "NaN", "''", "null", "undefined", "0n"] {
      assert!(!convert_with!(bool, code, ()).unwrap(), "{code}");
    }
    for code in ["'false'", "({})", "[]", "Symbol()"] {
      assert!(convert_with!(bool, code, ()).unwrap(), "{code}");
    }
  }

  #[test]
  fn integers_default() {
    let opts = IntOptions::default();
    assert_eq!(convert_with!(u8, "256", opts).unwrap(), 0);
    assert_eq!(convert_with!(u8, "-1", opts).unwrap(), 255);
    assert_eq!(convert_with!(u8, "'12'", opts).unwrap(), 12);
    assert_eq!(convert_with!(u8, "undefined", opts).unwrap(), 0);
    assert_eq!(convert_with!(u8, "NaN", opts).unwrap(), 0);
    assert_eq!(convert_with!(u8, "Infinity", opts).unwrap(), 0);
    assert_eq!(convert_with!(u8, "1.9", opts).unwrap(), 1);
    assert_eq!(convert_with!(u8, "-0", opts).unwrap(), 0);
    assert_eq!(convert_with!(i8, "128", opts).unwrap(), -128);
    assert_eq!(convert_with!(i8, "-129", opts).unwrap(), 127);
    assert_eq!(convert_with!(u32, "2 ** 32 + 1", opts).unwrap(), 1);
    assert_eq!(convert_with!(u32, "-1", opts).unwrap(), 4294967295);
    assert_eq!(convert_with!(i32, "2 ** 31", opts).unwrap(), -2147483648);
    assert_eq!(convert_with!(i64, "2 ** 63", opts).unwrap(), i64::MIN);
    assert_eq!(convert_with!(u64, "-1", opts).unwrap(), u64::MAX);
  }

  #[test]
  fn integers_clamp() {
    let opts = IntOptions {
      clamp: true,
      ..Default::default()
    };
    assert_eq!(convert_with!(u8, "1.5", opts).unwrap(), 2);
    assert_eq!(convert_with!(u8, "2.5", opts).unwrap(), 2);
    assert_eq!(convert_with!(u8, "0.5", opts).unwrap(), 0);
    assert_eq!(convert_with!(u8, "300", opts).unwrap(), 255);
    assert_eq!(convert_with!(u8, "-5", opts).unwrap(), 0);
    assert_eq!(convert_with!(u8, "NaN", opts).unwrap(), 0);
    assert_eq!(convert_with!(u8, "Infinity", opts).unwrap(), 255);
  }

  #[test]
  fn integers_enforce_range() {
    let opts = IntOptions {
      enforce_range: true,
      ..Default::default()
    };
    for code in ["256", "-1", "NaN", "Infinity", "-Infinity", "undefined"] {
      assert!(convert_with!(u8, code, opts).is_err(), "{code}");
    }
    assert_eq!(convert_with!(u8, "255.9", opts).unwrap(), 255);
    assert!(convert_with!(u64, "2 ** 53", opts).is_err());
    assert_eq!(
      convert_with!(u64, "2 ** 53 - 1", opts).unwrap(),
      (1u64 << 53) - 1
    );
    assert!(convert_with!(i64, "-(2 ** 53)", opts).is_err());
    // enforce_range is checked first when both flags are set.
    let both = IntOptions {
      enforce_range: true,
      clamp: true,
    };
    assert!(convert_with!(u8, "300", both).is_err());
  }

  #[test]
  fn integers_reject_symbol_and_bigint() {
    let opts = IntOptions::default();
    assert!(convert_with!(i32, "Symbol()", opts).is_err());
    assert!(convert_with!(i32, "1n", opts).is_err());
  }

  #[test]
  fn integer_error_message_order() {
    let opts = IntOptions {
      enforce_range: true,
      ..Default::default()
    };
    assert_eq!(
      msg(convert_with!(u8, "256", opts)),
      "P: C is outside the range 0 to 255"
    );
  }

  #[test]
  fn floats() {
    assert_eq!(convert_with!(f64, "'1.5'", ()).unwrap(), 1.5);
    assert_eq!(convert_with!(f64, "null", ()).unwrap(), 0.0);
    assert!(convert_with!(f64, "undefined", ()).is_err());
    assert!(convert_with!(f64, "Infinity", ()).is_err());
    assert!(convert_with!(f64, "1n", ()).is_err());
    assert!(convert_with!(f64, "Symbol()", ()).is_err());
    assert!(convert_with!(f64, "-0", ()).unwrap().is_sign_negative());
    assert_eq!(convert_with!(f32, "1.5", ()).unwrap(), 1.5);
    assert!(convert_with!(f32, "1e40", ()).is_err());
    assert_eq!(convert_with!(f32, "3.4028235e38", ()).unwrap(), f32::MAX);
    // Above the midpoint between f32::MAX and 2^128 rounds up to 2^128, and
    // the midpoint itself rounds to even (also 2^128), so both are out of range.
    assert!(convert_with!(f32, "3.40282365e38", ()).is_err());
    assert!(convert_with!(f32, "3.4028235677973366e38", ()).is_err());
    assert!(convert_with!(f32, "-3.40282365e38", ()).is_err());
    assert!(convert_with!(f32, "-3.4028235e38", ()).is_ok());
    assert!(convert_with!(f32, "NaN", ()).is_err());
    assert!(convert_with!(f32, "undefined", ()).is_err());
    assert!(convert_with!(f32, "null", ()).unwrap() == 0.0);
  }

  #[test]
  fn unrestricted_floats() {
    assert!(convert_with!(UnrestrictedDouble, "NaN", ())
      .unwrap()
      .is_nan());
    assert_eq!(
      convert_with!(UnrestrictedDouble, "-Infinity", ())
        .unwrap()
        .0,
      f64::NEG_INFINITY
    );
    assert_eq!(
      convert_with!(UnrestrictedFloat, "1e40", ()).unwrap().0,
      f32::INFINITY
    );
    assert!(convert_with!(UnrestrictedFloat, "Infinity", ())
      .unwrap()
      .is_infinite());
    assert!(convert_with!(UnrestrictedFloat, "Symbol()", ()).is_err());
  }

  #[test]
  fn strings() {
    let opts = StringOptions::default();
    assert_eq!(convert_with!(String, "null", opts).unwrap(), "null");
    assert_eq!(
      convert_with!(String, "undefined", opts).unwrap(),
      "undefined"
    );
    assert_eq!(convert_with!(String, "1", opts).unwrap(), "1");
    assert_eq!(
      convert_with!(String, "({toString(){return 'x'}})", opts).unwrap(),
      "x"
    );
    assert!(convert_with!(String, "Symbol()", opts).is_err());
    assert_eq!(
      convert_with!(String, "'héllo €😀'", opts).unwrap(),
      "héllo €😀"
    );
    assert_eq!(
      convert_with!(String, "'\\uD800'", opts).unwrap(),
      "\u{FFFD}"
    );
    let nulls = StringOptions {
      treat_null_as_empty_string: true,
    };
    assert_eq!(convert_with!(String, "null", nulls).unwrap(), "");
    assert_eq!(
      convert_with!(String, "undefined", nulls).unwrap(),
      "undefined"
    );
  }

  #[test]
  fn byte_strings() {
    let opts = StringOptions::default();
    assert_eq!(convert_with!(ByteString, "'\\xff'", opts).unwrap().0, "ÿ");
    assert_eq!(convert_with!(ByteString, "''", opts).unwrap().0, "");
    assert!(convert_with!(ByteString, "'€'", opts).is_err());
    assert!(convert_with!(ByteString, "'\\uD800'", opts).is_err());
  }

  #[test]
  fn bigints() {
    assert_eq!(
      convert_with!(BigInt, "-5n", ()).unwrap(),
      BigInt {
        sign: true,
        words: vec![5]
      }
    );
    assert_eq!(
      convert_with!(BigInt, "2n ** 64n", ()).unwrap(),
      BigInt {
        sign: false,
        words: vec![0, 1]
      }
    );
    let zero = convert_with!(BigInt, "0n", ()).unwrap();
    assert!(!zero.sign);
    assert!(zero.words.iter().all(|w| *w == 0));
    assert_eq!(convert_with!(BigInt, "true", ()).unwrap().words, vec![1]);
    assert_eq!(convert_with!(BigInt, "'123'", ()).unwrap().words, vec![123]);
    for code in ["'abc'", "1", "undefined", "null", "Symbol()"] {
      assert!(convert_with!(BigInt, code, ()).is_err(), "{code}");
    }
  }

  #[test]
  fn options_and_nullable() {
    assert_eq!(
      convert_with!(Option<u8>, "null", IntOptions::default()).unwrap(),
      None
    );
    assert_eq!(
      convert_with!(Option<u8>, "undefined", IntOptions::default()).unwrap(),
      None
    );
    assert_eq!(
      convert_with!(Option<u8>, "3", IntOptions::default()).unwrap(),
      Some(3)
    );
    let clamp = IntOptions {
      clamp: true,
      ..Default::default()
    };
    assert_eq!(convert_with!(Option<u8>, "300", clamp).unwrap(), Some(255));
    assert!(convert_with!(Option<u8>, "Symbol()", clamp).is_err());
  }

  #[test]
  fn sequences() {
    assert_eq!(
      convert_with!(Vec<u8>, "[]", IntOptions::default()).unwrap(),
      Vec::<u8>::new()
    );
    assert_eq!(
      convert_with!(Vec<u8>, "[1, 2]", IntOptions::default()).unwrap(),
      vec![1, 2]
    );
    assert_eq!(
      convert_with!(Vec<u8>, "new Set([3])", IntOptions::default()).unwrap(),
      vec![3]
    );
    assert_eq!(
      convert_with!(
        Vec<u8>,
        "(function*(){ yield 4; })()",
        IntOptions::default()
      )
      .unwrap(),
      vec![4]
    );
    assert_eq!(
      convert_with!(Vec<u8>,
        "({[Symbol.iterator]() { let i = 0; return { next() { return i < 2 ? { value: i++, done: false } : { done: true }; } }; } })",
        IntOptions::default()
      )
      .unwrap(),
      vec![0, 1]
    );
    let holes =
      convert_with!(Vec<Option<u8>>, "[1,,3]", IntOptions::default()).unwrap();
    assert_eq!(holes, vec![Some(1), None, Some(3)]);
    for code in ["'abc'", "null", "undefined", "({length: 2})"] {
      assert!(
        convert_with!(Vec<u8>, code, IntOptions::default()).is_err(),
        "{code}"
      );
    }
    let enforce = IntOptions {
      enforce_range: true,
      ..Default::default()
    };
    assert_eq!(
      msg(convert_with!(Vec<u8>, "[1, 'x']", enforce)),
      "P: C (element 1) is not a finite number"
    );
  }

  #[test]
  fn records() {
    let opts = IntOptions::default();
    let map = convert_with!(HashMap<String, u8>,
      "({b: 2, 1: 1, a: 3})",
      opts,
    )
    .unwrap();
    assert_eq!(map.len(), 3);
    assert_eq!(map["1"], 1);
    assert_eq!(map["b"], 2);
    // Non-enumerable and inherited properties are skipped.
    let map = convert_with!(HashMap<String, u8>,
      "Object.create({inherited: 1}, {hidden: {value: 2, enumerable: false}, shown: {value: 3, enumerable: true}})",
      opts,
    )
    .unwrap();
    assert_eq!(map.len(), 1);
    assert_eq!(map["shown"], 3);
    // Getters run.
    let map = convert_with!(HashMap<String, u8>,
      "({get g() { return 7; }})",
      opts,
    )
    .unwrap();
    assert_eq!(map["g"], 7);
    for code in ["null", "undefined", "'x'", "1"] {
      assert!(
        convert_with!(HashMap<String, u8>, code, opts).is_err(),
        "{code}"
      );
    }
    assert!(convert_with!(HashMap<String, u8>,
      "({[Symbol()]: 1})",
      opts
    )
    .is_err());
    let err = convert_with!(HashMap<String, u8>,
      "({a: 'x'})",
      IntOptions { enforce_range: true, ..Default::default() },
    );
    assert_eq!(msg(err), "P: C (key 'a') is not a finite number");
    // A lone surrogate key is read by its original value, not the lossy name.
    let map = convert_with!(HashMap<String, u32>,
      r#"({"\uD800": 5})"#,
      IntOptions::default(),
    )
    .unwrap();
    assert_eq!(map.len(), 1);
    assert_eq!(map["\u{FFFD}"], 5);
  }

  #[test]
  fn proxy_traps_errors() {
    let opts = IntOptions::default();
    assert!(convert_with!(HashMap<String, u8>,
      "new Proxy({}, { ownKeys() { throw new Error('boom'); } })",
      opts
    )
    .is_err());
    assert!(convert_with!(HashMap<String, u8>,
      "new Proxy({a: 1}, { getOwnPropertyDescriptor() { throw new Error('boom'); } })",
      opts
    )
    .is_err());
  }

  #[test]
  fn user_exceptions_become_errors() {
    let opts = IntOptions::default();
    assert!(
      convert_with!(u8, "({valueOf(){ throw new Error('x'); }})", opts)
        .is_err()
    );
    assert!(convert_with!(
      String,
      "({toString(){ throw new Error('x'); }})",
      StringOptions::default()
    )
    .is_err());
    assert!(convert_with!(Vec<u8>,
      "({[Symbol.iterator]() { return { next() { throw new Error('x'); } }; }})",
      opts
    )
    .is_err());
    assert!(convert_with!(HashMap<String, u8>,
      "({get a() { throw new Error('x'); }})",
      opts
    )
    .is_err());
    assert!(convert_with!(
      BigInt,
      "({valueOf(){ throw new Error('x'); }})",
      ()
    )
    .is_err());
    // A user exception from an object is kept, not replaced by a WebIdl error.
    let err =
      convert_with!(BigInt, "({valueOf(){ throw new Error('x'); }})", ());
    assert!(err.unwrap_err().is_js_exception());
    assert_eq!(
      msg(convert_with!(BigInt, "1", ())),
      "P: C can not be converted to a BigInt"
    );
  }
}
