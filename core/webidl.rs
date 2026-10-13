// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.

//! Conversions from JavaScript values to WebIDL types, following the
//! JS-to-IDL conversion algorithms in the WebIDL specification:
//! <https://webidl.spec.whatwg.org/#js-type-mapping>.
//!
//! Use the [`WebIDL`](crate::WebIDL) derive for dictionaries and enums, and the
//! `#[webidl]` argument attribute on `#[op2]` ops to convert arguments.

use std::borrow::Cow;
use std::collections::HashMap;
use std::fmt;

use crate::v8;

/// Converts a JavaScript value into a Rust value using WebIDL semantics.
pub trait WebIdlConverter<'a>: Sized {
  /// Options that control the conversion (for example, `[Clamp]` or
  /// `[EnforceRange]` for integers).
  type Options: Default;

  /// Converts `value`. `prefix` and `context` are used to build the error
  /// message, e.g. `Failed to execute 'fetch'` and `Argument 1`.
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

/// An error produced while converting a JavaScript value. Displays as
/// `"{prefix}: {context} {message}"`, for example
/// `Failed to execute 'fetch': Argument 1 is not a finite number.`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebIdlError {
  prefix: Cow<'static, str>,
  context: Cow<'static, str>,
  message: Cow<'static, str>,
}

impl WebIdlError {
  pub fn new(
    prefix: impl Into<Cow<'static, str>>,
    context: impl Into<Cow<'static, str>>,
    message: impl Into<Cow<'static, str>>,
  ) -> Self {
    Self {
      prefix: prefix.into(),
      context: context.into(),
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

/// Options for integer conversions.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct IntOptions {
  /// `[Clamp]`: clamp out-of-range values to the type's range.
  pub clamp: bool,
  /// `[EnforceRange]`: throw on non-finite or out-of-range values.
  pub enforce_range: bool,
}

/// Options for string conversions.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StringOptions {
  /// `[LegacyNullToEmptyString]`: `null` converts to `""`.
  pub treat_null_as_empty_string: bool,
}

/// `unrestricted float`: accepts NaN and infinities.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct UnrestrictedFloat(pub f32);

impl std::ops::Deref for UnrestrictedFloat {
  type Target = f32;
  fn deref(&self) -> &f32 {
    &self.0
  }
}

/// `unrestricted double`: accepts NaN and infinities.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct UnrestrictedDouble(pub f64);

impl std::ops::Deref for UnrestrictedDouble {
  type Target = f64;
  fn deref(&self) -> &f64 {
    &self.0
  }
}

/// `ByteString`: a string whose code units are all in `U+0000..=U+00FF`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ByteString(pub String);

impl std::ops::Deref for ByteString {
  type Target = String;
  fn deref(&self) -> &String {
    &self.0
  }
}

/// `bigint`: a sign and magnitude, with the magnitude as little-endian words.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BigInt {
  pub sign: bool,
  pub words: Vec<u64>,
}

fn fail<C>(
  prefix: &str,
  context: &C,
  message: impl Into<Cow<'static, str>>,
) -> WebIdlError
where
  C: Fn() -> Cow<'static, str>,
{
  WebIdlError::new(Cow::Owned(prefix.to_owned()), context(), message)
}

fn threw<C>(prefix: &str, context: &C) -> WebIdlError
where
  C: Fn() -> Cow<'static, str>,
{
  fail(prefix, context, "threw an exception during conversion.")
}

fn key<'a>(
  scope: &mut v8::HandleScope<'a>,
  name: &str,
) -> v8::Local<'a, v8::Value> {
  // Property names are short ASCII literals, so allocation cannot fail.
  v8::String::new(scope, name).unwrap().into()
}

/// ToNumber, rejecting Symbols and BigInts with a WebIDL error.
fn to_number<'a, C>(
  scope: &mut v8::HandleScope<'a>,
  value: v8::Local<'a, v8::Value>,
  prefix: &str,
  context: &C,
) -> Result<f64, WebIdlError>
where
  C: Fn() -> Cow<'static, str>,
{
  if value.is_symbol() || value.is_big_int() {
    return Err(fail(prefix, context, "cannot be converted to a number."));
  }
  value
    .to_number(scope)
    .map(|n| n.value())
    .ok_or_else(|| threw(prefix, context))
}

