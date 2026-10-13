// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.

//! Converts JavaScript values to Rust types using the WebIDL "JavaScript to
//! IDL" type mapping: <https://webidl.spec.whatwg.org/#js-type-mapping>.
//!
//! Use [`WebIdlConverter::convert`] directly, `#[webidl]` on an `#[op2]`
//! argument, or `#[derive(WebIDL)]` for dictionaries and enums.

use std::borrow::Cow;
use std::collections::HashMap;
use std::fmt;
use std::ops::Deref;

use crate::v8;

/// An error produced while converting a JavaScript value to a Rust type.
///
/// Displays as `"{prefix}: {context} {kind}"`, e.g.
/// `Failed to execute 'op_foo': Argument 1 is not a finite number`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebIdlError {
  /// The outer message, e.g. `Failed to execute 'op_foo'`.
  pub prefix: Cow<'static, str>,
  /// Where the value came from, e.g. `Argument 1` or `'fooBar' of 'Name' (Argument 1)`.
  pub context: Cow<'static, str>,
  /// What was wrong with the value.
  pub kind: WebIdlErrorKind,
}

impl WebIdlError {
  pub fn new(
    prefix: Cow<'static, str>,
    context: Cow<'static, str>,
    kind: WebIdlErrorKind,
  ) -> Self {
    Self {
      prefix,
      context,
      kind,
    }
  }
}

impl fmt::Display for WebIdlError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "{}: {} {}", self.prefix, self.context, self.kind)
  }
}

impl std::error::Error for WebIdlError {}

/// The problem with a value, displayed after the context.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WebIdlErrorKind {
  /// `can not be converted to {0}`
  ConvertToType(Cow<'static, str>),
  /// `is not a finite number`
  NotFinite,
  /// `is outside the accepted range of {lower} to {upper}`
  OutOfRange { lower: i128, upper: i128 },
  /// `is not a valid ByteString`
  InvalidByteString,
  /// `is a Symbol and can not be converted to a string`
  Symbol,
  /// `can not be converted to '{dictionary}' because '{key}' is required in '{dictionary}'`
  RequiredMember {
    dictionary: &'static str,
    key: &'static str,
  },
  /// `can not be converted to '{enum_name}' because '{value}' is not a valid enum value`
  InvalidEnumValue {
    enum_name: &'static str,
    value: String,
  },
  /// `threw an exception: {0}`
  JsException(String),
}

impl fmt::Display for WebIdlErrorKind {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      WebIdlErrorKind::ConvertToType(ty) => {
        write!(f, "can not be converted to {ty}")
      }
      WebIdlErrorKind::NotFinite => write!(f, "is not a finite number"),
      WebIdlErrorKind::OutOfRange { lower, upper } => {
        write!(f, "is outside the accepted range of {lower} to {upper}")
      }
      WebIdlErrorKind::InvalidByteString => {
        write!(f, "is not a valid ByteString")
      }
      WebIdlErrorKind::Symbol => {
        write!(f, "is a Symbol and can not be converted to a string")
      }
      WebIdlErrorKind::RequiredMember { dictionary, key } => write!(
        f,
        "can not be converted to '{dictionary}' because '{key}' is required in '{dictionary}'"
      ),
      WebIdlErrorKind::InvalidEnumValue { enum_name, value } => write!(
        f,
        "can not be converted to '{enum_name}' because '{value}' is not a valid enum value"
      ),
      WebIdlErrorKind::JsException(message) => {
        write!(f, "threw an exception: {message}")
      }
    }
  }
}

/// Converts a JavaScript value to `Self` following the WebIDL JS-to-IDL mapping.
///
/// Conversion never panics and never leaves a V8 exception pending: JS code that
/// throws (e.g. a `valueOf` or getter) is turned into a [`WebIdlError`].
pub trait WebIdlConverter<'a>: Sized {
  /// Conversion options, e.g. [`IntOptions`] for integers.
  type Options: Default;

  /// Converts `value`. `prefix` is the outer message (e.g. `Failed to execute
  /// 'op_foo'`) and `context` describes where the value came from (e.g.
  /// `Argument 1`).
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

fn error<C>(prefix: &str, context: &C, kind: WebIdlErrorKind) -> WebIdlError
where
  C: Fn() -> Cow<'static, str>,
{
  WebIdlError::new(Cow::Owned(prefix.to_owned()), context(), kind)
}

/// Builds the error for a JS exception caught by `tc`. Also used when a
/// JS operation fails without an exception, which should not happen.
fn exception_error<C>(
  prefix: &str,
  context: &C,
  tc: &mut v8::TryCatch<v8::HandleScope>,
) -> WebIdlError
where
  C: Fn() -> Cow<'static, str>,
{
  let message = match tc.exception() {
    Some(exception) => exception.to_rust_string_lossy(tc),
    None => "an unknown error".to_string(),
  };
  error(prefix, context, WebIdlErrorKind::JsException(message))
}

/// Creates a JS string value. Returns `None` only if V8 cannot allocate it.
fn js_string<'a>(
  scope: &mut v8::HandleScope<'a>,
  s: &str,
) -> Option<v8::Local<'a, v8::Value>> {
  v8::String::new(scope, s).map(Into::into)
}

/// ToNumber, with JS exceptions converted to errors.
fn to_number<'a, C>(
  scope: &mut v8::HandleScope<'a>,
  value: v8::Local<'a, v8::Value>,
  prefix: &str,
  context: &C,
) -> Result<f64, WebIdlError>
where
  C: Fn() -> Cow<'static, str>,
{
  let tc = &mut v8::TryCatch::new(&mut *scope);
  match value.to_number(tc) {
    Some(number) => Ok(number.value()),
    None => Err(exception_error(prefix, context, tc)),
  }
}

/// `any`: the value itself.
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

/// `boolean`: ToBoolean. Never fails.
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

