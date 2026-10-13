// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.

//! Converter tests that run real V8 code through a `JsRuntime`. Error cases
//! compare the exact message, since the message is the public contract.

use std::collections::HashMap;

use super::*;
use crate::JsRuntime;
use crate::RuntimeOptions;

/// The prefix and context used by every test conversion.
const OP: &str = "Failed to execute 'op': Argument 1";

/// Evaluates `code` as a script and returns its completion value.
fn eval<'a>(
  scope: &mut v8::HandleScope<'a>,
  code: &str,
) -> v8::Local<'a, v8::Value> {
  let source = v8::String::new(scope, code).unwrap();
  let script = v8::Script::compile(scope, source, None).unwrap();
  script.run(scope).unwrap()
}

/// Evaluates `code` and converts the result to `T`, returning the error text.
fn convert<'a, T: WebIdlConverter<'a>>(
  scope: &mut v8::HandleScope<'a>,
  code: &str,
  options: T::Options,
) -> Result<T, String> {
  let value = eval(scope, code);
  T::convert(
    scope,
    value,
    Cow::Borrowed("Failed to execute 'op'"),
    || Cow::Borrowed("Argument 1"),
    &options,
  )
  .map_err(|e| e.to_string())
}

#[test]
fn error_display_shape() {
  let e = WebIdlError::new(
    Cow::Borrowed("Failed to execute 'fetch'"),
    "Argument 1",
    WebIdlErrorKind::NotFinite,
  );
  assert_eq!(
    e.to_string(),
    "Failed to execute 'fetch': Argument 1 is not a finite number"
  );
  assert_eq!(e.kind(), &WebIdlErrorKind::NotFinite);

  let no_context =
    WebIdlError::new(Cow::Borrowed("P"), "", WebIdlErrorKind::NotAnObject);
  assert_eq!(no_context.to_string(), "P: is not an object");

  let required = WebIdlError::new(
    Cow::Borrowed("P"),
    "Argument 1",
    WebIdlErrorKind::Required {
      dictionary: "Options",
      key: "fooBar".to_string(),
    },
  );
  assert_eq!(
    required.to_string(),
    "P: Argument 1 can not be converted to 'Options' because 'fooBar' is \
     required in 'Options'"
  );

  let enum_error = WebIdlError::new(
    Cow::Borrowed("P"),
    "Argument 1",
    WebIdlErrorKind::InvalidEnumValue {
      value: "Foo-Bar".to_string(),
      enum_name: "Mode",
    },
  );
  assert_eq!(
    enum_error.to_string(),
    "P: Argument 1 'Foo-Bar' is not a valid enum value of 'Mode'"
  );

  assert_eq!(
    dictionary_member_context("fooBar", "Options", "Argument 1"),
    "'fooBar' of 'Options' (Argument 1)"
  );
}

#[test]
fn error_is_send_sync_static() {
  fn assert_traits<T: Error + Send + Sync + 'static>() {}
  assert_traits::<WebIdlError>();
}

#[test]
fn context_is_not_built_on_success() {
  let mut runtime = JsRuntime::new(RuntimeOptions::default());
  let scope = &mut runtime.handle_scope();
  let value = eval(scope, "1");
  let result = u8::convert(
    scope,
    value,
    Cow::Borrowed("P"),
    || -> Cow<'static, str> { panic!("context built on success path") },
    &IntOptions::default(),
  );
  assert_eq!(result.unwrap(), 1);
}

#[test]
fn option_defaults_are_all_false() {
  assert_eq!(
    IntOptions::default(),
    IntOptions {
      clamp: false,
      enforce_range: false
    }
  );
  assert_eq!(
    StringOptions::default(),
    StringOptions {
      treat_null_as_empty_string: false
    }
  );
}

#[test]
fn wrappers_deref_to_inner() {
  assert_eq!(*UnrestrictedFloat(1.5), 1.5);
  assert_eq!(*UnrestrictedDouble(2.5), 2.5);
  assert_eq!(ByteString("ab".to_string()).len(), 2);
}