/// ToString, rejecting Symbols with a WebIDL error. Used by `DOMString`,
/// `ByteString` and enums.
#[doc(hidden)]
pub fn to_string_value<'a, C>(
  scope: &mut v8::HandleScope<'a>,
  value: v8::Local<'a, v8::Value>,
  prefix: &str,
  context: &C,
) -> Result<String, WebIdlError>
where
  C: Fn() -> Cow<'static, str>,
{
  if value.is_symbol() {
    return Err(fail(prefix, context, "cannot be converted to a string."));
  }
  let string = value
    .to_string(scope)
    .ok_or_else(|| threw(prefix, context))?;
  Ok(string.to_rust_string_lossy(scope))
}

fn to_dom_string<'a, C>(
  scope: &mut v8::HandleScope<'a>,
  value: v8::Local<'a, v8::Value>,
  prefix: &str,
  context: &C,
  options: &StringOptions,
) -> Result<String, WebIdlError>
where
  C: Fn() -> Cow<'static, str>,
{
  if options.treat_null_as_empty_string && value.is_null() {
    return Ok(String::new());
  }
  to_string_value(scope, value, prefix, context)
}

fn to_object<'a, C>(
  value: v8::Local<'a, v8::Value>,
  prefix: &str,
  context: &C,
) -> Result<v8::Local<'a, v8::Object>, WebIdlError>
where
  C: Fn() -> Cow<'static, str>,
{
  v8::Local::<v8::Object>::try_from(value)
    .map_err(|_| fail(prefix, context, "is not an object."))
}

/// Range used for `[Clamp]` and `[EnforceRange]`. 64-bit types use the
/// integers that are exactly representable as doubles.
fn int_range(bits: u32, signed: bool) -> (f64, f64) {
  if bits == 64 {
    let max = 9007199254740991.0; // 2^53 - 1
    return (if signed { -max } else { 0.0 }, max);
  }
  if signed {
    let half = 2f64.powi(bits as i32 - 1);
    (-half, half - 1.0)
  } else {
    (0.0, 2f64.powi(bits as i32) - 1.0)
  }
}

/// ConvertToInt from the WebIDL spec.
fn convert_integer<'a, C>(
  scope: &mut v8::HandleScope<'a>,
  value: v8::Local<'a, v8::Value>,
  prefix: &str,
  context: &C,
  options: &IntOptions,
  bits: u32,
  signed: bool,
) -> Result<i128, WebIdlError>
where
  C: Fn() -> Cow<'static, str>,
{
  let x = to_number(scope, value, prefix, context)?;
  let (lo, hi) = int_range(bits, signed);
  if options.enforce_range {
    if !x.is_finite() {
      return Err(fail(prefix, context, "is not a finite number."));
    }
    let x = x.trunc();
    if x < lo || x > hi {
      return Err(fail(
        prefix,
        context,
        format!("is outside the accepted range of {bits}-bit integers."),
      ));
    }
    return Ok(x as i128);
  }
  if options.clamp {
    if x.is_nan() {
      return Ok(0);
    }
    return Ok(x.clamp(lo, hi).round_ties_even() as i128);
  }
  if !x.is_finite() {
    return Ok(0);
  }
  // Modulo 2^bits. `fmod` is exact for doubles, so reduce in f64 and do the
  // wrap in i128 to avoid any rounding of the intermediate result.
  let remainder = x.trunc() % 2f64.powi(bits as i32);
  let v = (remainder as i128).rem_euclid(1i128 << bits);
  Ok(if signed && v >= 1i128 << (bits - 1) {
    v - (1i128 << bits)
  } else {
    v
  })
}

macro_rules! impl_integer {
  ($($ty:ty => $bits:expr, $signed:expr;)*) => {
    $(
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
          convert_integer(scope, value, &prefix, &context, options, $bits, $signed)
            .map(|v| v as $ty)
        }
      }
    )*
  };
}