/// Options for integer conversions (WebIDL `[Clamp]` and `[EnforceRange]`).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct IntOptions {
  /// `[Clamp]`: clamp out-of-range values to the range of the type.
  pub clamp: bool,
  /// `[EnforceRange]`: error on out-of-range values. Wins over `clamp`.
  pub enforce_range: bool,
}

/// The largest integer that is exactly representable as a JS Number.
const MAX_SAFE_INTEGER: i128 = (1 << 53) - 1;

/// Rounds to the nearest integer, choosing the even one on ties.
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

/// ConvertToInt from the WebIDL spec, for a `bits`-wide integer type.
fn convert_to_int(
  n: f64,
  bits: u32,
  signed: bool,
  options: IntOptions,
) -> Result<i128, WebIdlErrorKind> {
  let (lower, upper): (i128, i128) = if bits == 64 {
    if signed {
      (-MAX_SAFE_INTEGER, MAX_SAFE_INTEGER)
    } else {
      (0, MAX_SAFE_INTEGER)
    }
  } else if signed {
    (-(1i128 << (bits - 1)), (1i128 << (bits - 1)) - 1)
  } else {
    (0, (1i128 << bits) - 1)
  };

  if options.enforce_range {
    if !n.is_finite() {
      return Err(WebIdlErrorKind::NotFinite);
    }
    let x = n.trunc();
    if !(lower as f64..=upper as f64).contains(&x) {
      return Err(WebIdlErrorKind::OutOfRange { lower, upper });
    }
    return Ok(x as i128);
  }

  if options.clamp {
    if n.is_nan() {
      return Ok(0);
    }
    let x = n.clamp(lower as f64, upper as f64);
    return Ok(round_half_to_even(x) as i128);
  }

  // Default: truncate, then wrap modulo 2^bits into the type's range.
  if !n.is_finite() || n == 0.0 {
    return Ok(0);
  }
  let modulus = 1i128 << bits;
  // `n.trunc() % 2^bits` is exact in f64, and the result fits in i128.
  let rem = (n.trunc() % ((1u128 << bits) as f64)) as i128;
  let rem = if rem < 0 { rem + modulus } else { rem };
  if signed && rem >= (1i128 << (bits - 1)) {
    Ok(rem - modulus)
  } else {
    Ok(rem)
  }
}

macro_rules! impl_int {
  ($($ty:ty => ($bits:expr, $signed:expr)),* $(,)?) => {
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
          let n = to_number(scope, value, &prefix, &context)?;
          match convert_to_int(n, $bits, $signed, *options) {
            Ok(v) => Ok(v as $ty),
            Err(kind) => Err(error(&prefix, &context, kind)),
          }
        }
      }
    )*
  };
}

impl_int! {
  i8 => (8, true),
  u8 => (8, false),
  i16 => (16, true),
  u16 => (16, false),
  i32 => (32, true),
  u32 => (32, false),
  i64 => (64, true),
  u64 => (64, false),
}

/// `double`: ToNumber, rejecting NaN and infinities.
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
    let n = to_number(scope, value, &prefix, &context)?;
    if n.is_finite() {
      Ok(n)
    } else {
      Err(error(&prefix, &context, WebIdlErrorKind::NotFinite))
    }
  }
}

/// `float`: like `double`, but also rejects values that overflow f32.
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
    let n = to_number(scope, value, &prefix, &context)?;
    let f = n as f32;
    if f.is_finite() {
      Ok(f)
    } else {
      Err(error(&prefix, &context, WebIdlErrorKind::NotFinite))
    }
  }
}

/// `unrestricted float`: accepts NaN and infinities; overflow becomes infinity.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
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
    _options: &Self::Options,
  ) -> Result<Self, WebIdlError>
  where
    C: Fn() -> Cow<'static, str>,
  {
    Ok(UnrestrictedFloat(
      to_number(scope, value, &prefix, &context)? as f32,
    ))
  }
}

/// `unrestricted double`: accepts NaN and infinities.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
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

/// Options for string conversions.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct StringOptions {
  /// `[LegacyNullToEmptyString]`: convert `null` to `""` instead of `"null"`.
  pub treat_null_as_empty_string: bool,
}

/// `DOMString`: ToString, rejecting Symbols. Lone surrogates become U+FFFD.
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
      return Err(error(&prefix, &context, WebIdlErrorKind::Symbol));
    }
    if options.treat_null_as_empty_string && value.is_null() {
      return Ok(String::new());
    }
    let tc = &mut v8::TryCatch::new(&mut *scope);
    match value.to_string(tc) {
      Some(string) => Ok(string.to_rust_string_lossy(tc)),
      None => Err(exception_error(&prefix, &context, tc)),
    }
  }
}

/// `ByteString`: a DOMString whose code units are all at most 0xFF.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
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
    // Lone surrogates were replaced with U+FFFD, which is also rejected here.
    if string.chars().any(|c| c as u32 > 0xFF) {
      return Err(error(&prefix, &context, WebIdlErrorKind::InvalidByteString));
    }
    Ok(ByteString(string))
  }
}

/// `bigint`: ToBigInt. `words` are the magnitude in little-endian 64-bit words
/// with no high zero words, so zero has empty `words`. `sign` is true for
/// negative values.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct BigInt {
  pub sign: bool,
  pub words: Vec<u64>,
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
    let tc = &mut v8::TryCatch::new(&mut *scope);
    let Some(bigint) = value.to_big_int(tc) else {
      return Err(exception_error(&prefix, &context, tc));
    };
    let mut buffer = vec![0u64; bigint.word_count().max(1)];
    let (sign, magnitude) = bigint.to_words_array(&mut buffer);
    Ok(BigInt {
      sign,
      words: magnitude.to_vec(),
    })
  }
}

/// `T?`: `undefined` and `null` become `None`.
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