#[test]
fn any_returns_value_unchanged() {
  let mut runtime = JsRuntime::new(RuntimeOptions::default());
  let scope = &mut runtime.handle_scope();
  let value = convert::<v8::Local<v8::Value>>(scope, "Symbol('s')", ());
  assert!(value.unwrap().is_symbol());
  let value = convert::<v8::Local<v8::Value>>(scope, "({a: 1})", ());
  assert!(value.unwrap().is_object());
}

#[test]
fn bool_uses_to_boolean_and_never_errors() {
  let mut runtime = JsRuntime::new(RuntimeOptions::default());
  let scope = &mut runtime.handle_scope();
  for code in ["0", "''", "null", "undefined", "NaN", "0n"] {
    assert_eq!(convert::<bool>(scope, code, ()), Ok(false), "{code}");
  }
  for code in ["'0'", "({})", "[]", "Symbol('s')", "1"] {
    assert_eq!(convert::<bool>(scope, code, ()), Ok(true), "{code}");
  }
}

#[test]
fn integers_default_wrap_modulo() {
  let mut runtime = JsRuntime::new(RuntimeOptions::default());
  let scope = &mut runtime.handle_scope();
  let o = IntOptions::default;
  assert_eq!(convert::<u8>(scope, "256", o()), Ok(0));
  assert_eq!(convert::<u8>(scope, "-1", o()), Ok(255));
  assert_eq!(convert::<u8>(scope, "255.9", o()), Ok(255));
  assert_eq!(convert::<u8>(scope, "257.5", o()), Ok(1));
  assert_eq!(convert::<i8>(scope, "128", o()), Ok(-128));
  assert_eq!(convert::<i8>(scope, "255", o()), Ok(-1));
  assert_eq!(convert::<u16>(scope, "65536", o()), Ok(0));
  assert_eq!(convert::<i16>(scope, "32768", o()), Ok(-32768));
  assert_eq!(convert::<u32>(scope, "-1", o()), Ok(4294967295));
  assert_eq!(convert::<i32>(scope, "2147483648", o()), Ok(-2147483648));
  assert_eq!(convert::<u64>(scope, "-1", o()), Ok(u64::MAX));
  assert_eq!(convert::<u64>(scope, "2 ** 64", o()), Ok(0));
  assert_eq!(convert::<i64>(scope, "2 ** 63", o()), Ok(i64::MIN));
  assert_eq!(convert::<i64>(scope, "-(2 ** 63)", o()), Ok(i64::MIN));
}

#[test]
fn integers_convert_special_values() {
  let mut runtime = JsRuntime::new(RuntimeOptions::default());
  let scope = &mut runtime.handle_scope();
  let o = IntOptions::default;
  for code in [
    "NaN",
    "Infinity",
    "-Infinity",
    "0",
    "-0",
    "undefined",
    "null",
  ] {
    assert_eq!(convert::<i32>(scope, code, o()), Ok(0), "{code}");
  }
  assert_eq!(convert::<u8>(scope, "'12'", o()), Ok(12));
  assert_eq!(convert::<u8>(scope, "new Number(5)", o()), Ok(5));
  assert_eq!(convert::<u8>(scope, "true", o()), Ok(1));
}

#[test]
fn integers_reject_bigint_and_symbol() {
  let mut runtime = JsRuntime::new(RuntimeOptions::default());
  let scope = &mut runtime.handle_scope();
  assert_eq!(
    convert::<u8>(scope, "1n", IntOptions::default()),
    Err(format!(
      "{OP} threw an exception: TypeError: Cannot convert a BigInt value to a number"
    ))
  );
  assert_eq!(
    convert::<i64>(scope, "Symbol('s')", IntOptions::default()),
    Err(format!(
      "{OP} is a Symbol and can not be converted to a number"
    ))
  );
}

