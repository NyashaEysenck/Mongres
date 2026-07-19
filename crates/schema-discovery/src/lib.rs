//! Pure schema inference for sampled `MongoDB` documents.
//!
//! The `MongoDB` driver adapter belongs at the outer edge of this crate. Keeping
//! inference independent of the driver makes its shape and ambiguity rules
//! deterministic and easy to test.

use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;

use futures_util::TryStreamExt;
use mongodb::{
    Client, Database,
    bson::{Bson, Document, doc, to_document},
};
use serde::{Deserialize, Serialize};

/// Name of the `MongoDB` collection used to retain inferred schema profiles.
pub const METADATA_COLLECTION: &str = "__pgproxy_schema";

/// Result type returned by the `MongoDB` integration boundary.
pub type DiscoveryResult<T> = Result<T, Box<dyn Error + Send + Sync>>;

/// A sampled document represented without a database-driver dependency.
pub type SampleDocument = BTreeMap<String, SampleValue>;

/// Values understood by the inference engine.
#[derive(Debug, Clone, PartialEq)]
pub enum SampleValue {
    Null,
    Boolean(bool),
    Integer(i64),
    FloatingPoint(f64),
    String(String),
    DateTime(String),
    ObjectId(String),
    Document(SampleDocument),
    Array(Vec<SampleValue>),
}

/// A BSON-compatible scalar or container type observed for a field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum ObservedType {
    Null,
    Boolean,
    Integer,
    FloatingPoint,
    String,
    DateTime,
    ObjectId,
    Document,
    Array,
}

/// The high-level field shapes that influence safe path and write decisions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum ObservedShape {
    Scalar,
    Document,
    Array,
}

/// A path represented as segments so literal dotted keys are never conflated
/// with nested document paths internally.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct FieldPath(Vec<String>);

impl FieldPath {
    /// Creates a path containing one top-level field name.
    #[must_use]
    pub fn top_level(field: impl Into<String>) -> Self {
        Self(vec![field.into()])
    }

    /// Returns a path with one nested field appended.
    #[must_use]
    pub fn child(&self, field: impl Into<String>) -> Self {
        let mut segments = self.0.clone();
        segments.push(field.into());
        Self(segments)
    }

    /// Returns the dot-separated display form used by SQL-facing surfaces.
    #[must_use]
    pub fn display_name(&self) -> String {
        self.0.join(".")
    }

    /// Returns the segment-preserving representation used for safe nesting.
    #[must_use]
    pub fn segments(&self) -> &[String] {
        &self.0
    }

    /// Returns whether the path is a literal top-level key containing a dot.
    #[must_use]
    pub fn is_literal_dotted_key(&self) -> bool {
        self.0.len() == 1 && self.0[0].contains('.')
    }
}

/// Inferred information for one field path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FieldProfile {
    pub path: FieldPath,
    pub present_documents: usize,
    pub missing_documents: usize,
    pub observed_types: BTreeSet<ObservedType>,
    pub observed_shapes: BTreeSet<ObservedShape>,
    pub has_dotted_key_collision: bool,
}

impl FieldProfile {
    /// Returns whether the profile has a type or shape that needs policy review.
    #[must_use]
    pub fn is_ambiguous(&self) -> bool {
        self.observed_types.len() > 1
            || self.observed_shapes.len() > 1
            || self.has_dotted_key_collision
    }
}

/// The versionable schema profile produced from one collection sample.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SchemaProfile {
    /// Format version for backward-compatible persistence and migration.
    pub profile_version: u32,
    pub sampled_documents: usize,
    pub fields: Vec<FieldProfile>,
}

impl SchemaProfile {
    /// Infers a deterministic profile from sampled documents.
    #[must_use]
    pub fn infer(documents: &[SampleDocument]) -> Self {
        let mut observations = BTreeMap::<FieldPath, MutableFieldProfile>::new();

        for document in documents {
            for (field, value) in document {
                observe_value(&mut observations, &FieldPath::top_level(field), value);
            }
        }

        let fields = observations
            .into_iter()
            .map(|(path, observation)| FieldProfile {
                has_dotted_key_collision: false,
                path,
                present_documents: observation.present_documents,
                missing_documents: documents.len() - observation.present_documents,
                observed_types: observation.observed_types,
                observed_shapes: observation.observed_shapes,
            })
            .collect();

        let mut profile = Self {
            profile_version: 1,
            sampled_documents: documents.len(),
            fields,
        };
        profile.mark_dotted_key_collisions();
        profile
    }

    /// Returns the profile for an exact, segment-preserving field path.
    #[must_use]
    pub fn field(&self, path: &FieldPath) -> Option<&FieldProfile> {
        self.fields.iter().find(|field| field.path == *path)
    }

