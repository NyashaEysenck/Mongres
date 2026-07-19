//! `PostgreSQL` catalog and information-schema emulation.
//!
//! The projection in this crate deliberately does not claim a more specific
//! SQL type than schema discovery can prove. A client can use the resulting
//! metadata for table and column discovery, while ambiguous source fields stay
//! visibly marked as such for the write-safety boundary.

use std::collections::{BTreeMap, BTreeSet};

use mongo_pg_schema_discovery::{FieldPath, FieldProfile, ObservedType, SchemaProfile};

/// Name of the `PostgreSQL` information schema namespace.
pub const INFORMATION_SCHEMA: &str = "information_schema";

/// Default schema used when exposing a `MongoDB` collection as a SQL table.
pub const DEFAULT_SCHEMA: &str = "public";

/// A conservative `PostgreSQL` type suitable for catalog output and row
/// encoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SqlType {
    Boolean,
    BigInt,
    DoublePrecision,
    Text,
    TimestampWithTimeZone,
    Jsonb,
}

impl SqlType {
    /// Returns the SQL spelling presented by `information_schema.columns`.
    #[must_use]
    pub const fn information_schema_name(self) -> &'static str {
        match self {
            Self::Boolean => "boolean",
            Self::BigInt => "bigint",
            Self::DoublePrecision => "double precision",
            Self::Text => "text",
            Self::TimestampWithTimeZone => "timestamp with time zone",
            Self::Jsonb => "jsonb",
        }
    }

    /// Returns the native `PostgreSQL` type name used by catalog-aware clients.
    #[must_use]
    pub const fn udt_name(self) -> &'static str {
        match self {
            Self::Boolean => "bool",
            Self::BigInt => "int8",
            Self::DoublePrecision => "float8",
            Self::Text => "text",
            Self::TimestampWithTimeZone => "timestamptz",
            Self::Jsonb => "jsonb",
        }
    }

    /// Returns the stable built-in `PostgreSQL` type OID.
    #[must_use]
    pub const fn oid(self) -> u32 {
        match self {
            Self::Boolean => 16,
            Self::BigInt => 20,
            Self::DoublePrecision => 701,
            Self::Text => 25,
            Self::TimestampWithTimeZone => 1184,
            Self::Jsonb => 3802,
        }
    }
}

/// Metadata for a SQL table backed by one `MongoDB` collection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableMetadata {
    pub schema_name: String,
    pub table_name: String,
}

/// Metadata for one visible SQL column.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnMetadata {
    pub table: TableMetadata,
    /// The SQL-facing field name. Nested `MongoDB` paths use dot notation.
    pub column_name: String,
    /// One-based, deterministic position within the collection projection.
    pub ordinal_position: u32,
    /// True if the sampled field was absent or `null` in any document.
    pub is_nullable: bool,
    pub sql_type: SqlType,
    /// True if discovery cannot identify one safe field shape or type.
    pub is_ambiguous: bool,
    /// Segment-preserving paths represented by this visible column. Usually
    /// this contains exactly one path; a dotted-key collision has more.
    pub source_paths: Vec<FieldPath>,
}

/// The catalog information derived from one collection profile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollectionCatalog {
    pub table: TableMetadata,
    pub columns: Vec<ColumnMetadata>,
}

/// Projects a collection profile into table and column metadata.
///
/// Literal dotted keys and true nested paths can have the same SQL-facing
/// spelling. They intentionally become one `JSONB`, ambiguous column instead
/// of two indistinguishable catalog columns.
#[must_use]
pub fn project_collection(
    schema_name: impl Into<String>,
    table_name: impl Into<String>,
    profile: &SchemaProfile,
) -> CollectionCatalog {
    let table = TableMetadata {
        schema_name: schema_name.into(),
        table_name: table_name.into(),
    };
    let mut fields_by_display_name = BTreeMap::<String, Vec<&FieldProfile>>::new();

    for field in &profile.fields {
        fields_by_display_name
            .entry(field.path.display_name())
            .or_default()
            .push(field);
    }

    let columns = fields_by_display_name
        .into_iter()
        .enumerate()
        .map(|(index, (column_name, fields))| {
            column_from_fields(&table, column_name, index, fields)
        })
        .collect();

    CollectionCatalog { table, columns }
}

/// Projects a collection into the default `public` schema.
#[must_use]
pub fn project_public_collection(
    table_name: impl Into<String>,
    profile: &SchemaProfile,
) -> CollectionCatalog {
    project_collection(DEFAULT_SCHEMA, table_name, profile)
}