#[test]
fn integers_enforce_range() {
  let mut runtime = JsRuntime::new(RuntimeOptions::default());
  let scope = &mut runtime.handle_scope();
  let o = || IntOptions {
    clamp: false,
    enforce_range: true,
  };
  assert_eq!(convert::<u8>(scope, "255.9", o()), Ok(255));
  assert_eq!(convert::<u8>(scope, "-0.9", o()), Ok(0));
  assert_eq!(convert::<i8>(scope, "-128", o()), Ok(-128));
  assert_eq!(
    convert::<u8>(scope, "256", o()),
    Err(format!(
      "{OP} is outside the accepted range of 0 to 255, inclusive"
    ))
  );
  assert_eq!(
    convert::<u8>(scope, "-1", o()),
    Err(format!(
      "{OP} is outside the accepted range of 0 to 255, inclusive"
    ))
  );
  assert_eq!(
    convert::<i8>(scope, "-129", o()),
    Err(format!(
      "{OP} is outside the accepted range of -128 to 127, inclusive"
    ))
  );
  assert_eq!(
    convert::<u64>(scope, "2 ** 53 - 1", o()),
    Ok(9007199254740991)
  );
  assert_eq!(
    convert::<u64>(scope, "2 ** 53", o()),
    Err(format!(
      "{OP} is outside the accepted range of 0 to 9007199254740991, inclusive"
    ))
  );
  for code in ["NaN", "Infinity", "-Infinity"] {
    assert_eq!(
      convert::<u8>(scope, code, o()),
      Err(format!("{OP} is not a finite number")),
      "{code}"
    );
  }
}

#[test]
fn integers_clamp() {
  let mut runtime = JsRuntime::new(RuntimeOptions::default());
  let scope = &mut runtime.handle_scope();
  let o = || IntOptions {
    clamp: true,
    enforce_range: false,
  };
  assert_eq!(convert::<u8>(scope, "300", o()), Ok(255));
  assert_eq!(convert::<u8>(scope, "-5", o()), Ok(0));
  assert_eq!(convert::<u8>(scope, "2.5", o()), Ok(2));
  assert_eq!(convert::<u8>(scope, "3.5", o()), Ok(4));
  assert_eq!(convert::<u8>(scope, "-0.5", o()), Ok(0));
  assert_eq!(convert::<u8>(scope, "-0", o()), Ok(0));
  assert_eq!(convert::<u8>(scope, "Infinity", o()), Ok(255));
  assert_eq!(convert::<u8>(scope, "-Infinity", o()), Ok(0));
  assert_eq!(convert::<u8>(scope, "NaN", o()), Ok(0));
  assert_eq!(convert::<i8>(scope, "-200", o()), Ok(-128));
  assert_eq!(convert::<u64>(scope, "1e300", o()), Ok(9007199254740991));
}

#[test]
fn integers_enforce_range_wins_over_clamp() {
  let mut runtime = JsRuntime::new(RuntimeOptions::default());
  let scope = &mut runtime.handle_scope();
  let options = IntOptions {
    clamp: true,
    enforce_range: true,
  };
  assert_eq!(
    convert::<u8>(scope, "300", options),
    Err(format!(
      "{OP} is outside the accepted range of 0 to 255, inclusive"
    ))
  );
}

#[test]
fn floats_require_finite_values() {
  let mut runtime = JsRuntime::new(RuntimeOptions::default());
  let scope = &mut runtime.handle_scope();
  assert_eq!(convert::<f64>(scope, "1.5", ()), Ok(1.5));
  assert_eq!(
    convert::<f64>(scope, "'abc'", ()),
    Err(format!("{OP} is not a finite number"))
  );
  assert_eq!(
    convert::<f64>(scope, "Infinity", ()),
    Err(format!("{OP} is not a finite number"))
  );
  assert_eq!(
    convert::<f64>(scope, "Symbol('s')", ()),
    Err(format!(
      "{OP} is a Symbol and can not be converted to a number"
    ))
  );
  assert_eq!(
    convert::<f64>(scope, "({ valueOf() { throw new Error('boom') } })", ()),
    Err(format!("{OP} threw an exception: Error: boom"))
  );
}

#[test]
fn floats_f32_rounds_and_overflows() {
  let mut runtime = JsRuntime::new(RuntimeOptions::default());
  let scope = &mut runtime.handle_scope();
  assert_eq!(convert::<f32>(scope, "0.1", ()), Ok(0.1f32));
  assert_eq!(
    convert::<f32>(scope, "3.4028234663852886e38", ()),
    Ok(f32::MAX)
  );
  assert_eq!(
    convert::<f32>(scope, "1e40", ()),
    Err(format!("{OP} is not a finite number"))
  );
  assert_eq!(
    convert::<f32>(scope, "NaN", ()),
    Err(format!("{OP} is not a finite number"))
  );
}