/// Calls the iterator's `return` method, if it is callable, so an iterator that
/// is abandoned early can release its resources. Exceptions it throws are
/// ignored because the element error is the one reported.
fn close_iterator(
  tc: &mut v8::TryCatch<v8::HandleScope>,
  iterator: v8::Local<v8::Object>,
) {
  let Some(return_key) = js_string(tc, "return") else {
    return;
  };
  let Some(method) = iterator.get(tc, return_key) else {
    return;
  };
  if let Ok(method) = v8::Local::<v8::Function>::try_from(method) {
    method.call(tc, iterator.into(), &[]);
  }
}

/// `sequence<T>`: any iterable object, converted by walking its iterator.
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
    let not_a_sequence = WebIdlErrorKind::ConvertToType("a sequence".into());
    let Ok(object) = v8::Local::<v8::Object>::try_from(value) else {
      return Err(error(&prefix, &context, not_a_sequence));
    };

    let tc = &mut v8::TryCatch::new(&mut *scope);
    let iterator_key = v8::Symbol::get_iterator(tc).into();
    let Some(method) = object.get(tc, iterator_key) else {
      return Err(exception_error(&prefix, &context, tc));
    };
    let Ok(method) = v8::Local::<v8::Function>::try_from(method) else {
      return Err(error(&prefix, &context, not_a_sequence));
    };
    let Some(iterator) = method.call(tc, value, &[]) else {
      return Err(exception_error(&prefix, &context, tc));
    };
    let Ok(iterator) = v8::Local::<v8::Object>::try_from(iterator) else {
      return Err(error(&prefix, &context, not_a_sequence));
    };

    let (Some(next_key), Some(done_key), Some(value_key)) = (
      js_string(tc, "next"),
      js_string(tc, "done"),
      js_string(tc, "value"),
    ) else {
      return Err(exception_error(&prefix, &context, tc));
    };
    let Some(next) = iterator.get(tc, next_key) else {
      return Err(exception_error(&prefix, &context, tc));
    };
    let Ok(next) = v8::Local::<v8::Function>::try_from(next) else {
      return Err(error(&prefix, &context, not_a_sequence));
    };

    let mut elements = Vec::new();
    loop {
      let Some(result) = next.call(tc, iterator.into(), &[]) else {
        return Err(exception_error(&prefix, &context, tc));
      };
      let Ok(result) = v8::Local::<v8::Object>::try_from(result) else {
        return Err(error(&prefix, &context, not_a_sequence));
      };
      let Some(done) = result.get(tc, done_key) else {
        return Err(exception_error(&prefix, &context, tc));
      };
      if done.to_boolean(tc).is_true() {
        break;
      }
      let Some(item) = result.get(tc, value_key) else {
        return Err(exception_error(&prefix, &context, tc));
      };
      let index = elements.len();
      let element_context = || -> Cow<'static, str> {
        format!("{}, index {index}", context()).into()
      };
      match T::convert(tc, item, prefix.clone(), element_context, options) {
        Ok(element) => elements.push(element),
        Err(err) => {
          close_iterator(tc, iterator);
          return Err(err);
        }
      }
    }
    Ok(elements)
  }
}

/// `record<DOMString, T>`: the own enumerable string-keyed properties of an object.
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
      return Err(error(
        &prefix,
        &context,
        WebIdlErrorKind::ConvertToType("a record".into()),
      ));
    };

    let tc = &mut v8::TryCatch::new(&mut *scope);
    // Symbol keys are kept so that converting them to a string reports an error,
    // as the spec does.
    let key_args = v8::GetPropertyNamesArgs {
      mode: v8::KeyCollectionMode::OwnOnly,
      property_filter: v8::PropertyFilter::ONLY_ENUMERABLE,
      index_filter: v8::IndexFilter::IncludeIndices,
      key_conversion: v8::KeyConversionMode::ConvertToString,
    };
    let Some(keys) = object.get_own_property_names(tc, key_args) else {
      return Err(exception_error(&prefix, &context, tc));
    };

    let mut record = HashMap::new();
    for i in 0..keys.length() {
      let Some(key) = keys.get_index(tc, i) else {
        return Err(exception_error(&prefix, &context, tc));
      };
      let key = <String as WebIdlConverter<'a>>::convert(
        tc,
        key,
        prefix.clone(),
        &context,
        &StringOptions::default(),
      )?;
      let Some(key_value) = js_string(tc, &key) else {
        return Err(exception_error(&prefix, &context, tc));
      };
      let Some(member) = object.get(tc, key_value) else {
        return Err(exception_error(&prefix, &context, tc));
      };
      let member_context =
        || -> Cow<'static, str> { format!("'{key}' ({})", context()).into() };
      let converted =
        T::convert(tc, member, prefix.clone(), member_context, options)?;
      record.insert(key, converted);
    }
    Ok(record)
  }
}

/// Converts a dictionary argument to an object. `undefined` and `null` are an
/// empty dictionary, so `None` is returned for them.
#[doc(hidden)]
pub fn dictionary_object<'a, C>(
  value: v8::Local<'a, v8::Value>,
  dictionary: &'static str,
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
    Err(_) => Err(error(
      prefix,
      context,
      WebIdlErrorKind::ConvertToType(format!("'{dictionary}'").into()),
    )),
  }
}

/// Reads one dictionary member with a single [[Get]]. Returns `None` if the
/// dictionary is empty or the member is missing or `undefined`.
#[doc(hidden)]
pub fn dictionary_member<'a, C>(
  scope: &mut v8::HandleScope<'a>,
  dictionary: Option<v8::Local<'a, v8::Object>>,
  key: &'static str,
  prefix: &str,
  context: &C,
) -> Result<Option<v8::Local<'a, v8::Value>>, WebIdlError>
where
  C: Fn() -> Cow<'static, str>,
{
  let Some(dictionary) = dictionary else {
    return Ok(None);
  };
  let tc = &mut v8::TryCatch::new(&mut *scope);
  let Some(key_value) = js_string(tc, key) else {
    return Err(exception_error(prefix, context, tc));
  };
  let Some(value) = dictionary.get(tc, key_value) else {
    return Err(exception_error(prefix, context, tc));
  };
  if value.is_undefined() {
    Ok(None)
  } else {
    Ok(Some(value))
  }
}