    fn mark_dotted_key_collisions(&mut self) {
        let display_name_counts = self
            .fields
            .iter()
            .map(|field| field.path.display_name())
            .fold(BTreeMap::<String, usize>::new(), |mut counts, name| {
                *counts.entry(name).or_default() += 1;
                counts
            });

        for field in &mut self.fields {
            field.has_dotted_key_collision = display_name_counts
                .get(&field.path.display_name())
                .is_some_and(|count| *count > 1);
        }
    }
}

/// Samples a collection and infers its profile without persisting it.
///
/// # Errors
///
/// Returns an error when the sample size is zero or `MongoDB` cannot be queried.
pub async fn discover_collection(
    database: &Database,
    collection_name: &str,
    sample_size: usize,
) -> DiscoveryResult<SchemaProfile> {
    if sample_size == 0 {
        return Err("schema discovery sample size must be greater than zero".into());
    }

    let collection = database.collection::<Document>(collection_name);
    let mut cursor = collection.find(doc! {}).await?;
    let mut documents = Vec::with_capacity(sample_size);

    while let Some(document) = cursor.try_next().await? {
        documents.push(sample_document_from_bson(&document));
        if documents.len() == sample_size {
            break;
        }
    }

    Ok(SchemaProfile::infer(&documents))
}

/// Discovers and upserts a versioned schema profile for one source collection.
///
/// # Errors
///
/// Returns an error when sampling, serializing, or writing the profile fails.
pub async fn discover_and_persist_collection(
    client: &Client,
    database_name: &str,
    collection_name: &str,
    sample_size: usize,
) -> DiscoveryResult<SchemaProfile> {
    let database = client.database(database_name);
    let profile = discover_collection(&database, collection_name, sample_size).await?;
    persist_profile(&database, collection_name, &profile).await?;
    Ok(profile)
}

/// Upserts one profile per source collection in the metadata collection.
///
/// # Errors
///
/// Returns an error when the profile cannot be serialized or persisted.
pub async fn persist_profile(
    database: &Database,
    source_collection: &str,
    profile: &SchemaProfile,
) -> DiscoveryResult<()> {
    let metadata = database.collection::<Document>(METADATA_COLLECTION);
    let record = to_document(&StoredSchemaProfile {
        database: database.name().to_owned(),
        collection: source_collection.to_owned(),
        profile: profile.clone(),
    })?;

    metadata
        .replace_one(
            doc! {
                "database": database.name(),
                "collection": source_collection,
            },
            record,
        )
        .upsert(true)
        .await?;

    Ok(())
}