#[test]
fn unrestricted_floats_pass_non_finite_values() {
  let mut runtime = JsRuntime::new(RuntimeOptions::default());
  let scope = &mut runtime.handle_scope();
  assert!(convert::<UnrestrictedDouble>(scope, "'abc'", ())
    .unwrap()
    .is_nan());
  assert_eq!(
    convert::<UnrestrictedDouble>(scope, "-Infinity", ()),
    Ok(UnrestrictedDouble(f64::NEG_INFINITY))
  );
  assert!(convert::<UnrestrictedFloat>(scope, "NaN", ())
    .unwrap()
    .is_nan());
  assert_eq!(
    convert::<UnrestrictedFloat>(scope, "1e40", ()),
    Ok(UnrestrictedFloat(f32::INFINITY))
  );
  assert_eq!(
    convert::<UnrestrictedDouble>(scope, "Symbol('s')", ()),
    Err(format!(
      "{OP} is a Symbol and can not be converted to a number"
    ))
  );
}

#[test]
fn strings_follow_to_string() {
  let mut runtime = JsRuntime::new(RuntimeOptions::default());
  let scope = &mut runtime.handle_scope();
  let o = StringOptions::default;
  assert_eq!(
    convert::<String>(scope, "'abc'", o()),
    Ok("abc".to_string())
  );
  assert_eq!(
    convert::<String>(scope, "null", o()),
    Ok("null".to_string())
  );
  assert_eq!(
    convert::<String>(scope, "undefined", o()),
    Ok("undefined".to_string())
  );
  assert_eq!(
    convert::<String>(scope, "({ toString() { return 'x' } })", o()),
    Ok("x".to_string())
  );
  assert_eq!(
    convert::<String>(scope, r"'\uD800'", o()),
    Ok("\u{FFFD}".to_string())
  );
}

#[test]
fn strings_treat_null_as_empty() {
  let mut runtime = JsRuntime::new(RuntimeOptions::default());
  let scope = &mut runtime.handle_scope();
  let o = || StringOptions {
    treat_null_as_empty_string: true,
  };
  assert_eq!(convert::<String>(scope, "null", o()), Ok(String::new()));
  assert_eq!(
    convert::<String>(scope, "undefined", o()),
    Ok("undefined".to_string())
  );
}

#[test]
fn strings_reject_symbol_and_report_throws() {
  let mut runtime = JsRuntime::new(RuntimeOptions::default());
  let scope = &mut runtime.handle_scope();
  let o = StringOptions::default;
  assert_eq!(
    convert::<String>(scope, "Symbol('s')", o()),
    Err(format!(
      "{OP} is a Symbol and can not be converted to a string"
    ))
  );
  assert_eq!(
    convert::<String>(scope, "({ toString() { throw 1 } })", o()),
    Err(format!("{OP} threw an exception: 1"))
  );
  assert_eq!(
    convert::<String>(
      scope,
      "({ toString() { throw new Error('boom') } })",
      o()
    ),
    Err(format!("{OP} threw an exception: Error: boom"))
  );
}

#[test]
fn byte_strings() {
  let mut runtime = JsRuntime::new(RuntimeOptions::default());
  let scope = &mut runtime.handle_scope();
  let o = StringOptions::default;
  assert_eq!(
    convert::<ByteString>(scope, r"'\xFF'", o()),
    Ok(ByteString("ÿ".to_string()))
  );
  assert_eq!(
    convert::<ByteString>(scope, "'abc'", o()),
    Ok(ByteString("abc".to_string()))
  );
  assert_eq!(
    convert::<ByteString>(scope, "''", o()),
    Ok(ByteString(String::new()))
  );
  assert_eq!(
    convert::<ByteString>(scope, "'€'", o()),
    Err(format!(
      "{OP} is not a valid ByteString (U+20AC is greater than U+00FF)"
    ))
  );
  assert_eq!(
    convert::<ByteString>(scope, r"'\uD800'", o()),
    Err(format!(
      "{OP} is not a valid ByteString (U+FFFD is greater than U+00FF)"
    ))
  );
  assert_eq!(
    convert::<ByteString>(scope, "Symbol('s')", o()),
    Err(format!(
      "{OP} is a Symbol and can not be converted to a string"
    ))
  );
  let null_empty = StringOptions {
    treat_null_as_empty_string: true,
  };
  assert_eq!(
    convert::<ByteString>(scope, "null", null_empty),
    Ok(ByteString(String::new()))
  );
}