/// Builds the error for a required dictionary member that is missing.
#[doc(hidden)]
pub fn missing_dictionary_member<C>(
  prefix: &str,
  context: &C,
  dictionary: &'static str,
  key: &'static str,
) -> WebIdlError
where
  C: Fn() -> Cow<'static, str>,
{
  error(
    prefix,
    context,
    WebIdlErrorKind::RequiredMember { dictionary, key },
  )
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::JsRuntime;
  use crate::RuntimeOptions;
  use crate::WebIDL;

  /// Evaluates `code` in the runtime's context.
  fn eval<'a>(
    scope: &mut v8::HandleScope<'a>,
    code: &str,
  ) -> v8::Local<'a, v8::Value> {
    let source = v8::String::new(scope, code).unwrap();
    let script = v8::Script::compile(scope, source, None).unwrap();
    script.run(scope).unwrap()
  }

  /// Converts the result of evaluating `code` as `T`.
  fn convert<'a, T>(
    scope: &mut v8::HandleScope<'a>,
    code: &str,
    options: T::Options,
  ) -> Result<T, WebIdlError>
  where
    T: WebIdlConverter<'a>,
  {
    let value = eval(scope, code);
    T::convert(
      scope,
      value,
      "Failed to execute 'op_test'".into(),
      || "Argument 1".into(),
      &options,
    )
  }

  /// Converts `code` and returns the error text, panicking if it converts.
  fn convert_err<'a, T>(
    scope: &mut v8::HandleScope<'a>,
    code: &str,
    options: <T as WebIdlConverter<'a>>::Options,
  ) -> String
  where
    T: WebIdlConverter<'a> + fmt::Debug,
  {
    match convert::<T>(scope, code, options) {
      Ok(value) => panic!("expected an error for {code}, got {value:?}"),
      Err(err) => err.to_string(),
    }
  }

  #[test]
  fn webidl_any_returns_value() {
    let mut runtime = JsRuntime::new(RuntimeOptions::default());
    let scope = &mut runtime.handle_scope();
    for code in ["1", "'x'", "null", "undefined"] {
      let expected = eval(scope, code);
      let got = convert::<v8::Local<v8::Value>>(scope, code, ()).expect("any");
      assert!(got.strict_equals(expected), "any({code}) changed the value");
    }
    let object = eval(scope, "globalThis.anyObject = {}; anyObject");
    let got = <v8::Local<v8::Value> as WebIdlConverter>::convert(
      scope,
      object,
      "P".into(),
      || "C".into(),
      &(),
    )
    .expect("any");
    assert!(got.strict_equals(object));
  }

  #[test]
  fn webidl_bool_to_boolean() {
    let mut runtime = JsRuntime::new(RuntimeOptions::default());
    let scope = &mut runtime.handle_scope();
    for (code, expected) in [
      ("0", false),
      ("-0", false),
      ("NaN", false),
      ("''", false),
      ("null", false),
      ("undefined", false),
      ("0n", false),
      ("'false'", true),
      ("({})", true),
      ("[]", true),
      ("1n", true),
      ("Symbol()", true),
    ] {
      assert_eq!(
        convert::<bool>(scope, code, ()).unwrap(),
        expected,
        "{code}"
      );
    }
  }

  #[test]
  fn webidl_int_default_wraps() {
    let mut runtime = JsRuntime::new(RuntimeOptions::default());
    let scope = &mut runtime.handle_scope();
    let o = IntOptions::default();
    assert_eq!(convert::<u8>(scope, "256", o).unwrap(), 0);
    assert_eq!(convert::<u8>(scope, "-1", o).unwrap(), 255);
    assert_eq!(convert::<u8>(scope, "257.9", o).unwrap(), 1);
    assert_eq!(convert::<i8>(scope, "128", o).unwrap(), -128);
    assert_eq!(convert::<i8>(scope, "255", o).unwrap(), -1);
    assert_eq!(convert::<u32>(scope, "2**32+5", o).unwrap(), 5);
    assert_eq!(convert::<u32>(scope, "-1", o).unwrap(), 4294967295);
    assert_eq!(
      convert::<u64>(scope, "-1", o).unwrap(),
      18446744073709551615
    );
    assert_eq!(convert::<i64>(scope, "2**63", o).unwrap(), i64::MIN);
    assert_eq!(convert::<i64>(scope, "NaN", o).unwrap(), 0);
    assert_eq!(convert::<i64>(scope, "-0", o).unwrap(), 0);
    assert_eq!(convert::<i64>(scope, "Infinity", o).unwrap(), 0);
    assert_eq!(convert::<u16>(scope, "'12'", o).unwrap(), 12);
    assert_eq!(convert::<u16>(scope, "null", o).unwrap(), 0);
    assert_eq!(convert::<u16>(scope, "undefined", o).unwrap(), 0);
  }

  #[test]
  fn webidl_int_clamp_and_enforce_range() {
    let mut runtime = JsRuntime::new(RuntimeOptions::default());
    let scope = &mut runtime.handle_scope();
    let clamp = IntOptions {
      clamp: true,
      enforce_range: false,
    };
    assert_eq!(convert::<u8>(scope, "300", clamp).unwrap(), 255);
    assert_eq!(convert::<u8>(scope, "-5", clamp).unwrap(), 0);
    assert_eq!(convert::<u8>(scope, "NaN", clamp).unwrap(), 0);
    assert_eq!(convert::<u8>(scope, "2.5", clamp).unwrap(), 2);
    assert_eq!(convert::<u8>(scope, "3.5", clamp).unwrap(), 4);
    assert_eq!(convert::<i8>(scope, "-2.5", clamp).unwrap(), -2);
    assert_eq!(convert::<i8>(scope, "-Infinity", clamp).unwrap(), -128);
    assert_eq!(
      convert::<u64>(scope, "2**60", clamp).unwrap(),
      9007199254740991
    );
    let enforce = IntOptions {
      clamp: false,
      enforce_range: true,
    };
    assert_eq!(convert::<u8>(scope, "255.9", enforce).unwrap(), 255);
    assert_eq!(
      convert::<i64>(scope, "-9007199254740991", enforce).unwrap(),
      -9007199254740991
    );
    let both = IntOptions {
      clamp: true,
      enforce_range: true,
    };
    assert!(convert::<u8>(scope, "300", both).is_err());
  }

  #[test]
  fn webidl_int_hostile_inputs() {
    let mut runtime = JsRuntime::new(RuntimeOptions::default());
    let scope = &mut runtime.handle_scope();
    let enforce = IntOptions {
      clamp: false,
      enforce_range: true,
    };
    assert_eq!(
      convert_err::<u8>(scope, "NaN", enforce),
      "Failed to execute 'op_test': Argument 1 is not a finite number"
    );
    assert_eq!(
      convert_err::<u8>(scope, "Infinity", enforce),
      "Failed to execute 'op_test': Argument 1 is not a finite number"
    );
    assert_eq!(
      convert_err::<u8>(scope,"300", enforce),
      "Failed to execute 'op_test': Argument 1 is outside the accepted range of 0 to 255"
    );
    assert_eq!(
      convert_err::<u8>(scope,"-1", enforce),
      "Failed to execute 'op_test': Argument 1 is outside the accepted range of 0 to 255"
    );
    assert_eq!(
      convert_err::<i64>(scope,"2**53", enforce),
      "Failed to execute 'op_test': Argument 1 is outside the accepted range of -9007199254740991 to 9007199254740991"
    );
    assert_eq!(
      convert_err::<u32>(scope,"Symbol('x')", IntOptions::default()),
      "Failed to execute 'op_test': Argument 1 threw an exception: TypeError: Cannot convert a Symbol value to a number"
    );
    assert_eq!(
      convert_err::<u32>(scope,"1n", IntOptions::default()),
      "Failed to execute 'op_test': Argument 1 threw an exception: TypeError: Cannot convert a BigInt value to a number"
    );
    assert_eq!(
      convert_err::<i32>(
        scope,
        "({ valueOf() { throw new Error('boom') } })",
        IntOptions::default()
      ),
      "Failed to execute 'op_test': Argument 1 threw an exception: Error: boom"
    );
  }

  #[test]
  fn webidl_float_conversions() {
    let mut runtime = JsRuntime::new(RuntimeOptions::default());
    let scope = &mut runtime.handle_scope();
    assert_eq!(convert::<f64>(scope, "null", ()).unwrap(), 0.0);
    assert_eq!(convert::<f64>(scope, "'1.5'", ()).unwrap(), 1.5);
    assert_eq!(convert::<f32>(scope, "'1.5'", ()).unwrap(), 1.5);
    assert_eq!(
      convert::<f32>(scope, "3.4028234663852886e38", ()).unwrap(),
      f32::MAX
    );
    assert_eq!(
      convert_err::<f64>(scope, "NaN", ()),
      "Failed to execute 'op_test': Argument 1 is not a finite number"
    );
    assert_eq!(
      convert_err::<f64>(scope, "Infinity", ()),
      "Failed to execute 'op_test': Argument 1 is not a finite number"
    );
    assert_eq!(
      convert_err::<f32>(scope, "1e40", ()),
      "Failed to execute 'op_test': Argument 1 is not a finite number"
    );
    assert_eq!(
      convert_err::<f64>(scope, "undefined", ()),
      "Failed to execute 'op_test': Argument 1 is not a finite number"
    );
  }

  #[test]
  fn webidl_unrestricted_float_accepts_non_finite() {
    let mut runtime = JsRuntime::new(RuntimeOptions::default());
    let scope = &mut runtime.handle_scope();
    let f = convert::<UnrestrictedFloat>(scope, "1e40", ()).unwrap();
    assert_eq!(*f, f32::INFINITY);
    let f = convert::<UnrestrictedFloat>(scope, "NaN", ()).unwrap();
    assert!(f.is_nan());
    let d = convert::<UnrestrictedDouble>(scope, "-Infinity", ()).unwrap();
    assert_eq!(*d, f64::NEG_INFINITY);
    let d = convert::<UnrestrictedDouble>(scope, "undefined", ()).unwrap();
    assert!(d.is_nan());
  }

  #[test]
  fn webidl_string_conversions() {
    let mut runtime = JsRuntime::new(RuntimeOptions::default());
    let scope = &mut runtime.handle_scope();
    let o = StringOptions::default();
    assert_eq!(convert::<String>(scope, "'é😀'", o).unwrap(), "é😀");
    assert_eq!(convert::<String>(scope, "null", o).unwrap(), "null");
    assert_eq!(
      convert::<String>(scope, "undefined", o).unwrap(),
      "undefined"
    );
    assert_eq!(convert::<String>(scope, "1", o).unwrap(), "1");
    assert_eq!(
      convert::<String>(scope, "'\\uD800'", o).unwrap(),
      "\u{FFFD}"
    );
    assert_eq!(
      convert::<String>(scope, "({ toString() { return 'obj' } })", o).unwrap(),
      "obj"
    );
    let treat = StringOptions {
      treat_null_as_empty_string: true,
    };
    assert_eq!(convert::<String>(scope, "null", treat).unwrap(), "");
    assert_eq!(
      convert_err::<String>(scope,"Symbol('s')", o),
      "Failed to execute 'op_test': Argument 1 is a Symbol and can not be converted to a string"
    );
  }

  #[test]
  fn webidl_bytestring_conversions() {
    let mut runtime = JsRuntime::new(RuntimeOptions::default());
    let scope = &mut runtime.handle_scope();
    let o = StringOptions::default();
    assert_eq!(convert::<ByteString>(scope, "'ÿ'", o).unwrap().0, "ÿ");
    assert_eq!(
      convert::<ByteString>(scope, "'\\u0000'", o).unwrap().0,
      "\u{0}"
    );
    assert_eq!(convert::<ByteString>(scope, "'ÿ'", o).unwrap().len(), 2);
    assert_eq!(
      convert_err::<ByteString>(scope, "'Ā'", o),
      "Failed to execute 'op_test': Argument 1 is not a valid ByteString"
    );
    assert_eq!(
      convert_err::<ByteString>(scope, "'😀'", o),
      "Failed to execute 'op_test': Argument 1 is not a valid ByteString"
    );
    assert_eq!(
      convert_err::<ByteString>(scope, "'\\uD800'", o),
      "Failed to execute 'op_test': Argument 1 is not a valid ByteString"
    );
  }

  #[test]
  fn webidl_bigint_conversions() {
    let mut runtime = JsRuntime::new(RuntimeOptions::default());
    let scope = &mut runtime.handle_scope();
    let b = convert::<BigInt>(scope, "-1n", ()).unwrap();
    assert!(b.sign);
    assert_eq!(b.words, vec![1]);
    let b = convert::<BigInt>(scope, "2n ** 64n", ()).unwrap();
    assert!(!b.sign);
    assert_eq!(b.words, vec![0, 1]);
    let b = convert::<BigInt>(scope, "0n", ()).unwrap();
    assert!(!b.sign);
    assert!(b.words.is_empty());
    let b = convert::<BigInt>(scope, "true", ()).unwrap();
    assert_eq!(b.words, vec![1]);
    let b = convert::<BigInt>(scope, "'10'", ()).unwrap();
    assert_eq!(b.words, vec![10]);
    for code in ["1", "undefined", "null", "Symbol()", "'abc'"] {
      assert!(
        convert::<BigInt>(scope, code, ()).is_err(),
        "bigint({code}) should fail"
      );
    }
  }

  #[test]
  fn webidl_nullable_converts_null_and_undefined_to_none() {
    let mut runtime = JsRuntime::new(RuntimeOptions::default());
    let scope = &mut runtime.handle_scope();
    assert_eq!(
      convert::<Option<u8>>(scope, "null", IntOptions::default()).unwrap(),
      None
    );
    assert_eq!(
      convert::<Option<u8>>(scope, "undefined", IntOptions::default()).unwrap(),
      None
    );
    assert_eq!(
      convert::<Option<u8>>(scope, "7", IntOptions::default()).unwrap(),
      Some(7)
    );
    let clamp = IntOptions {
      clamp: true,
      enforce_range: false,
    };
    assert_eq!(
      convert::<Option<u8>>(scope, "300", clamp).unwrap(),
      Some(255)
    );
    assert_eq!(
      convert::<Option<String>>(scope, "0", StringOptions::default()).unwrap(),
      Some("0".to_string())
    );
  }

  #[test]
  fn webidl_sequence_iterables() {
    let mut runtime = JsRuntime::new(RuntimeOptions::default());
    let scope = &mut runtime.handle_scope();
    let o = IntOptions::default();
    assert_eq!(
      convert::<Vec<u8>>(scope, "[1, 2, 3]", o).unwrap(),
      vec![1, 2, 3]
    );
    assert_eq!(
      convert::<Vec<u8>>(scope, "new Set([4, 5])", o).unwrap(),
      vec![4, 5]
    );
    assert_eq!(
      convert::<Vec<u8>>(scope, "(function* () { yield 6; yield 7; })()", o)
        .unwrap(),
      vec![6, 7]
    );
    assert_eq!(
      convert::<Vec<u8>>(
        scope,
        "({ [Symbol.iterator]() { let i = 0; return { next() { return i < 2 ? { value: i++, done: false } : { done: true } } }; } })",
        o
      )
      .unwrap(),
      vec![0, 1]
    );
    // Holes convert as undefined, which u8 turns into 0.
    assert_eq!(convert::<Vec<u8>>(scope, "[, 1]", o).unwrap(), vec![0, 1]);
    assert_eq!(
      convert::<Vec<String>>(scope, "[]", StringOptions::default()).unwrap(),
      Vec::<String>::new()
    );
  }

  #[test]
  fn webidl_sequence_hostile_inputs() {
    let mut runtime = JsRuntime::new(RuntimeOptions::default());
    let scope = &mut runtime.handle_scope();
    let o = IntOptions::default();
    let not_a_sequence =
      "Failed to execute 'op_test': Argument 1 can not be converted to a sequence";
    for code in [
      "'abc'",
      "1",
      "undefined",
      "null",
      "true",
      "({})",
      "({ length: 2 })",
    ] {
      assert_eq!(
        convert_err::<Vec<u8>>(scope, code, o),
        not_a_sequence,
        "{code}"
      );
    }
    assert_eq!(
      convert_err::<Vec<u8>>(scope, "({ [Symbol.iterator]: 1 })", o),
      not_a_sequence
    );
    assert_eq!(
      convert_err::<Vec<u8>>(scope,
        "({ [Symbol.iterator]() { return { next() { throw new Error('next broke') } } } })",
        o
      ),
      "Failed to execute 'op_test': Argument 1 threw an exception: Error: next broke"
    );
    let enforce = IntOptions {
      clamp: false,
      enforce_range: true,
    };
    assert_eq!(
      convert_err::<Vec<u8>>(scope,"[1, 300]", enforce),
      "Failed to execute 'op_test': Argument 1, index 1 is outside the accepted range of 0 to 255"
    );
  }

  #[test]
  fn webidl_record_conversions() {
    let mut runtime = JsRuntime::new(RuntimeOptions::default());
    let scope = &mut runtime.handle_scope();
    let o = IntOptions::default();
    let record: HashMap<String, u8> =
      convert(scope, "({ a: 1, b: 2, [5]: 3 })", o).unwrap();
    assert_eq!(record.len(), 3);
    assert_eq!(record["a"], 1);
    assert_eq!(record["5"], 3);
    // Inherited and non-enumerable properties are skipped.
    let record: HashMap<String, u8> = convert(
      scope,
      "(function () { const p = { inherited: 1 }; const o = Object.create(p); Object.defineProperty(o, 'hidden', { value: 2, enumerable: false }); o.shown = 3; return o; })()",
      o,
    )
    .unwrap();
    assert_eq!(record.len(), 1);
    assert_eq!(record["shown"], 3);
    // Proxies work through their traps.
    let record: HashMap<String, u8> = convert(
      scope,
      "new Proxy({}, { ownKeys() { return ['p']; }, getOwnPropertyDescriptor() { return { value: 9, enumerable: true, configurable: true }; }, get() { return 9; } })",
      o,
    )
    .unwrap();
    assert_eq!(record["p"], 9);
  }

  #[test]
  fn webidl_record_hostile_inputs() {
    let mut runtime = JsRuntime::new(RuntimeOptions::default());
    let scope = &mut runtime.handle_scope();
    let o = IntOptions::default();
    for code in ["null", "undefined", "1", "'abc'"] {
      assert_eq!(
        convert_err::<HashMap<String, u8>>(scope,code, o),
        "Failed to execute 'op_test': Argument 1 can not be converted to a record",
        "{code}"
      );
    }
    assert_eq!(
      convert_err::<HashMap<String, u8>>(scope,"({ [Symbol('s')]: 1 })", o),
      "Failed to execute 'op_test': Argument 1 is a Symbol and can not be converted to a string"
    );
    let enforce = IntOptions {
      clamp: false,
      enforce_range: true,
    };
    assert_eq!(
      convert_err::<HashMap<String, u8>>(scope,"({ bad: 300 })", enforce),
      "Failed to execute 'op_test': 'bad' (Argument 1) is outside the accepted range of 0 to 255"
    );
    assert_eq!(
      convert_err::<HashMap<String, u8>>(scope,
        "({ get bad() { throw new Error('getter boom') } })",
        o
      ),
      "Failed to execute 'op_test': Argument 1 threw an exception: Error: getter boom"
    );
  }

  #[derive(WebIDL, Debug, PartialEq)]
  #[webidl(dictionary)]
  struct Member {
    #[webidl(rename = "zeta")]
    z: u8,
    alpha: u8,
    #[webidl(default = 7)]
    mid: u8,
  }

  #[test]
  fn webidl_derive_dictionary_reads_in_sorted_order() {
    let mut runtime = JsRuntime::new(RuntimeOptions::default());
    let scope = &mut runtime.handle_scope();
    let _ = eval(
      scope,
      "globalThis.log = []; globalThis.d = { get zeta() { log.push('zeta'); return 3; }, get alpha() { log.push('alpha'); return 1; }, get mid() { log.push('mid'); return undefined; } }",
    );
    let got = convert::<Member>(scope, "globalThis.d", ()).unwrap();
    assert_eq!(
      got,
      Member {
        z: 3,
        alpha: 1,
        mid: 7
      }
    );
    let log = eval(scope, "log.join(',')").to_rust_string_lossy(scope);
    assert_eq!(log, "alpha,mid,zeta");
  }

  #[derive(WebIDL, Debug, PartialEq)]
  #[webidl(dictionary)]
  struct Options {
    #[webidl(required)]
    foo_bar: u8,
    #[options(clamp = true)]
    clamped: Option<u8>,
    #[options(enforce_range = true)]
    ranged: Option<u8>,
    list: Option<Vec<u8>>,
    r#type: Option<String>,
    #[webidl(default = String::from("dflt"))]
    text: String,
  }

  #[test]
  fn webidl_derive_dictionary_members() {
    let mut runtime = JsRuntime::new(RuntimeOptions::default());
    let scope = &mut runtime.handle_scope();
    let got = convert::<Options>(
      scope,
      "({ fooBar: 1, clamped: 300, ranged: null, list: [1, 2], type: 'x' })",
      (),
    )
    .unwrap();
    assert_eq!(
      got,
      Options {
        foo_bar: 1,
        clamped: Some(255),
        ranged: None,
        list: Some(vec![1, 2]),
        r#type: Some("x".to_string()),
        text: "dflt".to_string(),
      }
    );
    // An empty or undefined dictionary still reports the required member.
    assert_eq!(
      convert_err::<Options>(scope,"undefined", ()),
      "Failed to execute 'op_test': Argument 1 can not be converted to 'Options' because 'fooBar' is required in 'Options'"
    );
    assert_eq!(
      convert_err::<Options>(scope,"({ fooBar: 1, ranged: 300 })", ()),
      "Failed to execute 'op_test': 'ranged' of 'Options' (Argument 1) is outside the accepted range of 0 to 255"
    );
    assert_eq!(
      convert_err::<Options>(scope,"({ fooBar: 1, ranged: 1, text: Symbol() })", ()),
      "Failed to execute 'op_test': 'text' of 'Options' (Argument 1) is a Symbol and can not be converted to a string"
    );
    for code in ["1", "'x'", "true", "Symbol()", "1n"] {
      assert_eq!(
        convert_err::<Options>(scope, code, ()),
        "Failed to execute 'op_test': Argument 1 can not be converted to 'Options'",
        "{code}"
      );
    }
  }

  #[derive(WebIDL, Debug, PartialEq)]
  #[webidl(enum)]
  enum Mode {
    FooBar,
    Foo,
    #[webidl(rename = "custom")]
    Renamed,
  }

  #[test]
  fn webidl_derive_enum_values() {
    let mut runtime = JsRuntime::new(RuntimeOptions::default());
    let scope = &mut runtime.handle_scope();
    assert_eq!(
      convert::<Mode>(scope, "'foo-bar'", ()).unwrap(),
      Mode::FooBar
    );
    assert_eq!(convert::<Mode>(scope, "'foo'", ()).unwrap(), Mode::Foo);
    assert_eq!(
      convert::<Mode>(scope, "'custom'", ()).unwrap(),
      Mode::Renamed
    );
    for code in ["'FooBar'", "''", "'Foo'", "'renamed'"] {
      let err = convert::<Mode>(scope, code, ());
      assert!(err.is_err(), "{code} should be rejected");
    }
    assert_eq!(
      convert_err::<Mode>(scope,"'FooBar'", ()),
      "Failed to execute 'op_test': Argument 1 can not be converted to 'Mode' because 'FooBar' is not a valid enum value"
    );
  }

  #[test]
  fn webidl_derive_key_names() {
    // camelCase member keys, including digits and raw identifiers.
    #[derive(WebIDL, Debug, PartialEq)]
    #[webidl(dictionary)]
    struct Keys {
      foo_bar_baz2: u8,
      r#type: u8,
      foo: u8,
    }
    let mut runtime = JsRuntime::new(RuntimeOptions::default());
    let scope = &mut runtime.handle_scope();
    let got =
      convert::<Keys>(scope, "({ fooBarBaz2: 1, type: 2, foo: 3 })", ())
        .unwrap();
    assert_eq!(
      got,
      Keys {
        foo_bar_baz2: 1,
        r#type: 2,
        foo: 3
      }
    );
  }

  #[test]
  fn webidl_js_exception_is_not_pending() {
    let mut runtime = JsRuntime::new(RuntimeOptions::default());
    let scope = &mut runtime.handle_scope();
    let value = eval(scope, "({ toString() { throw new TypeError('nope') } })");
    let err = <String as WebIdlConverter>::convert(
      scope,
      value,
      "P".into(),
      || Cow::Borrowed("C"),
      &StringOptions::default(),
    )
    .unwrap_err();
    assert_eq!(err.to_string(), "P: C threw an exception: TypeError: nope");
    // The exception was caught by convert, so nothing is pending.
    let mut tc = v8::TryCatch::new(scope);
    assert!(tc.exception().is_none());
  }

  #[test]
  fn webidl_sequence_closes_iterator_on_element_error() {
    let mut runtime = JsRuntime::new(RuntimeOptions::default());
    let scope = &mut runtime.handle_scope();
    let enforce = IntOptions {
      clamp: false,
      enforce_range: true,
    };
    let message =
      "Failed to execute 'op_test': Argument 1, index 0 is outside the accepted range of 0 to 255";
    let _ = eval(scope, "globalThis.closed = 0");
    assert_eq!(
      convert_err::<Vec<u8>>(
        scope,
        "({ [Symbol.iterator]() { return { next() { return { value: 300, done: false } }, return() { globalThis.closed++; return {}; } }; } })",
        enforce
      ),
      message
    );
    assert_eq!(
      eval(scope, "globalThis.closed").to_rust_string_lossy(scope),
      "1"
    );
    // A `return` that throws or is not callable is ignored, and the element
    // error is still the one reported.
    for code in [
      "({ [Symbol.iterator]() { return { next() { return { value: 300, done: false } }, return() { throw new Error('ignored') } }; } })",
      "({ [Symbol.iterator]() { return { next() { return { value: 300, done: false } }, return: 1 }; } })",
      "({ [Symbol.iterator]() { return { next() { return { value: 300, done: false } } }; } })",
    ] {
      assert_eq!(
        convert_err::<Vec<u8>>(scope, code, enforce),
        message,
        "{code}"
      );
    }
  }

  #[derive(WebIDL, Debug, PartialEq)]
  #[webidl(dictionary)]
  struct SeqMembers {
    #[options(clamp = true)]
    clamped: Vec<u8>,
    #[options(enforce_range = true)]
    ranged: Vec<u8>,
  }

  #[test]
  fn webidl_derive_dictionary_sequence_members() {
    let mut runtime = JsRuntime::new(RuntimeOptions::default());
    let scope = &mut runtime.handle_scope();
    let got = convert::<SeqMembers>(
      scope,
      "({ clamped: [300, -5, 2.5], ranged: [1] })",
      (),
    )
    .unwrap();
    assert_eq!(
      got,
      SeqMembers {
        clamped: vec![255, 0, 2],
        ranged: vec![1],
      }
    );
    assert_eq!(
      convert_err::<SeqMembers>(scope, "({ clamped: [], ranged: [300] })", ()),
      "Failed to execute 'op_test': 'ranged' of 'SeqMembers' (Argument 1), index 0 is outside the accepted range of 0 to 255"
    );
  }

  #[derive(WebIDL, Debug, PartialEq)]
  #[webidl(enum)]
  enum Acronym {
    HTTPMethod,
    AB2Cd,
  }

  #[test]
  fn webidl_derive_enum_acronyms_and_symbols() {
    let mut runtime = JsRuntime::new(RuntimeOptions::default());
    let scope = &mut runtime.handle_scope();
    // An uppercase run is one word, so HTTPMethod is "http-method". A digit
    // ends a word, so AB2Cd is "ab2-cd".
    assert_eq!(
      convert::<Acronym>(scope, "'http-method'", ()).unwrap(),
      Acronym::HTTPMethod
    );
    assert_eq!(
      convert::<Acronym>(scope, "'ab2-cd'", ()).unwrap(),
      Acronym::AB2Cd
    );
    for (code, value) in [
      ("'HTTPMethod'", "HTTPMethod"),
      ("'h-t-t-p-method'", "h-t-t-p-method"),
    ] {
      assert_eq!(
        convert_err::<Acronym>(scope, code, ()),
        format!(
          "Failed to execute 'op_test': Argument 1 can not be converted to 'Acronym' because '{value}' is not a valid enum value"
        ),
        "{code}"
      );
    }
    assert_eq!(
      convert_err::<Mode>(scope, "Symbol()", ()),
      "Failed to execute 'op_test': Argument 1 is a Symbol and can not be converted to a string"
    );
  }
}