/// Converts a driver document into the pure input model used by inference.
#[must_use]
pub fn sample_document_from_bson(document: &Document) -> SampleDocument {
    document
        .iter()
        .map(|(key, value)| (key.clone(), sample_value_from_bson(value)))
        .collect()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredSchemaProfile {
    database: String,
    collection: String,
    profile: SchemaProfile,
}

#[derive(Debug, Default)]
struct MutableFieldProfile {
    present_documents: usize,
    observed_types: BTreeSet<ObservedType>,
    observed_shapes: BTreeSet<ObservedShape>,
}

fn observe_value(
    observations: &mut BTreeMap<FieldPath, MutableFieldProfile>,
    path: &FieldPath,
    value: &SampleValue,
) {
    let entry = observations.entry(path.clone()).or_default();
    entry.present_documents += 1;
    entry.observed_types.insert(value.observed_type());
    entry.observed_shapes.insert(value.observed_shape());

    if let SampleValue::Document(document) = value {
        for (field, nested_value) in document {
            observe_value(observations, &path.child(field), nested_value);
        }
    }
}

impl SampleValue {
    fn observed_type(&self) -> ObservedType {
        match self {
            Self::Null => ObservedType::Null,
            Self::Boolean(_) => ObservedType::Boolean,
            Self::Integer(_) => ObservedType::Integer,
            Self::FloatingPoint(_) => ObservedType::FloatingPoint,
            Self::String(_) => ObservedType::String,
            Self::DateTime(_) => ObservedType::DateTime,
            Self::ObjectId(_) => ObservedType::ObjectId,
            Self::Document(_) => ObservedType::Document,
            Self::Array(_) => ObservedType::Array,
        }
    }

    fn observed_shape(&self) -> ObservedShape {
        match self {
            Self::Document(_) => ObservedShape::Document,
            Self::Array(_) => ObservedShape::Array,
            Self::Null
            | Self::Boolean(_)
            | Self::Integer(_)
            | Self::FloatingPoint(_)
            | Self::String(_)
            | Self::DateTime(_)
            | Self::ObjectId(_) => ObservedShape::Scalar,
        }
    }
}

fn sample_value_from_bson(value: &Bson) -> SampleValue {
    match value {
        Bson::Null => SampleValue::Null,
        Bson::Boolean(value) => SampleValue::Boolean(*value),
        Bson::Int32(value) => SampleValue::Integer(i64::from(*value)),
        Bson::Int64(value) => SampleValue::Integer(*value),
        Bson::Double(value) => SampleValue::FloatingPoint(*value),
        Bson::String(value) => SampleValue::String(value.clone()),
        Bson::DateTime(value) => SampleValue::DateTime(value.to_string()),
        Bson::ObjectId(value) => SampleValue::ObjectId(value.to_string()),
        Bson::Document(value) => SampleValue::Document(sample_document_from_bson(value)),
        Bson::Array(values) => {
            SampleValue::Array(values.iter().map(sample_value_from_bson).collect())
        }
        _ => SampleValue::String(value.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        FieldPath, ObservedShape, ObservedType, SampleDocument, SampleValue, SchemaProfile,
        sample_document_from_bson,
    };
    use mongodb::bson::doc;

    fn document(fields: impl IntoIterator<Item = (&'static str, SampleValue)>) -> SampleDocument {
        fields
            .into_iter()
            .map(|(key, value)| (key.to_owned(), value))
            .collect()
    }

    #[test]
    fn infers_nested_fields_and_missing_document_counts() {
        let profile = SchemaProfile::infer(&[
            document([(
                "profile",
                SampleValue::Document(document([("city", SampleValue::String("Harare".into()))])),
            )]),
            document([("profile", SampleValue::Document(document([])))]),
            document([]),
        ]);

        let city = profile
            .field(&FieldPath::top_level("profile").child("city"))
            .expect("nested city profile should exist");
        assert_eq!(city.present_documents, 1);
        assert_eq!(city.missing_documents, 2);
        assert_eq!(city.observed_types, [ObservedType::String].into());
        assert!(!city.is_ambiguous());
    }

    #[test]
    fn flags_mixed_types_and_shapes_as_ambiguous() {
        let profile = SchemaProfile::infer(&[
            document([("status", SampleValue::String("active".into()))]),
            document([("status", SampleValue::Integer(1))]),
            document([("status", SampleValue::Array(vec![]))]),
        ]);

        let status = profile
            .field(&FieldPath::top_level("status"))
            .expect("status profile should exist");
        assert_eq!(status.observed_types.len(), 3);
        assert!(status.observed_shapes.contains(&ObservedShape::Scalar));
        assert!(status.observed_shapes.contains(&ObservedShape::Array));
        assert!(status.is_ambiguous());
    }

    #[test]
    fn records_arrays_without_inventing_nested_array_paths() {
        let profile = SchemaProfile::infer(&[document([(
            "tags",
            SampleValue::Array(vec![SampleValue::String("database".into())]),
        )])]);

        let tags = profile
            .field(&FieldPath::top_level("tags"))
            .expect("tags profile should exist");
        assert_eq!(tags.observed_types, [ObservedType::Array].into());
        assert_eq!(tags.observed_shapes, [ObservedShape::Array].into());
        assert!(
            profile
                .field(&FieldPath::top_level("tags").child("0"))
                .is_none()
        );
    }

    #[test]
    fn distinguishes_a_literal_dotted_key_from_a_nested_path() {
        let profile = SchemaProfile::infer(&[document([
            ("profile.city", SampleValue::String("literal".into())),
            (
                "profile",
                SampleValue::Document(document([("city", SampleValue::String("nested".into()))])),
            ),
        ])]);

        let literal = profile
            .field(&FieldPath::top_level("profile.city"))
            .expect("literal dotted key profile should exist");
        let nested = profile
            .field(&FieldPath::top_level("profile").child("city"))
            .expect("nested path profile should exist");
        assert!(literal.has_dotted_key_collision);
        assert!(nested.has_dotted_key_collision);
        assert!(literal.is_ambiguous());
        assert!(nested.is_ambiguous());
    }

    #[test]
    fn converts_nested_driver_documents_before_inference() {
        let document = doc! {
            "active": true,
            "profile": { "city": "Harare" },
        };

        let sample = sample_document_from_bson(&document);
        let profile = SchemaProfile::infer(&[sample]);
        assert!(profile.field(&FieldPath::top_level("active")).is_some());
        assert!(
            profile
                .field(&FieldPath::top_level("profile").child("city"))
                .is_some()
        );
    }
}