#[test]
fn bigint_conversions() {
  let mut runtime = JsRuntime::new(RuntimeOptions::default());
  let scope = &mut runtime.handle_scope();
  let big = |sign, words: &[u64]| BigInt {
    sign,
    words: words.to_vec(),
  };
  assert_eq!(convert::<BigInt>(scope, "123n", ()), Ok(big(false, &[123])));
  assert_eq!(convert::<BigInt>(scope, "-1n", ()), Ok(big(true, &[1])));
  assert_eq!(convert::<BigInt>(scope, "0n", ()), Ok(big(false, &[])));
  assert_eq!(
    convert::<BigInt>(scope, "2n ** 64n", ()),
    Ok(big(false, &[0, 1]))
  );
  assert_eq!(
    convert::<BigInt>(scope, "-(2n ** 64n)", ()),
    Ok(big(true, &[0, 1]))
  );
  assert_eq!(convert::<BigInt>(scope, "true", ()), Ok(big(false, &[1])));
  assert_eq!(convert::<BigInt>(scope, "false", ()), Ok(big(false, &[])));
  assert_eq!(
    convert::<BigInt>(scope, "'123'", ()),
    Ok(big(false, &[123]))
  );
}

#[test]
fn bigint_rejects_invalid_input() {
  let mut runtime = JsRuntime::new(RuntimeOptions::default());
  let scope = &mut runtime.handle_scope();
  assert_eq!(
    convert::<BigInt>(scope, "'abc'", ()),
    Err(format!(
      "{OP} threw an exception: SyntaxError: Cannot convert abc to a BigInt"
    ))
  );
  assert_eq!(
    convert::<BigInt>(scope, "1", ()),
    Err(format!(
      "{OP} threw an exception: TypeError: Cannot convert 1 to a BigInt"
    ))
  );
  assert_eq!(
    convert::<BigInt>(scope, "undefined", ()),
    Err(format!(
      "{OP} threw an exception: TypeError: Cannot convert undefined to a BigInt"
    ))
  );
  assert_eq!(
    convert::<BigInt>(scope, "null", ()),
    Err(format!(
      "{OP} threw an exception: TypeError: Cannot convert null to a BigInt"
    ))
  );
  assert_eq!(
    convert::<BigInt>(scope, "Symbol('s')", ()),
    Err(format!(
      "{OP} is a Symbol and can not be converted to a BigInt"
    ))
  );
}

#[test]
fn option_maps_nullish_to_none() {
  let mut runtime = JsRuntime::new(RuntimeOptions::default());
  let scope = &mut runtime.handle_scope();
  assert_eq!(
    convert::<Option<u8>>(scope, "undefined", Default::default()),
    Ok(None)
  );
  assert_eq!(
    convert::<Option<u8>>(scope, "null", Default::default()),
    Ok(None)
  );
  assert_eq!(
    convert::<Option<u8>>(scope, "5", Default::default()),
    Ok(Some(5))
  );
  assert_eq!(
    convert::<Option<String>>(scope, "null", StringOptions::default()),
    Ok(None)
  );
  let treat = StringOptions {
    treat_null_as_empty_string: true,
  };
  assert_eq!(convert::<Option<String>>(scope, "null", treat), Ok(None));
  assert_eq!(
    convert::<Option<String>>(scope, "'x'", treat),
    Ok(Some("x".to_string()))
  );
  assert_eq!(
    convert::<Option<u8>>(scope, "Symbol('s')", Default::default()),
    Err(format!(
      "{OP} is a Symbol and can not be converted to a number"
    ))
  );
  let clamp = IntOptions {
    clamp: true,
    enforce_range: false,
  };
  assert_eq!(convert::<Option<u8>>(scope, "300", clamp), Ok(Some(255)));
}