fn column_from_fields(
    table: &TableMetadata,
    column_name: String,
    index: usize,
    fields: Vec<&FieldProfile>,
) -> ColumnMetadata {
    let observed_types = fields
        .iter()
        .flat_map(|field| field.observed_types.iter().copied())
        .collect::<BTreeSet<_>>();
    let is_ambiguous = fields.len() > 1 || fields.iter().any(|field| field.is_ambiguous());
    let is_nullable = fields.iter().any(|field| {
        field.missing_documents > 0 || field.observed_types.contains(&ObservedType::Null)
    });

    ColumnMetadata {
        table: table.clone(),
        column_name,
        ordinal_position: u32::try_from(index + 1).expect("catalog column count exceeds u32"),
        is_nullable,
        sql_type: if is_ambiguous {
            SqlType::Jsonb
        } else {
            sql_type_for_observations(&observed_types)
        },
        is_ambiguous,
        source_paths: fields.into_iter().map(|field| field.path.clone()).collect(),
    }
}

fn sql_type_for_observations(observed_types: &BTreeSet<ObservedType>) -> SqlType {
    match observed_types.first().copied() {
        Some(ObservedType::Boolean) => SqlType::Boolean,
        Some(ObservedType::Integer) => SqlType::BigInt,
        Some(ObservedType::FloatingPoint) => SqlType::DoublePrecision,
        Some(ObservedType::String | ObservedType::ObjectId) => SqlType::Text,
        Some(ObservedType::DateTime) => SqlType::TimestampWithTimeZone,
        // BSON documents, arrays, and a field containing only null values do
        // not support a safer scalar claim.
        Some(ObservedType::Document | ObservedType::Array | ObservedType::Null) | None => {
            SqlType::Jsonb
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use mongo_pg_schema_discovery::{SampleDocument, SampleValue, SchemaProfile};

    use super::{DEFAULT_SCHEMA, SqlType, project_collection, project_public_collection};

    fn document(fields: impl IntoIterator<Item = (&'static str, SampleValue)>) -> SampleDocument {
        fields
            .into_iter()
            .map(|(name, value)| (name.to_owned(), value))
            .collect::<BTreeMap<_, _>>()
    }

    #[test]
    fn projects_nested_fields_with_conservative_sql_types_and_nullability() {
        let profile = SchemaProfile::infer(&[
            document([
                ("active", SampleValue::Boolean(true)),
                ("id", SampleValue::Integer(42)),
                (
                    "profile",
                    SampleValue::Document(document([(
                        "city",
                        SampleValue::String("Harare".into()),
                    )])),
                ),
            ]),
            document([
                ("active", SampleValue::Boolean(false)),
                (
                    "profile",
                    SampleValue::Document(document([(
                        "city",
                        SampleValue::String("Mutare".into()),
                    )])),
                ),
            ]),
        ]);

        let catalog = project_public_collection("customers", &profile);
        assert_eq!(catalog.table.schema_name, DEFAULT_SCHEMA);
        assert_eq!(catalog.table.table_name, "customers");
        assert_eq!(catalog.columns.len(), 4);

        let active = &catalog.columns[0];
        assert_eq!(active.column_name, "active");
        assert_eq!(active.ordinal_position, 1);
        assert_eq!(active.sql_type, SqlType::Boolean);
        assert!(!active.is_nullable);

        let id = &catalog.columns[1];
        assert_eq!(id.sql_type, SqlType::BigInt);
        assert!(id.is_nullable);

        let profile_document = &catalog.columns[2];
        assert_eq!(profile_document.sql_type, SqlType::Jsonb);
        let city = &catalog.columns[3];
        assert_eq!(city.column_name, "profile.city");
        assert_eq!(city.sql_type, SqlType::Text);
    }

    #[test]
    fn maps_mixed_types_to_ambiguous_jsonb() {
        let profile = SchemaProfile::infer(&[
            document([("status", SampleValue::String("active".into()))]),
            document([("status", SampleValue::Integer(1))]),
        ]);

        let column = &project_public_collection("customers", &profile).columns[0];
        assert_eq!(column.sql_type, SqlType::Jsonb);
        assert!(column.is_ambiguous);
    }

    #[test]
    fn folds_dotted_key_collisions_into_one_visible_ambiguous_column() {
        let profile = SchemaProfile::infer(&[document([
            ("profile.city", SampleValue::String("literal".into())),
            (
                "profile",
                SampleValue::Document(document([("city", SampleValue::String("nested".into()))])),
            ),
        ])]);

        let catalog = project_collection("mongo", "customers", &profile);
        let city_columns = catalog
            .columns
            .iter()
            .filter(|column| column.column_name == "profile.city")
            .collect::<Vec<_>>();
        assert_eq!(city_columns.len(), 1);
        assert_eq!(city_columns[0].table.schema_name, "mongo");
        assert_eq!(city_columns[0].sql_type, SqlType::Jsonb);
        assert!(city_columns[0].is_ambiguous);
        assert_eq!(city_columns[0].source_paths.len(), 2);
    }

    #[test]
    fn exposes_postgres_type_details_for_wire_and_information_schema_consumers() {
        assert_eq!(
            SqlType::TimestampWithTimeZone.information_schema_name(),
            "timestamp with time zone"
        );
        assert_eq!(SqlType::TimestampWithTimeZone.udt_name(), "timestamptz");
        assert_eq!(SqlType::Jsonb.oid(), 3802);
    }
}