impl_integer! {
  i8 => 8, true;
  u8 => 8, false;
  i16 => 16, true;
  u16 => 16, false;
  i32 => 32, true;
  u32 => 32, false;
  i64 => 64, true;
  u64 => 64, false;
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
      return Err(fail(&prefix, &context, "is not a finite number."));
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
      return Err(fail(&prefix, &context, "is not a finite number."));
    }
    let y = x as f32;
    if y.is_infinite() {
      return Err(fail(&prefix, &context, "is outside the range of a float."));
    }
    Ok(y)
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
    to_number(scope, value, &prefix, &context).map(UnrestrictedDouble)
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
    // `as` rounds to the nearest f32 and overflows to infinity.
    to_number(scope, value, &prefix, &context)
      .map(|x| UnrestrictedFloat(x as f32))
  }
}

/// Converts a value to a `DOMString` (ToString). Rust strings cannot hold
/// lone surrogates, so any lone surrogate in the JS string becomes U+FFFD.
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
    to_dom_string(scope, value, &prefix, &context, options)
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
    let s = to_dom_string(scope, value, &prefix, &context, options)?;
    // Each char is one UTF-16 code unit here (lossy conversion turns lone
    // surrogates into U+FFFD, which is also rejected).
    if s.chars().any(|c| c as u32 > 0xFF) {
      return Err(fail(
        &prefix,
        &context,
        "contains a character that is not a valid byte string.",
      ));
    }
    Ok(ByteString(s))
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
    if value.is_undefined()
      || value.is_null()
      || value.is_number()
      || value.is_symbol()
    {
      return Err(fail(&prefix, &context, "cannot be converted to a BigInt."));
    }
    let big = value.to_big_int(scope).ok_or_else(|| {
      fail(&prefix, &context, "cannot be converted to a BigInt.")
    })?;
    let mut buf = vec![0u64; big.word_count()];
    let (sign, words) = big.to_words_array(&mut buf);
    Ok(BigInt {
      sign,
      words: words.to_vec(),
    })
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
    options: &T::Options,
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
    options: &T::Options,
  ) -> Result<Self, WebIdlError>
  where
    C: Fn() -> Cow<'static, str>,
  {
    let object = to_object(value, &prefix, &context)?;
    let iterator_key: v8::Local<v8::Value> =
      v8::Symbol::get_iterator(scope).into();
    let method = object
      .get(scope, iterator_key)
      .ok_or_else(|| threw(&prefix, &context))?;
    let method = v8::Local::<v8::Function>::try_from(method)
      .map_err(|_| fail(&prefix, &context, "is not iterable."))?;
    let iterator = method
      .call(scope, value, &[])
      .ok_or_else(|| threw(&prefix, &context))?;
    let iterator = v8::Local::<v8::Object>::try_from(iterator)
      .map_err(|_| fail(&prefix, &context, "is not iterable."))?;
    let next_key = key(scope, "next");
    let next = iterator
      .get(scope, next_key)
      .ok_or_else(|| threw(&prefix, &context))?;
    let next = v8::Local::<v8::Function>::try_from(next)
      .map_err(|_| fail(&prefix, &context, "iterator has no next method."))?;

    let mut out = Vec::new();
    loop {
      let result = next
        .call(scope, iterator.into(), &[])
        .ok_or_else(|| threw(&prefix, &context))?;
      let result = v8::Local::<v8::Object>::try_from(result).map_err(|_| {
        fail(&prefix, &context, "iterator result is not an object.")
      })?;
      let done_key = key(scope, "done");
      let done = result
        .get(scope, done_key)
        .ok_or_else(|| threw(&prefix, &context))?
        .boolean_value(scope);
      if done {
        break;
      }
      let value_key = key(scope, "value");
      let item = result
        .get(scope, value_key)
        .ok_or_else(|| threw(&prefix, &context))?;
      let index = out.len();
      out.push(T::convert(
        scope,
        item,
        prefix.clone(),
        || Cow::Owned(format!("{} element {index}", context())),
        options,
      )?);
    }
    Ok(out)
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
    options: &T::Options,
  ) -> Result<Self, WebIdlError>
  where
    C: Fn() -> Cow<'static, str>,
  {
    let object = to_object(value, &prefix, &context)?;
    let keys = object
      .get_own_property_names(
        scope,
        v8::GetPropertyNamesArgs {
          mode: v8::KeyCollectionMode::OwnOnly,
          property_filter: v8::PropertyFilter::ONLY_ENUMERABLE,
          index_filter: v8::IndexFilter::IncludeIndices,
          key_conversion: v8::KeyConversionMode::ConvertToString,
        },
      )
      .ok_or_else(|| threw(&prefix, &context))?;
    let mut map = HashMap::new();
    for i in 0..keys.length() {
      let property_key = keys
        .get_index(scope, i)
        .ok_or_else(|| threw(&prefix, &context))?;
      let name = to_string_value(scope, property_key, &prefix, &context)?;
      let property = object
        .get(scope, property_key)
        .ok_or_else(|| threw(&prefix, &context))?;
      let converted = T::convert(
        scope,
        property,
        prefix.clone(),
        || Cow::Owned(format!("{} property \"{name}\"", context())),
        options,
      )?;
      map.insert(name, converted);
    }
    Ok(map)
  }
}