#[test]
fn sequences_iterate_with_the_iterator_protocol() {
  let mut runtime = JsRuntime::new(RuntimeOptions::default());
  let scope = &mut runtime.handle_scope();
  let o = IntOptions::default;
  assert_eq!(
    convert::<Vec<u8>>(scope, "[1, 2, 3]", o()),
    Ok(vec![1, 2, 3])
  );
  assert_eq!(convert::<Vec<u8>>(scope, "[]", o()), Ok(vec![]));
  assert_eq!(
    convert::<Vec<u8>>(scope, "new Set([4, 5])", o()),
    Ok(vec![4, 5])
  );
  assert_eq!(
    convert::<Vec<u8>>(scope, "(function* () { yield 1; yield 2; })()", o()),
    Ok(vec![1, 2])
  );
  assert_eq!(
    convert::<Vec<u8>>(
      scope,
      "({ [Symbol.iterator]() { let i = 0; return { next() { i++; \
       return { done: i > 2, value: i * 10 }; } }; } })",
      o()
    ),
    Ok(vec![10, 20])
  );
  let clamp = IntOptions {
    clamp: true,
    enforce_range: false,
  };
  assert_eq!(
    convert::<Vec<u8>>(scope, "[300, -5]", clamp),
    Ok(vec![255, 0])
  );
}

#[test]
fn sequences_reject_non_iterables() {
  let mut runtime = JsRuntime::new(RuntimeOptions::default());
  let scope = &mut runtime.handle_scope();
  let expected = format!("{OP} can not be converted to sequence");
  for code in [
    "'abc'",
    "1",
    "null",
    "undefined",
    "Symbol('s')",
    "({})",
    "({ [Symbol.iterator]: 1 })",
    "({ [Symbol.iterator]() { return 1 } })",
  ] {
    assert_eq!(
      convert::<Vec<u8>>(scope, code, IntOptions::default()),
      Err(expected.clone()),
      "{code}"
    );
  }
  assert_eq!(
    convert::<Vec<u8>>(
      scope,
      "({ [Symbol.iterator]() { return { next() { return 1 } } } })",
      IntOptions::default()
    ),
    Err(format!("{OP} returned a non-object iterator result"))
  );
}

#[test]
fn sequences_report_iterator_throws_and_element_index() {
  let mut runtime = JsRuntime::new(RuntimeOptions::default());
  let scope = &mut runtime.handle_scope();
  assert_eq!(
    convert::<Vec<u8>>(
      scope,
      "({ [Symbol.iterator]() { return { next() { throw new Error('boom') } } } })",
      IntOptions::default()
    ),
    Err(format!("{OP} threw an exception: Error: boom"))
  );
  assert_eq!(
    convert::<Vec<f64>>(scope, "[1, 2, 'abc']", ()),
    Err(
      "Failed to execute 'op': Argument 1, index 2 is not a finite number"
        .to_string()
    )
  );
  assert_eq!(
    convert::<Vec<u8>>(scope, "[1, Symbol('s')]", IntOptions::default()),
    Err(
      "Failed to execute 'op': Argument 1, index 1 is a Symbol and can not be converted to a number"
        .to_string()
    )
  );
}

#[test]
fn records_read_own_enumerable_string_keys() {
  let mut runtime = JsRuntime::new(RuntimeOptions::default());
  let scope = &mut runtime.handle_scope();
  let o = IntOptions::default;
  let map = |pairs: &[(&str, u8)]| -> Result<HashMap<String, u8>, String> {
    Ok(
      pairs
        .iter()
        .map(|(k, v)| (k.to_string(), *v))
        .collect::<HashMap<String, u8>>(),
    )
  };
  assert_eq!(
    convert::<HashMap<String, u8>>(scope, "({a: 1, b: 2})", o()),
    map(&[("a", 1), ("b", 2)])
  );
  assert_eq!(convert::<HashMap<String, u8>>(scope, "({})", o()), map(&[]));
  assert_eq!(
    convert::<HashMap<String, u8>>(
      scope,
      "(() => { const o = {a: 1}; Object.defineProperty(o, 'hidden', \
       {value: 2, enumerable: false}); return o; })()",
      o()
    ),
    map(&[("a", 1)])
  );
  assert_eq!(
    convert::<HashMap<String, u8>>(
      scope,
      "Object.create({ inherited: 1 }, { own: { value: 2, enumerable: true } })",
      o()
    ),
    map(&[("own", 2)])
  );
  assert_eq!(
    convert::<HashMap<String, u8>>(scope, "({ [Symbol('s')]: 1, a: 2 })", o()),
    map(&[("a", 2)])
  );
  assert_eq!(
    convert::<HashMap<String, String>>(
      scope,
      "({ 1: 'x' })",
      StringOptions::default()
    ),
    Ok(HashMap::from([("1".to_string(), "x".to_string())]))
  );
}

