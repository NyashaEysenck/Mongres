//! Strict decoding of `PostgreSQL` extended-query parameter bytes.
//!
//! Decoding stays at the wire boundary. The SQL engine receives only typed,
//! lossless [`SqlValue`] instances and revalidates them against the discovered
//! schema before `MongoDB` execution.

use mongo_pg_common::{ErrorKind, ProxyError};
use mongo_pg_sql_engine::SqlValue;
use pgwire::api::{Type, portal::Format, results::FieldFormat};

/// Decodes one parameter from its `PostgreSQL` type and text or binary format.
///
/// # Errors
///
/// Returns an invalid-input error for unsupported OIDs, malformed encodings,
/// non-finite floating point values, or invalid binary lengths.
pub(crate) fn decode_parameter(
    value: Option<&[u8]>,
    parameter_type: &Type,
    format: FieldFormat,
) -> Result<SqlValue, ProxyError> {
    let Some(value) = value else {
        return Ok(SqlValue::Null);
    };
    if format == FieldFormat::Binary {
        decode_binary(value, parameter_type)
    } else {
        decode_text(value, parameter_type)
    }
}

pub(crate) fn parameter_format(format: &Format, index: usize) -> Result<FieldFormat, ProxyError> {
    match format {
        Format::Individual(codes) if index >= codes.len() => Err(invalid_input(
            "extended-query parameter format count does not match parameter count",
        )),
        _ => Ok(format.format_for(index)),
    }
}

fn decode_text(value: &[u8], parameter_type: &Type) -> Result<SqlValue, ProxyError> {
    let value = std::str::from_utf8(value)
        .map_err(|_| invalid_input("text parameter is not valid UTF-8"))?;
    match parameter_type {
        &Type::BOOL => parse_boolean(value),
        &Type::INT2 => value
            .parse::<i16>()
            .map(i64::from)
            .map(SqlValue::Integer)
            .map_err(|_| invalid_input("invalid int2 parameter")),
        &Type::INT4 => value
            .parse::<i32>()
            .map(i64::from)
            .map(SqlValue::Integer)
            .map_err(|_| invalid_input("invalid int4 parameter")),
        &Type::INT8 => value
            .parse::<i64>()
            .map(SqlValue::Integer)
            .map_err(|_| invalid_input("invalid int8 parameter")),
        &Type::FLOAT4 => finite_float(
            value
                .parse::<f32>()
                .map(f64::from)
                .map_err(|_| invalid_input("invalid float4 parameter"))?,
        ),
        &Type::FLOAT8 => finite_float(
            value
                .parse::<f64>()
                .map_err(|_| invalid_input("invalid float8 parameter"))?,
        ),
        &Type::TEXT | &Type::VARCHAR | &Type::BPCHAR => Ok(SqlValue::String(value.to_owned())),
        _ => Err(unsupported_type(parameter_type)),
    }
}

fn decode_binary(value: &[u8], parameter_type: &Type) -> Result<SqlValue, ProxyError> {
    match parameter_type {
        &Type::BOOL => match value {
            [0] => Ok(SqlValue::Boolean(false)),
            [1] => Ok(SqlValue::Boolean(true)),
            _ => Err(invalid_input("invalid binary boolean parameter")),
        },
        &Type::INT2 => fixed_binary(value, "int2", |bytes| {
            i16::from_be_bytes(bytes.try_into().expect("length is checked"))
        }),
        &Type::INT4 => fixed_binary(value, "int4", |bytes| {
            i32::from_be_bytes(bytes.try_into().expect("length is checked"))
        }),
        &Type::INT8 => fixed_binary(value, "int8", |bytes| {
            i64::from_be_bytes(bytes.try_into().expect("length is checked"))
        }),
        &Type::FLOAT4 => fixed_binary_float(value, "float4", |bytes| {
            f64::from(f32::from_be_bytes(
                bytes.try_into().expect("length is checked"),
            ))
        }),
        &Type::FLOAT8 => fixed_binary_float(value, "float8", |bytes| {
            f64::from_be_bytes(bytes.try_into().expect("length is checked"))
        }),
        &Type::TEXT | &Type::VARCHAR | &Type::BPCHAR => std::str::from_utf8(value)
            .map(|value| SqlValue::String(value.to_owned()))
            .map_err(|_| invalid_input("binary text parameter is not valid UTF-8")),
        _ => Err(unsupported_type(parameter_type)),
    }
}