/// Reads the object for a dictionary. `undefined` and `null` mean that every
/// member is missing, so `None` is returned for them.
#[doc(hidden)]
pub fn dictionary_object<'a, C>(
  value: v8::Local<'a, v8::Value>,
  prefix: &str,
  context: &C,
) -> Result<Option<v8::Local<'a, v8::Object>>, WebIdlError>
where
  C: Fn() -> Cow<'static, str>,
{
  if value.is_undefined() || value.is_null() {
    return Ok(None);
  }
  to_object(value, prefix, context).map(Some)
}

/// Reads one dictionary member with a normal Get. Returns `None` when the
/// member is missing or `undefined`.
#[doc(hidden)]
pub fn dictionary_member<'a, C>(
  scope: &mut v8::HandleScope<'a>,
  object: Option<v8::Local<'a, v8::Object>>,
  name: &str,
  prefix: &str,
  context: &C,
) -> Result<Option<v8::Local<'a, v8::Value>>, WebIdlError>
where
  C: Fn() -> Cow<'static, str>,
{
  let Some(object) = object else {
    return Ok(None);
  };
  let name_key = key(scope, name);
  let value = object
    .get(scope, name_key)
    .ok_or_else(|| threw(prefix, context))?;
  if value.is_undefined() {
    return Ok(None);
  }
  Ok(Some(value))
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::JsRuntime;

  #[derive(crate::WebIDL, Debug, PartialEq)]
  #[webidl(dictionary)]
  struct Dict {
    #[webidl(required)]
    foo_bar: u8,
    #[webidl(default = 7)]
    count: u32,
    opt: Option<String>,
    #[options(clamp = true)]
    #[webidl(required)]
    clamped: u8,
    #[options(treat_null_as_empty_string = true)]
    #[webidl(default = String::from("unset"))]
    text: String,
  }

  #[derive(crate::WebIDL, Debug, PartialEq)]
  #[webidl(enum)]
  enum Kind {
    FooBar,
    #[webidl(rename = "other")]
    Plain,
  }

  fn runtime() -> JsRuntime {
    JsRuntime::new(Default::default())
  }

  fn eval<'a>(
    scope: &mut v8::HandleScope<'a>,
    js: &str,
  ) -> v8::Local<'a, v8::Value> {
    let code = v8::String::new(scope, js).unwrap();
    let script = v8::Script::compile(scope, code, None).unwrap();
    script.run(scope).unwrap()
  }

  fn ctx() -> Cow<'static, str> {
    Cow::Borrowed("Argument 1")
  }

  /// Evaluates `js` and converts it with default options.
  fn conv<'a, T>(
    scope: &mut v8::HandleScope<'a>,
    js: &str,
  ) -> Result<T, WebIdlError>
  where
    T: WebIdlConverter<'a>,
  {
    conv_with::<T>(scope, js, T::Options::default())
  }

  fn conv_with<'a, T>(
    scope: &mut v8::HandleScope<'a>,
    js: &str,
    options: T::Options,
  ) -> Result<T, WebIdlError>
  where
    T: WebIdlConverter<'a>,
  {
    let tc = &mut v8::TryCatch::new(scope);
    let value = eval(tc, js);
    T::convert(
      tc,
      value,
      Cow::Borrowed("Failed to execute 'op'"),
      ctx,
      &options,
    )
  }

  fn int_opts(clamp: bool, enforce_range: bool) -> IntOptions {
    IntOptions {
      clamp,
      enforce_range,
    }
  }

  #[test]
  fn error_display_and_traits() {
    let err = WebIdlError::new(
      "Failed to execute 'fetch'",
      "Argument 1",
      "is not a finite number.",
    );
    assert_eq!(
      err.to_string(),
      "Failed to execute 'fetch': Argument 1 is not a finite number."
    );
    fn assert_send_sync<T: Send + Sync + 'static + std::error::Error>() {}
    assert_send_sync::<WebIdlError>();
    let anyhow_err = crate::anyhow::Error::from(err);
    assert!(anyhow_err.to_string().contains("Argument 1"));
  }

  #[test]
  fn context_not_called_on_success() {
    let mut runtime = runtime();
    let scope = &mut runtime.handle_scope();
    let value = eval(scope, "1");
    let r = u8::convert(
      scope,
      value,
      Cow::Borrowed("p"),
      || panic!("context must not be called"),
      &IntOptions::default(),
    );
    assert_eq!(r.unwrap(), 1);
  }

  #[test]
  fn any_returns_same_value() {
    let mut runtime = runtime();
    let scope = &mut runtime.handle_scope();
    let value = eval(scope, "({ a: 1 })");
    let out = <v8::Local<v8::Value>>::convert(
      scope,
      value,
      Cow::Borrowed("p"),
      ctx,
      &(),
    )
    .unwrap();
    assert!(out.strict_equals(value));
  }

  #[test]
  fn bool_conversion() {
    let mut runtime = runtime();
    let scope = &mut runtime.handle_scope();
    let cases = [
      ("''", false),
      ("'false'", true),
      ("0n", false),
      ("NaN", false),
      ("({})", true),
      ("Symbol()", true),
      ("undefined", false),
    ];
    for (js, expected) in cases {
      assert_eq!(conv::<bool>(scope, js).unwrap(), expected, "{js}");
    }
  }

  #[test]
  fn integer_defaults() {
    let mut runtime = runtime();
    let scope = &mut runtime.handle_scope();
    assert_eq!(conv::<u8>(scope, "256").unwrap(), 0);
    assert_eq!(conv::<u8>(scope, "-1").unwrap(), 255);
    assert_eq!(conv::<u8>(scope, "257.9").unwrap(), 1);
    assert_eq!(conv::<u8>(scope, "'12'").unwrap(), 12);
    assert_eq!(conv::<u8>(scope, "'abc'").unwrap(), 0);
    assert_eq!(conv::<u8>(scope, "null").unwrap(), 0);
    assert_eq!(conv::<u8>(scope, "undefined").unwrap(), 0);
    assert_eq!(conv::<u8>(scope, "true").unwrap(), 1);
    assert_eq!(conv::<u8>(scope, "NaN").unwrap(), 0);
    assert_eq!(conv::<u8>(scope, "Infinity").unwrap(), 0);
    assert_eq!(conv::<u8>(scope, "-0").unwrap(), 0);
    assert_eq!(conv::<i8>(scope, "128").unwrap(), -128);
    assert_eq!(conv::<i32>(scope, "2 ** 31").unwrap(), -2147483648);
    assert_eq!(conv::<u32>(scope, "-1").unwrap(), 4294967295);
    assert_eq!(conv::<u64>(scope, "-1").unwrap(), u64::MAX);
    assert_eq!(conv::<u64>(scope, "1e20").unwrap(), 7766279631452241920);
    assert_eq!(conv::<u64>(scope, "2 ** 64 + 4096").unwrap(), 4096);
    assert_eq!(conv::<i64>(scope, "2 ** 63").unwrap(), i64::MIN);
  }

  #[test]
  fn integer_symbol_and_bigint_are_errors() {
    let mut runtime = runtime();
    let scope = &mut runtime.handle_scope();
    let err = conv::<u8>(scope, "Symbol()").unwrap_err();
    assert_eq!(
      err.to_string(),
      "Failed to execute 'op': Argument 1 cannot be converted to a number."
    );
    assert!(conv::<u8>(scope, "1n").is_err());
  }

  #[test]
  fn integer_enforce_range() {
    let mut runtime = runtime();
    let scope = &mut runtime.handle_scope();
    let enforce = int_opts(false, true);
    assert_eq!(conv_with::<u8>(scope, "255.9", enforce).unwrap(), 255);
    assert_eq!(conv_with::<u8>(scope, "-0.9", enforce).unwrap(), 0);
    assert!(conv_with::<u8>(scope, "256", enforce).is_err());
    assert!(conv_with::<u8>(scope, "-1", enforce).is_err());
    assert!(conv_with::<u8>(scope, "NaN", enforce).is_err());
    assert!(conv_with::<u8>(scope, "Infinity", enforce).is_err());
    assert_eq!(
      conv_with::<u64>(scope, "2 ** 53 - 1", enforce).unwrap(),
      9007199254740991
    );
    assert!(conv_with::<u64>(scope, "2 ** 53", enforce).is_err());
    let both = int_opts(true, true);
    assert!(conv_with::<u8>(scope, "256", both).is_err());
  }

  #[test]
  fn integer_clamp() {
    let mut runtime = runtime();
    let scope = &mut runtime.handle_scope();
    let clamp = int_opts(true, false);
    assert_eq!(conv_with::<u8>(scope, "300", clamp).unwrap(), 255);
    assert_eq!(conv_with::<u8>(scope, "-5", clamp).unwrap(), 0);
    assert_eq!(conv_with::<u8>(scope, "2.5", clamp).unwrap(), 2);
    assert_eq!(conv_with::<u8>(scope, "3.5", clamp).unwrap(), 4);
    assert_eq!(conv_with::<u8>(scope, "-0.5", clamp).unwrap(), 0);
    assert_eq!(conv_with::<u8>(scope, "Infinity", clamp).unwrap(), 255);
    assert_eq!(conv_with::<u8>(scope, "NaN", clamp).unwrap(), 0);
    assert_eq!(
      conv_with::<i64>(scope, "2 ** 60", clamp).unwrap(),
      9007199254740991
    );
  }

  #[test]
  fn float_conversions() {
    let mut runtime = runtime();
    let scope = &mut runtime.handle_scope();
    assert!(conv::<f64>(scope, "undefined").is_err());
    assert_eq!(conv::<f64>(scope, "null").unwrap(), 0.0);
    assert_eq!(conv::<f64>(scope, "'1.5'").unwrap(), 1.5);
    assert!(conv::<f32>(scope, "1e40").is_err());
    assert_eq!(
      conv::<UnrestrictedFloat>(scope, "1e40").unwrap().0,
      f32::INFINITY
    );
    assert!(conv::<UnrestrictedDouble>(scope, "NaN").unwrap().0.is_nan());
    let neg_zero = conv::<UnrestrictedDouble>(scope, "-0").unwrap().0;
    assert!(neg_zero == 0.0 && neg_zero.is_sign_negative());
    assert!(conv::<f64>(scope, "1n").is_err());
  }

  #[test]
  fn string_conversions() {
    let mut runtime = runtime();
    let scope = &mut runtime.handle_scope();
    assert_eq!(conv::<String>(scope, "undefined").unwrap(), "undefined");
    assert_eq!(conv::<String>(scope, "null").unwrap(), "null");
    assert_eq!(conv::<String>(scope, "123").unwrap(), "123");
    assert_eq!(conv::<String>(scope, "'héllo😀'").unwrap(), "héllo😀");
    assert!(conv::<String>(scope, "Symbol()").is_err());
    assert_eq!(
      conv::<String>(scope, "({ toString() { return 'x' } })").unwrap(),
      "x"
    );
    let empty = StringOptions {
      treat_null_as_empty_string: true,
    };
    assert_eq!(conv_with::<String>(scope, "null", empty).unwrap(), "");
    assert!(conv::<String>(scope, "({ toString() { throw 1 } })").is_err());
  }

  #[test]
  fn byte_string_conversions() {
    let mut runtime = runtime();
    let scope = &mut runtime.handle_scope();
    assert_eq!(conv::<ByteString>(scope, "'ÿ'").unwrap().0, "\u{ff}");
    assert!(conv::<ByteString>(scope, "'Ā'").is_err());
    assert!(conv::<ByteString>(scope, "'😀'").is_err());
    assert_eq!(conv::<ByteString>(scope, "null").unwrap().0, "null");
    let empty = StringOptions {
      treat_null_as_empty_string: true,
    };
    assert_eq!(conv_with::<ByteString>(scope, "null", empty).unwrap().0, "");
  }

  #[test]
  fn bigint_conversions() {
    let mut runtime = runtime();
    let scope = &mut runtime.handle_scope();
    let zero = conv::<BigInt>(scope, "0n").unwrap();
    assert!(!zero.sign);
    let neg = conv::<BigInt>(scope, "-1n").unwrap();
    assert_eq!(
      neg,
      BigInt {
        sign: true,
        words: vec![1]
      }
    );
    assert_eq!(
      conv::<BigInt>(scope, "2n ** 64n").unwrap().words,
      vec![0, 1]
    );
    assert_eq!(
      conv::<BigInt>(scope, "-(2n ** 128n)").unwrap(),
      BigInt {
        sign: true,
        words: vec![0, 0, 1]
      }
    );
    assert_eq!(conv::<BigInt>(scope, "true").unwrap().words, vec![1]);
    assert_eq!(conv::<BigInt>(scope, "'10'").unwrap().words, vec![10]);
    assert!(conv::<BigInt>(scope, "5").is_err());
    assert!(conv::<BigInt>(scope, "undefined").is_err());
    assert!(conv::<BigInt>(scope, "null").is_err());
    assert!(conv::<BigInt>(scope, "Symbol()").is_err());
    assert!(conv::<BigInt>(scope, "'abc'").is_err());
  }

  #[test]
  fn option_conversions() {
    let mut runtime = runtime();
    let scope = &mut runtime.handle_scope();
    assert_eq!(conv::<Option<String>>(scope, "null").unwrap(), None);
    assert_eq!(conv::<Option<String>>(scope, "undefined").unwrap(), None);
    assert_eq!(
      conv::<Option<String>>(scope, "'null'").unwrap(),
      Some("null".to_string())
    );
    assert_eq!(conv::<Option<u8>>(scope, "0").unwrap(), Some(0));
  }

  #[test]
  fn vec_conversions() {
    let mut runtime = runtime();
    let scope = &mut runtime.handle_scope();
    assert!(conv::<Vec<u8>>(scope, "({ length: 2 })").is_err());
    assert!(conv::<Vec<u8>>(scope, "'abc'").is_err());
    assert!(conv::<Vec<u8>>(scope, "null").is_err());
    assert_eq!(
      conv::<Vec<u8>>(scope, "new Set([1, 2])").unwrap(),
      vec![1, 2]
    );
    assert_eq!(
      conv::<Vec<u8>>(scope, "(function* () { yield 1; yield 2; })()").unwrap(),
      vec![1, 2]
    );
    assert_eq!(conv::<Vec<u8>>(scope, "[1,,3]").unwrap(), vec![1, 0, 3]);
    assert_eq!(
      conv::<Vec<Vec<u8>>>(scope, "[[1], [2, 3]]").unwrap(),
      vec![vec![1], vec![2, 3]]
    );
    assert!(conv::<Vec<u8>>(
      scope,
      "({ [Symbol.iterator]() { return { next() { throw new Error('x') } } } })"
    )
    .is_err());
    let err = conv::<Vec<u8>>(scope, "[1, Symbol()]").unwrap_err();
    assert_eq!(
      err.to_string(),
      "Failed to execute 'op': Argument 1 element 1 cannot be converted to a number."
    );
  }

  #[test]
  fn hashmap_conversions() {
    let mut runtime = runtime();
    let scope = &mut runtime.handle_scope();
    assert!(conv::<HashMap<String, u8>>(scope, "null").is_err());
    assert!(conv::<HashMap<String, u8>>(scope, "undefined").is_err());
    let map = conv::<HashMap<String, u8>>(scope, "[7, 8]").unwrap();
    assert_eq!(map.get("0"), Some(&7));
    assert_eq!(map.get("1"), Some(&8));
    assert_eq!(map.len(), 2);
    let map =
      conv::<HashMap<String, u8>>(scope, "({ a: 1, get b() { return 2 } })")
        .unwrap();
    assert_eq!(map.get("a"), Some(&1));
    assert_eq!(map.get("b"), Some(&2));
    let map = conv::<HashMap<String, u8>>(
      scope,
      "(() => { const o = {}; Object.defineProperty(o, 'hidden', { value: 1, enumerable: false }); return o })()",
    )
    .unwrap();
    assert!(map.is_empty());
    assert!(conv::<HashMap<String, u8>>(
      scope,
      "({ get a() { throw new Error('x') } })"
    )
    .is_err());
    assert!(
      conv::<HashMap<String, u8>>(scope, "({ [Symbol('s')]: 1 })").is_err()
    );
  }

  #[test]
  fn thrown_js_is_error_not_panic() {
    let mut runtime = runtime();
    let scope = &mut runtime.handle_scope();
    assert!(
      conv::<u32>(scope, "({ valueOf() { throw new Error('boom') } })")
        .is_err()
    );
    assert!(conv::<String>(scope, "({ toString() { throw 1 } })").is_err());
  }

  #[test]
  fn derive_dictionary() {
    let mut runtime = runtime();
    let scope = &mut runtime.handle_scope();
    let dict = conv::<Dict>(
      scope,
      "({ fooBar: 3, clamped: 300, opt: 'a', text: null })",
    );
    assert_eq!(
      dict.unwrap(),
      Dict {
        foo_bar: 3,
        count: 7,
        opt: Some("a".to_string()),
        clamped: 255,
        text: String::new(),
      }
    );
    let dict =
      conv::<Dict>(scope, "({ fooBar: 3, clamped: 1, opt: null })").unwrap();
    assert_eq!(dict.opt, None);
    assert_eq!(dict.text, "unset");
    let err = conv::<Dict>(scope, "({ clamped: 1 })").unwrap_err();
    assert!(err.to_string().contains("fooBar"), "{err}");
    let err = conv::<Dict>(scope, "({ fooBar: 1 })").unwrap_err();
    assert!(err.to_string().contains("clamped"), "{err}");
    assert!(conv::<Dict>(scope, "5").is_err());
    assert!(conv::<Dict>(scope, "null").is_err());
  }

  #[test]
  fn derive_dictionary_read_order_and_getter_errors() {
    let mut runtime = runtime();
    let scope = &mut runtime.handle_scope();
    let js = r"({
      get fooBar() { globalThis.order = (globalThis.order || '') + 'f'; return 1 },
      get clamped() { globalThis.order = (globalThis.order || '') + 'c'; return 1 },
    })";
    conv::<Dict>(scope, js).unwrap();
    let order = eval(scope, "globalThis.order");
    assert_eq!(order.to_rust_string_lossy(scope), "cf");
    assert!(conv::<Dict>(
      scope,
      "({ get fooBar() { throw new Error('x') }, clamped: 1 })"
    )
    .is_err());
  }

  #[test]
  fn derive_enum() {
    let mut runtime = runtime();
    let scope = &mut runtime.handle_scope();
    assert_eq!(conv::<Kind>(scope, "'foo-bar'").unwrap(), Kind::FooBar);
    assert_eq!(conv::<Kind>(scope, "'other'").unwrap(), Kind::Plain);
    assert_eq!(
      conv::<Kind>(scope, "({ toString() { return 'foo-bar' } })").unwrap(),
      Kind::FooBar
    );
    assert!(conv::<Kind>(scope, "'Foo-Bar'").is_err());
    assert!(conv::<Kind>(scope, "''").is_err());
    assert!(conv::<Kind>(scope, "Symbol()").is_err());
    let err = conv::<Kind>(scope, "'nope'").unwrap_err();
    assert!(err.to_string().contains("\"nope\""), "{err}");
    assert!(err.to_string().contains("Kind"), "{err}");
  }
}