#[test]
fn records_check_each_key_when_it_is_reached() {
  let mut runtime = JsRuntime::new(RuntimeOptions::default());
  let scope = &mut runtime.handle_scope();
  let o = IntOptions::default;
  // A getter that deletes a later key hides that key from the record.
  assert_eq!(
    convert::<HashMap<String, u8>>(
      scope,
      "(() => { const o = {a: 1, b: 2}; Object.defineProperty(o, 'a', \
       {get() { delete this.b; return 1 }, enumerable: true, \
       configurable: true}); return o; })()",
      o()
    ),
    Ok(HashMap::from([("a".to_string(), 1)]))
  );
  // A getter that makes a later key non-enumerable hides that key too.
  assert_eq!(
    convert::<HashMap<String, u8>>(
      scope,
      "(() => { const o = {a: 1, b: 2}; Object.defineProperty(o, 'a', \
       {get() { Object.defineProperty(this, 'b', {enumerable: false}); \
       return 1 }, enumerable: true, configurable: true}); return o; })()",
      o()
    ),
    Ok(HashMap::from([("a".to_string(), 1)]))
  );
  // A throwing getOwnPropertyDescriptor trap is an error, not a skipped key.
  let err = convert::<HashMap<String, u8>>(
    scope,
    "new Proxy({a: 1}, {getOwnPropertyDescriptor() { throw new Error('gopd') }})",
    o(),
  )
  .unwrap_err();
  assert!(err.contains("gopd"), "{err}");
}

#[test]
fn records_invoke_getters_and_proxy_traps() {
  let mut runtime = JsRuntime::new(RuntimeOptions::default());
  let scope = &mut runtime.handle_scope();
  let o = IntOptions::default;
  assert_eq!(
    convert::<HashMap<String, u8>>(scope, "({ get a() { return 3 } })", o()),
    Ok(HashMap::from([("a".to_string(), 3)]))
  );
  assert_eq!(
    convert::<HashMap<String, u8>>(
      scope,
      "new Proxy({}, { ownKeys() { return ['x'] }, \
       getOwnPropertyDescriptor() { return { value: 1, enumerable: true, \
       configurable: true } }, get() { return 7 } })",
      o()
    ),
    Ok(HashMap::from([("x".to_string(), 7)]))
  );
  assert_eq!(
    convert::<HashMap<String, u8>>(
      scope,
      "({ get a() { throw new Error('boom') } })",
      o()
    ),
    Err(format!("{OP} threw an exception: Error: boom"))
  );
}

#[test]
fn records_reject_non_objects_and_name_failing_key() {
  let mut runtime = JsRuntime::new(RuntimeOptions::default());
  let scope = &mut runtime.handle_scope();
  let expected = format!("{OP} is not an object");
  for code in ["null", "undefined", "1", "'s'", "true", "Symbol('s')"] {
    assert_eq!(
      convert::<HashMap<String, u8>>(scope, code, IntOptions::default()),
      Err(expected.clone()),
      "{code}"
    );
  }
  assert_eq!(
    convert::<HashMap<String, f64>>(scope, "({ a: 'x' })", ()),
    Err(
      "Failed to execute 'op': 'a' of record (Argument 1) is not a finite number"
        .to_string()
    )
  );
}

#[test]
fn engine_stays_usable_after_a_caught_exception() {
  let mut runtime = JsRuntime::new(RuntimeOptions::default());
  let scope = &mut runtime.handle_scope();
  let _ = convert::<u8>(
    scope,
    "({ valueOf() { throw new Error('boom') } })",
    IntOptions::default(),
  );
  let value = eval(scope, "1 + 1");
  assert_eq!(value.number_value(scope), Some(2.0));
}
