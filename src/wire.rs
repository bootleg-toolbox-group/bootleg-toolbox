//! Floating-point values in JSON without loss: JSON has no NaN or infinity (serde_json writes
//! `null`), so those are written as the strings "NaN", "inf" and "-inf". Finite values keep
//! the shortest text that reads back as the same bits (serde_json with `float_roundtrip`).
//! For `#[serde(with = "crate::wire")]` on a field of `f64`, `Option<f64>` or tuples of them.

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

pub(crate) trait Wire: Sized {
    fn to_value(&self) -> Value;
    fn from_value(v: &Value) -> Option<Self>;
}

impl Wire for f64 {
    fn to_value(&self) -> Value {
        match serde_json::Number::from_f64(*self) {
            Some(n) => Value::Number(n),
            None if self.is_nan() => Value::String("NaN".into()),
            None if *self > 0.0 => Value::String("inf".into()),
            None => Value::String("-inf".into()),
        }
    }

    fn from_value(v: &Value) -> Option<f64> {
        match v {
            Value::Number(n) => n.as_f64(),
            Value::String(s) => match s.as_str() {
                "NaN" => Some(f64::NAN),
                "inf" => Some(f64::INFINITY),
                "-inf" => Some(f64::NEG_INFINITY),
                _ => None,
            },
            _ => None,
        }
    }
}

impl<T: Wire> Wire for Option<T> {
    fn to_value(&self) -> Value {
        self.as_ref().map_or(Value::Null, Wire::to_value)
    }

    fn from_value(v: &Value) -> Option<Option<T>> {
        if v.is_null() { Some(None) } else { T::from_value(v).map(Some) }
    }
}

impl<A: Wire, B: Wire> Wire for (A, B) {
    fn to_value(&self) -> Value {
        Value::Array(vec![self.0.to_value(), self.1.to_value()])
    }

    fn from_value(v: &Value) -> Option<(A, B)> {
        match v.as_array()?.as_slice() {
            [a, b] => Some((A::from_value(a)?, B::from_value(b)?)),
            _ => None,
        }
    }
}

impl<A: Wire, B: Wire, C: Wire> Wire for (A, B, C) {
    fn to_value(&self) -> Value {
        Value::Array(vec![self.0.to_value(), self.1.to_value(), self.2.to_value()])
    }

    fn from_value(v: &Value) -> Option<(A, B, C)> {
        match v.as_array()?.as_slice() {
            [a, b, c] => Some((A::from_value(a)?, B::from_value(b)?, C::from_value(c)?)),
            _ => None,
        }
    }
}

pub(crate) fn serialize<T: Wire, S: Serializer>(value: &T, serializer: S) -> Result<S::Ok, S::Error> {
    value.to_value().serialize(serializer)
}

pub(crate) fn deserialize<'de, T: Wire, D: Deserializer<'de>>(deserializer: D) -> Result<T, D::Error> {
    let v = Value::deserialize(deserializer)?;
    T::from_value(&v).ok_or_else(|| serde::de::Error::custom(format!("not a number: {v}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Serialize, Deserialize)]
    struct Holder {
        #[serde(with = "crate::wire")]
        x: f64,
        #[serde(with = "crate::wire")]
        o: Option<f64>,
        #[serde(with = "crate::wire")]
        pair: Option<(f64, f64)>,
        #[serde(with = "crate::wire")]
        triple: Option<(f64, f64, f64)>,
    }

    /// Every awkward value: the signs of zero, the ends of the range, subnormals, the last
    /// digit of a shortest representation, and what JSON cannot say.
    fn awkward() -> Vec<f64> {
        let mut v = vec![
            0.0,
            -0.0,
            1.0,
            -1.0,
            0.1,
            0.1 + 0.2,
            1.0 / 3.0,
            std::f64::consts::PI,
            f64::MAX,
            f64::MIN,
            f64::MIN_POSITIVE,
            f64::EPSILON,
            5e-324,
            -5e-324,
            2.225073858507201e-308,
            1e308,
            1e-300,
            9007199254740993.0,
            4503599627370497.5,
            123456789.12345679,
            f64::NAN,
            f64::INFINITY,
            f64::NEG_INFINITY,
        ];
        // The neighbours of ordinary values: an error of one last place shows here.
        for base in [0.3f64, 17.25, 1234.5678, 44100.0, 19.5, -63.123456789] {
            let bits = base.to_bits();
            v.extend([f64::from_bits(bits - 1), base, f64::from_bits(bits + 1)]);
        }
        v
    }

    fn same(a: f64, b: f64) -> bool {
        // NaN has many bit patterns; "NaN" reads back as one of them.
        (a.is_nan() && b.is_nan()) || a.to_bits() == b.to_bits()
    }

    #[test]
    fn every_float_round_trips_exactly() {
        for x in awkward() {
            let h = Holder { x, o: Some(x), pair: Some((x, -x)), triple: Some((x, 1.0, x)) };
            let text = serde_json::to_string(&h).unwrap();
            let back: Holder = serde_json::from_str(&text).unwrap();
            assert!(same(h.x, back.x), "{x:e} -> {text}");
            assert!(back.o.is_some_and(|o| same(x, o)), "{x:e}");
            let p = back.pair.unwrap();
            assert!(same(x, p.0) && same(-x, p.1), "{x:e}");
            let t = back.triple.unwrap();
            assert!(same(x, t.0) && same(1.0, t.1) && same(x, t.2), "{x:e}");
            // And written again, the same text: nothing moved.
            assert_eq!(text, serde_json::to_string(&back).unwrap());
        }
    }

    #[test]
    fn non_finite_values_are_words_not_null() {
        let h = Holder { x: f64::NAN, o: Some(f64::INFINITY), pair: Some((f64::NEG_INFINITY, 0.5)), triple: None };
        let text = serde_json::to_string(&h).unwrap();
        assert!(text.contains(r#""x":"NaN""#) && text.contains(r#""o":"inf""#) && text.contains(r#""-inf""#), "{text}");
        // None stays null, and a missing number is refused rather than read as NaN.
        let back: Holder = serde_json::from_str(&text).unwrap();
        assert!(back.triple.is_none() && back.x.is_nan());
        assert!(serde_json::from_str::<Holder>(r#"{"x":null,"o":null,"pair":null,"triple":null}"#).is_err());
    }
}