fn parse_boolean(value: &str) -> Result<SqlValue, ProxyError> {
    match value {
        "t" | "true" | "1" => Ok(SqlValue::Boolean(true)),
        "f" | "false" | "0" => Ok(SqlValue::Boolean(false)),
        _ => Err(invalid_input("invalid boolean parameter")),
    }
}

fn fixed_binary<T>(
    value: &[u8],
    type_name: &str,
    decode: impl FnOnce(&[u8]) -> T,
) -> Result<SqlValue, ProxyError>
where
    T: Into<i64>,
{
    let expected_length = match type_name {
        "int2" => 2,
        "int4" => 4,
        "int8" => 8,
        _ => unreachable!("only fixed integer types use this helper"),
    };
    if value.len() != expected_length {
        return Err(invalid_input(format!(
            "invalid binary {type_name} parameter length"
        )));
    }
    Ok(SqlValue::Integer(decode(value).into()))
}

fn fixed_binary_float(
    value: &[u8],
    type_name: &str,
    decode: impl FnOnce(&[u8]) -> f64,
) -> Result<SqlValue, ProxyError> {
    let expected_length = if type_name == "float4" { 4 } else { 8 };
    if value.len() != expected_length {
        return Err(invalid_input(format!(
            "invalid binary {type_name} parameter length"
        )));
    }
    finite_float(decode(value))
}

fn finite_float(value: f64) -> Result<SqlValue, ProxyError> {
    if value.is_finite() {
        Ok(SqlValue::FloatingPoint(value))
    } else {
        Err(invalid_input(
            "non-finite floating-point parameters are not supported",
        ))
    }
}

fn unsupported_type(parameter_type: &Type) -> ProxyError {
    ProxyError::new(
        ErrorKind::FeatureNotSupported,
        format!(
            "prepared-statement parameter type '{}' (OID {}) is not supported",
            parameter_type.name(),
            parameter_type.oid()
        ),
    )
}

fn invalid_input(message: impl Into<String>) -> ProxyError {
    ProxyError::new(ErrorKind::InvalidInput, message)
}

#[cfg(test)]
mod tests {
    use super::{decode_parameter, parameter_format};
    use mongo_pg_common::ErrorKind;
    use mongo_pg_sql_engine::SqlValue;
    use pgwire::{
        api::{Type, portal::Format, results::FieldFormat},
        messages::data::FORMAT_CODE_BINARY,
    };

    #[test]
    fn decodes_supported_text_and_binary_values_losslessly() {
        assert_eq!(
            decode_parameter(Some(b"-42"), &Type::INT8, FieldFormat::Text)
                .expect("int8 should decode"),
            SqlValue::Integer(-42)
        );
        assert_eq!(
            decode_parameter(
                Some(&42_i32.to_be_bytes()),
                &Type::INT4,
                FieldFormat::Binary,
            )
            .expect("binary int4 should decode"),
            SqlValue::Integer(42)
        );
        assert_eq!(
            decode_parameter(Some(b"Harare"), &Type::VARCHAR, FieldFormat::Text)
                .expect("varchar should decode"),
            SqlValue::String("Harare".to_owned())
        );
    }

    #[test]
    fn rejects_unknown_types_and_invalid_encodings() {
        let unknown = decode_parameter(Some(b"value"), &Type::UNKNOWN, FieldFormat::Text)
            .expect_err("unknown OID is unsafe");
        assert_eq!(unknown.kind, ErrorKind::FeatureNotSupported);

        let malformed = decode_parameter(Some(&[1]), &Type::INT4, FieldFormat::Binary)
            .expect_err("binary int length must be exact");
        assert_eq!(malformed.kind, ErrorKind::InvalidInput);
    }

    #[test]
    fn parameter_format_rejects_truncated_per_parameter_format_lists() {
        let format = Format::Individual(vec![FORMAT_CODE_BINARY]);
        assert_eq!(
            parameter_format(&format, 0).expect("first format exists"),
            FieldFormat::Binary
        );
        assert_eq!(
            parameter_format(&format, 1)
                .expect_err("second format must not be invented")
                .kind,
            ErrorKind::InvalidInput
        );
    }
}
